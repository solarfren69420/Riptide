//! H2Overdrive `triton.lux` archive.
//!
//! Layout (little endian):
//! - `0x00` u32 version (3), `0x04` u32 entry count, `0x08` u32 TOC offset.
//! - `0x30` `"LUX!"` + u32, then the entry blobs back to back, uncompressed.
//! - TOC: `count` records of 0x60 bytes, sorted by name hash:
//!   `+0x00` name (NUL terminated, 0x20 bytes), `+0x40` u64 size, `+0x48` u32 hash,
//!   `+0x50` u64 FILETIME.
//!
//! The TOC carries no offsets: blobs are stored in FILETIME order starting at `0x38`.

use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct LuxEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

pub struct LuxArchive {
    map: Mmap,
    entries: Vec<LuxEntry>,
    by_name: HashMap<String, usize>,
}

const TOC_RECORD: usize = 0x60;
const DATA_START: u64 = 0x38;

impl LuxArchive {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        // SAFETY: the archive is read-only game data that nothing rewrites while we run.
        let map = unsafe { Mmap::map(&file)? };
        if map.len() < 0x40 || &map[0x30..0x34] != b"LUX!" {
            bail!("{} is not a LUX archive", path.display());
        }
        let count = u32_at(&map, 4) as usize;
        let toc = u32_at(&map, 8) as usize;
        if toc + count * TOC_RECORD > map.len() {
            bail!("LUX TOC out of range");
        }
        let mut raw: Vec<(u64, String, u64)> = (0..count)
            .map(|i| {
                let r = &map[toc + i * TOC_RECORD..toc + (i + 1) * TOC_RECORD];
                let name = cstr(&r[..0x20]);
                let size = u64::from_le_bytes(r[0x40..0x48].try_into().unwrap());
                let filetime = u64::from_le_bytes(r[0x50..0x58].try_into().unwrap());
                (filetime, name, size)
            })
            .collect();
        raw.sort_by_key(|(ft, _, _)| *ft);
        let mut offset = DATA_START;
        let mut entries = Vec::with_capacity(count);
        for (_, name, size) in raw {
            entries.push(LuxEntry { name, offset, size });
            offset += size;
        }
        if offset > toc as u64 {
            bail!("LUX blobs overrun the TOC ({offset:#x} > {toc:#x})");
        }
        let by_name = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.to_ascii_lowercase(), i))
            .collect();
        Ok(Self { map, entries, by_name })
    }

    pub fn entries(&self) -> &[LuxEntry] {
        &self.entries
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let e = &self.entries[*self.by_name.get(&name.to_ascii_lowercase())?];
        Some(&self.map[e.offset as usize..(e.offset + e.size) as usize])
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(&name.to_ascii_lowercase())
    }

    /// Names (without the `kind.` prefix) of every entry of one kind, e.g. `"mesh32"`.
    pub fn names_of_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries.iter().filter_map(move |e| {
            let (k, rest) = e.name.split_once('.')?;
            (k == kind).then_some(rest)
        })
    }
}

pub(crate) fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub(crate) fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

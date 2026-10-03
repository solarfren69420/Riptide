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

use crate::source::Source;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct LuxEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

pub struct LuxArchive {
    src: Box<dyn Source>,
    entries: Vec<LuxEntry>,
    by_name: HashMap<String, usize>,
}

const TOC_RECORD: usize = 0x60;
const DATA_START: u64 = 0x38;

impl LuxArchive {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::from_source(Box::new(crate::source::MapSource::open(path)?)).with_context(|| path.display().to_string())
    }

    /// Byte ranges the header and table of contents live in: `(0, 0x40)`, then (once the
    /// header is present) the TOC. A sparse source needs these before [`Self::from_source`].
    pub fn index_ranges(src: &dyn Source) -> Vec<(u64, u64)> {
        let mut out = vec![(0, 0x40)];
        if let Some(h) = src.read(0, 0x40) {
            let (count, toc) = (u32_at(h, 4) as u64, u32_at(h, 8) as u64);
            out.push((toc, toc + count * TOC_RECORD as u64));
        }
        out
    }

    pub fn from_source(src: Box<dyn Source>) -> Result<Self> {
        let head = src.read(0, 0x40).context("LUX header not available")?;
        if &head[0x30..0x34] != b"LUX!" {
            bail!("not a LUX archive");
        }
        let count = u32_at(head, 4) as usize;
        let toc = u32_at(head, 8) as usize;
        let table = src.read(toc as u64, count * TOC_RECORD).context("LUX TOC out of range or not available")?;
        let mut raw: Vec<(u64, String, u64)> = (0..count)
            .map(|i| {
                let r = &table[i * TOC_RECORD..(i + 1) * TOC_RECORD];
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
        Ok(Self { src, entries, by_name })
    }

    pub fn entries(&self) -> &[LuxEntry] {
        &self.entries
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let e = self.entry(name)?;
        if let Some(r) = RECORD.lock().unwrap().as_mut() {
            r.insert(e.name.clone());
        }
        self.src.read(e.offset, e.size as usize)
    }

    pub fn entry(&self, name: &str) -> Option<&LuxEntry> {
        Some(&self.entries[*self.by_name.get(&name.to_ascii_lowercase())?])
    }

    /// Some reads failed only because their bytes aren't fetched yet (browser build): a
    /// `None` from [`Self::get`] is then "not yet", not "doesn't exist".
    pub fn pending(&self) -> bool {
        self.src.pending()
    }

    /// The underlying source (to fetch ranges into a sparse one).
    pub fn source(&self) -> &dyn Source {
        self.src.as_ref()
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

pub fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Entry names read through [`LuxArchive::get`] while recording (see [`record`]).
static RECORD: Mutex<Option<BTreeSet<String>>> = Mutex::new(None);

/// Start (or restart) recording which entries are read: the per-course lists the web build
/// fetches are made this way.
pub fn record() {
    *RECORD.lock().unwrap() = Some(BTreeSet::new());
}

/// Entries read since [`record`], and stop recording.
pub fn take_recorded() -> Vec<String> {
    RECORD.lock().unwrap().take().map(|s| s.into_iter().collect()).unwrap_or_default()
}

/// Entries read since [`record`], recording continuing.
pub fn recorded() -> Vec<String> {
    RECORD.lock().unwrap().as_ref().map(|s| s.iter().cloned().collect()).unwrap_or_default()
}

//! Hydro Thunder (Dreamcast) `.R2` resource archives, header `"James Cameron Rules"`.
//!
//! Header: `+0x14` header size (0x88), `+0x20` directory offset, `+0x2c` entry count.
//! Directory entries are 0x4c bytes: `{u32 offset, u32 size, u32 relocations, char name[12]}`.
//! The first letter of a name is its kind (`T`/`M` texture, `G` geometry, `H` track, `A` anim).
//!
//! An entry is `relocations` records of 16 bytes (`char name[12]`, `u32 slot`), then a u32 tag
//! (`size | kind << 24`) and `size` bytes of body. Slots and internal pointer values are relative
//! to the body (just past the tag); a named record points the slot at another entry.

use crate::lux::{cstr, u32_at};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct R2Entry {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub relocations: usize,
}

pub struct R2Archive {
    data: Vec<u8>,
    entries: Vec<R2Entry>,
    by_name: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct R2Object<'a> {
    pub name: String,
    pub kind: u8,
    pub body: &'a [u8],
    /// Body offsets of internal pointers.
    pub internal: Vec<usize>,
    /// Body offset -> referenced entry name.
    pub external: Vec<(usize, String)>,
}

impl R2Object<'_> {
    pub fn u32(&self, o: usize) -> Option<u32> {
        (o + 4 <= self.body.len()).then(|| u32_at(self.body, o))
    }
    pub fn f32(&self, o: usize) -> Option<f32> {
        self.u32(o).map(f32::from_bits)
    }
    pub fn u16(&self, o: usize) -> Option<u16> {
        (o + 2 <= self.body.len()).then(|| u16::from_le_bytes([self.body[o], self.body[o + 1]]))
    }
    /// Follow an internal pointer stored at `o`.
    pub fn ptr(&self, o: usize) -> Option<usize> {
        let v = self.u32(o)? as usize;
        (v < self.body.len()).then_some(v)
    }
    pub fn external_at(&self, o: usize) -> Option<&str> {
        self.external.iter().find(|(a, _)| *a == o).map(|(_, n)| n.as_str())
    }
}

impl R2Archive {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        if !data.starts_with(b"James Cameron Rules") {
            bail!("not an R2 archive");
        }
        let dir = u32_at(&data, 0x20) as usize;
        let count = u32_at(&data, 0x2c) as usize;
        if dir + count * 0x4c > data.len() {
            bail!("R2 directory out of range");
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let e = &data[dir + i * 0x4c..dir + (i + 1) * 0x4c];
            entries.push(R2Entry {
                offset: u32_at(e, 0) as usize,
                size: u32_at(e, 4) as usize,
                relocations: u32_at(e, 8) as usize,
                name: cstr(&e[12..24]),
            });
        }
        let by_name = entries.iter().enumerate().map(|(i, e)| (e.name.to_ascii_uppercase(), i)).collect();
        Ok(Self { data, entries, by_name })
    }

    pub fn entries(&self) -> &[R2Entry] {
        &self.entries
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(&name.to_ascii_uppercase())
    }

    pub fn object(&self, name: &str) -> Result<R2Object<'_>> {
        let e = &self.entries[*self.by_name.get(&name.to_ascii_uppercase()).with_context(|| format!("{name} not in archive"))?];
        let rel_end = e.offset + e.relocations * 16;
        let body_start = rel_end + 4;
        let body_end = (body_start + e.size).min(self.data.len());
        if body_start > self.data.len() {
            bail!("{name}: entry out of range");
        }
        let tag = u32_at(&self.data, rel_end);
        let mut internal = Vec::new();
        let mut external = Vec::new();
        for r in 0..e.relocations {
            let rec = &self.data[e.offset + r * 16..e.offset + r * 16 + 16];
            let slot = u32_at(rec, 12) as usize;
            let target = cstr(&rec[..12]);
            if target.is_empty() {
                internal.push(slot);
            } else {
                external.push((slot, target));
            }
        }
        Ok(R2Object {
            name: name.to_string(),
            kind: (tag >> 24) as u8,
            body: &self.data[body_start..body_end],
            internal,
            external,
        })
    }

    /// Raw entry bytes (relocations included), for texture entries.
    pub fn raw(&self, name: &str) -> Option<&[u8]> {
        let e = &self.entries[*self.by_name.get(&name.to_ascii_uppercase())?];
        let end = (e.offset + e.relocations * 16 + 4 + e.size).min(self.data.len());
        Some(&self.data[e.offset..end])
    }
}

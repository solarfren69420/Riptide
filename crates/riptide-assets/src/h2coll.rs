//! H2Overdrive `coll4.wc_<level>` world collision decoder.
//!
//! The blob is a serialized memory image like `mesh32.*` (tag `"COLL"`):
//! - `0x00` u32 4, u32 3, u32 fixup count, `"COLL"`, u32 4
//! - `0x14` fixups, 0x2c bytes each: `{u32 kind, u32 at, u32 target}` (body-relative); kind 1 is an
//!   internal pointer (`body[at] = target`). Body starts at `0x14 + nfix * 0x2c`.
//! - Body `+0x10`: u32 triangle count; body `+0x14`: pointer (fixup with `at == 0x14`) to the
//!   triangle array. Body `+0x30..` holds the spatial-tree nodes (not needed for triangles).
//! - Triangle record, 72 bytes, packed at the tail of the blob, index 0..count:
//!   `f32 v0[3], v1[3], v2[3]` world-space (game, left-handed Y up, not Z-mirrored),
//!   `f32 normal[3]`, `u32 0x83` (flags), `u32 0`, `u32 hash`, `u32 0`, `u32 index`, `u32 -1`.
//! - Empty levels (84-byte blobs) have zero fixups and no triangles.

use crate::lux::u32_at;
use anyhow::{bail, Result};

const REC: usize = 72;

pub fn decode_collision(blob: &[u8]) -> Result<Vec<[[f32; 3]; 3]>> {
    Ok(decode_collision_flags(blob)?.into_iter().map(|(t, _)| t).collect())
}

/// Triangles with their flags word (record `+0x30`).
pub fn decode_collision_flags(blob: &[u8]) -> Result<Vec<([[f32; 3]; 3], u32)>> {
    if blob.len() < 0x14 || &blob[0x0c..0x10] != b"COLL" {
        bail!("not a COLL blob");
    }
    let nfix = u32_at(blob, 8) as usize;
    if nfix == 0 {
        return Ok(Vec::new());
    }
    let body = 0x14 + nfix * 0x2c;
    if body + 0x18 > blob.len() {
        bail!("COLL header overruns blob");
    }
    let count = u32_at(blob, body + 0x10) as usize;
    let mut tri_at = None;
    for i in 0..nfix {
        let f = 0x14 + i * 0x2c;
        if u32_at(blob, f) == 1 && u32_at(blob, f + 4) == 0x14 {
            tri_at = Some(body + u32_at(blob, f + 8) as usize);
        }
    }
    let Some(start) = tri_at else { bail!("COLL: no triangle array pointer") };
    if start + count * REC != blob.len() {
        bail!("COLL: {count} triangles at {start:#x} do not end at blob end {:#x}", blob.len());
    }
    let f = |o: usize| f32::from_bits(u32_at(blob, o));
    Ok((0..count)
        .map(|i| {
            let o = start + i * REC;
            let v = |k: usize| [f(o + k * 12), f(o + k * 12 + 4), f(o + k * 12 + 8)];
            ([v(0), v(1), v(2)], u32_at(blob, o + 0x30))
        })
        .collect())
}

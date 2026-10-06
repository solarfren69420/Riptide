//! Hydro Thunder node animation clips (`A_<name>H1` objects in R2 archives), which move the
//! nodes (groups) of a `G_<name>H1` geometry: seagulls, parrots, bats.
//!
//! Header (16 B): u16 last frame, u16 part count, f32 seconds per frame, u32 (2), u32 track
//! offset of part 0. Then one descriptor per part, holding its frame-0 pose: part 0 is a bare pose
//! (27 f32), parts 1.. start with u32 node id and u32 track offset (29 words). A pose is 27 f32:
//! a 3x3 rotation-scale and a translation, the same pair inverted, then 1.0 1.0 0. A part's track
//! holds its poses for frames 1..=last at its offset, one after another.

use crate::r2::R2Object;
use anyhow::{bail, Result};

#[derive(Clone, Debug)]
pub struct HtTrack {
    /// Geometry group (node) id this part moves; `None` for part 0 (the object's first group).
    pub node: Option<u16>,
    /// Per frame: column-major 4x4 matrix (Riptide space, Z mirrored).
    pub poses: Vec<[f32; 16]>,
}

#[derive(Clone, Debug)]
pub struct HtClip {
    pub seconds_per_frame: f32,
    pub tracks: Vec<HtTrack>,
}

const POSE: usize = 27 * 4;

/// A pose record at `o` as a column-major 4x4, mirrored across Z (S M S, S = diag(1, 1, -1)).
fn pose(obj: &R2Object, o: usize) -> Option<[f32; 16]> {
    let f = |k: usize| obj.f32(o + k * 4);
    let r = [f(0)?, f(1)?, f(2)?, f(3)?, f(4)?, f(5)?, f(6)?, f(7)?, f(8)?];
    let t = [f(9)?, f(10)?, f(11)?];
    // Row-major 3x3 r (rows i, columns j); mirror: entries with exactly one Z index flip sign.
    let s = |i: usize, j: usize| if (i == 2) != (j == 2) { -1.0 } else { 1.0 };
    let m = |i: usize, j: usize| r[i * 3 + j] * s(i, j);
    Some([
        m(0, 0), m(1, 0), m(2, 0), 0.0,
        m(0, 1), m(1, 1), m(2, 1), 0.0,
        m(0, 2), m(1, 2), m(2, 2), 0.0,
        t[0], t[1], -t[2], 1.0,
    ])
}

pub fn decode_clip(obj: &R2Object) -> Result<HtClip> {
    let (Some(last), Some(nparts)) = (obj.u16(0), obj.u16(2)) else { bail!("{}: short clip", obj.name) };
    let (last, nparts) = (last as usize, nparts as usize);
    if nparts == 0 || nparts > 64 || last > 10_000 {
        bail!("{}: implausible clip ({nparts} parts, {last} frames)", obj.name);
    }
    let spf = obj.f32(4).filter(|v| *v > 0.0 && *v < 1.0).unwrap_or(1.0 / 30.0);
    let mut tracks = Vec::new();
    let mut at = 16;
    for p in 0..nparts {
        let (node, offset) = if p == 0 {
            (None, obj.u32(12).unwrap_or(0) as usize)
        } else {
            let n = obj.u32(at).unwrap_or(0) as u16;
            let off = obj.u32(at + 4).unwrap_or(0) as usize;
            at += 8;
            (Some(n), off)
        };
        let Some(first) = pose(obj, at) else { bail!("{}: part {p} pose", obj.name) };
        at += POSE;
        let mut poses = vec![first];
        for k in 0..last {
            match pose(obj, offset + k * POSE) {
                Some(m) => poses.push(m),
                None => break,
            }
        }
        tracks.push(HtTrack { node, poses });
    }
    Ok(HtClip { seconds_per_frame: spf, tracks })
}

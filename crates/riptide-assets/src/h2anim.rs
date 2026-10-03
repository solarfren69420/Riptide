//! H2Overdrive `anim4.*` clips: per-node keyframe tracks for boats' moving parts (boosters,
//! props, wings) and animated scenery.
//!
//! The blob is the same relocatable image as `mesh32` (header `u32 4, u32 3, u32 fixups,
//! "ANIM"`, 0x2c-byte fixups, then the body). Body:
//! - `+0x04` f32 clip length in seconds (keys run at 30 frames per second), `+0x08` track count,
//!   `[+0x0c]` -> tracks, 0x30 bytes each:
//!   `{name*, n pos, n rot, n scale, pos times*, rot times*, scale times*, pos*, rot*, scale*}`.
//! - Times are pairs `(t, 1 / (t_next - t))` with `t` a fraction of the clip (0..1).
//! - Positions and scales are 16-byte vectors (`w` unused), rotations quaternions `(x, y, z, w)`.
//! - Values are node-local, in Direct3D space (left-handed, Y up), like the mesh bones.

use crate::lux::{cstr, u32_at};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

/// Keys run at this rate: boat defs give `Anim Boost Partition Frame` in these frames.
pub const FPS: f32 = 30.0;

#[derive(Debug, Clone, Default)]
pub struct Track {
    pub name: String,
    /// `(t, value)` with `t` a fraction of the clip.
    pub pos: Vec<(f32, [f32; 3])>,
    pub rot: Vec<(f32, [f32; 4])>,
    pub scale: Vec<(f32, [f32; 3])>,
}

#[derive(Debug, Clone, Default)]
pub struct Clip {
    pub name: String,
    /// Seconds.
    pub duration: f32,
    pub tracks: Vec<Track>,
}

pub fn decode_anim(name: &str, blob: &[u8]) -> Result<Clip> {
    if blob.len() < 0x14 || &blob[0x0c..0x10] != b"ANIM" {
        bail!("{name}: not an ANIM blob");
    }
    let nfix = u32_at(blob, 8) as usize;
    let start = 0x14 + nfix * 0x2c;
    if start > blob.len() {
        bail!("{name}: fixup table overruns blob");
    }
    let mut ptrs: HashMap<usize, usize> = HashMap::new();
    for i in 0..nfix {
        let r = &blob[0x14 + i * 0x2c..0x14 + (i + 1) * 0x2c];
        if u32_at(r, 0) == 1 {
            ptrs.insert(u32_at(r, 4) as usize, u32_at(r, 8) as usize);
        }
    }
    let body = &blob[start..];
    let f = |o: usize| -> Option<f32> { body.get(o..o + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())) };
    let rd = |o: usize| -> Option<usize> { (o + 4 <= body.len()).then(|| u32_at(body, o) as usize) };
    let ptr = |o: usize| ptrs.get(&o).copied().filter(|&t| t < body.len());

    let duration = f(0x04).context("no clip length")?;
    let count = rd(0x08).context("no track count")?;
    let table = ptr(0x0c).context("no track table")?;
    if count > 1024 {
        bail!("{name}: {count} tracks");
    }
    let times = |at: Option<usize>, n: usize| -> Vec<f32> { at.map_or(Vec::new(), |a| (0..n).filter_map(|k| f(a + k * 8)).collect()) };
    let vec3 = |at: Option<usize>, n: usize| -> Vec<[f32; 3]> {
        at.map_or(Vec::new(), |a| (0..n).filter_map(|k| Some([f(a + k * 16)?, f(a + k * 16 + 4)?, f(a + k * 16 + 8)?])).collect())
    };
    let mut tracks = Vec::with_capacity(count);
    for i in 0..count {
        let t = table + i * 0x30;
        let name = ptr(t).and_then(|n| body.get(n..)).map(|b| cstr(&b[..b.len().min(0x40)])).unwrap_or_default();
        let (np, nr, ns) = (rd(t + 4).unwrap_or(0).min(4096), rd(t + 8).unwrap_or(0).min(4096), rd(t + 12).unwrap_or(0).min(4096));
        let pos_t = times(ptr(t + 0x10), np);
        let rot_t = times(ptr(t + 0x14), nr);
        let scale_t = times(ptr(t + 0x18), ns);
        let pos = vec3(ptr(t + 0x1c), np);
        let scale = vec3(ptr(t + 0x24), ns);
        let rot: Vec<[f32; 4]> = ptr(t + 0x20).map_or(Vec::new(), |a| {
            (0..nr).filter_map(|k| Some([f(a + k * 16)?, f(a + k * 16 + 4)?, f(a + k * 16 + 8)?, f(a + k * 16 + 12)?])).collect()
        });
        tracks.push(Track {
            name,
            pos: pos_t.into_iter().zip(pos).collect(),
            rot: rot_t.into_iter().zip(rot).collect(),
            scale: scale_t.into_iter().zip(scale).collect(),
        });
    }
    Ok(Clip { name: name.to_string(), duration, tracks })
}

/// The bracketing keys around clip fraction `t` and the blend between them.
pub fn keys_at<T: Copy>(keys: &[(f32, T)], t: f32) -> Option<(T, T, f32)> {
    let first = keys.first()?;
    if keys.len() == 1 || t <= first.0 {
        return Some((first.1, first.1, 0.0));
    }
    for w in keys.windows(2) {
        let ((t0, a), (t1, b)) = (w[0], w[1]);
        if t <= t1 {
            let span = (t1 - t0).max(1e-6);
            return Some((a, b, ((t - t0) / span).clamp(0.0, 1.0)));
        }
    }
    let last = keys.last()?;
    Some((last.1, last.1, 0.0))
}

type M4 = [f32; 16];

fn mul(a: &M4, b: &M4) -> M4 {
    std::array::from_fn(|k| (0..4).map(|i| a[i * 4 + k % 4] * b[(k / 4) * 4 + i]).sum())
}

/// Inverse of a rigid/affine column-major matrix (no projection).
fn affine_inverse(m: &M4) -> M4 {
    let a = [[m[0], m[4], m[8]], [m[1], m[5], m[9]], [m[2], m[6], m[10]]];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let d = if det.abs() < 1e-12 { 1.0 } else { 1.0 / det };
    let inv = [
        [(a[1][1] * a[2][2] - a[1][2] * a[2][1]) * d, (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * d, (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * d],
        [(a[1][2] * a[2][0] - a[1][0] * a[2][2]) * d, (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * d, (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * d],
        [(a[1][0] * a[2][1] - a[1][1] * a[2][0]) * d, (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * d, (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * d],
    ];
    let t = [m[12], m[13], m[14]];
    let it: [f32; 3] = std::array::from_fn(|r| -(inv[r][0] * t[0] + inv[r][1] * t[1] + inv[r][2] * t[2]));
    [inv[0][0], inv[1][0], inv[2][0], 0.0, inv[0][1], inv[1][1], inv[2][1], 0.0, inv[0][2], inv[1][2], inv[2][2], 0.0, it[0], it[1], it[2], 1.0]
}

/// Translation + rotation (unit quaternion x, y, z, w) as a column-major matrix.
fn trs(p: [f32; 3], q: [f32; 4]) -> M4 {
    let [x, y, z, w] = q;
    [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0,
        2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0,
        2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0,
        p[0], p[1], p[2], 1.0,
    ]
}

/// `model` (from `decode_rigged`) posed by `clip` at `t` seconds: every part moved by its bone's
/// animated model-space matrix. Bones the clip doesn't name keep their rest pose. Output space
/// (Z mirrored), so a clip key `(x, y, z)` / `(qx, qy, qz, qw)` enters as `(x, y, -z)` /
/// `(-qx, -qy, qz, qw)`.
pub fn pose(model: &crate::model::Model, clip: &Clip, t: f32) -> crate::model::Model {
    let f = (t / clip.duration.max(1e-3)).clamp(0.0, 1.0);
    let n = model.bones.len();
    let rest = |i: usize| model.bones[i].rest;
    let mut world: Vec<Option<M4>> = vec![None; n];
    for _ in 0..n {
        for i in 0..n {
            if world[i].is_some() {
                continue;
            }
            let parent_world = match model.bones[i].parent {
                Some(p) => match world[p] {
                    Some(w) => Some((w, rest(p))),
                    None => continue,
                },
                None => None,
            };
            let rest_local = match parent_world {
                Some((_, pr)) => mul(&affine_inverse(&pr), &rest(i)),
                None => rest(i),
            };
            let track = clip.tracks.iter().find(|tr| tr.name.eq_ignore_ascii_case(&model.bones[i].name));
            let local = match track {
                Some(tr) => {
                    let p = keys_at(&tr.pos, f).map_or([rest_local[12], rest_local[13], rest_local[14]], |(a, b, k)| {
                        let v: [f32; 3] = std::array::from_fn(|j| a[j] + (b[j] - a[j]) * k);
                        [v[0], v[1], -v[2]]
                    });
                    let q = keys_at(&tr.rot, f).map(|(a, b, k)| {
                        // nlerp (short way round), then into output space.
                        let s = if a.iter().zip(&b).map(|(x, y)| x * y).sum::<f32>() < 0.0 { -1.0 } else { 1.0 };
                        let v: [f32; 4] = std::array::from_fn(|j| a[j] + (s * b[j] - a[j]) * k);
                        let len = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                        [-v[0] / len, -v[1] / len, v[2] / len, v[3] / len]
                    });
                    match q {
                        Some(q) => trs(p, q),
                        None => {
                            let mut m = rest_local;
                            m[12] = p[0];
                            m[13] = p[1];
                            m[14] = p[2];
                            m
                        }
                    }
                }
                None => rest_local,
            };
            world[i] = Some(match parent_world {
                Some((pw, _)) => mul(&pw, &local),
                None => local,
            });
        }
    }
    let mut out = model.clone();
    for part in &mut out.parts {
        let Some(m) = part.bone.and_then(|b| world.get(b as usize).copied().flatten()) else { continue };
        for v in &mut part.positions {
            let [x, y, z] = *v;
            *v = [m[0] * x + m[4] * y + m[8] * z + m[12], m[1] * x + m[5] * y + m[9] * z + m[13], m[2] * x + m[6] * y + m[10] * z + m[14]];
        }
        for nrm in &mut part.normals {
            let [x, y, z] = *nrm;
            *nrm = [m[0] * x + m[4] * y + m[8] * z, m[1] * x + m[5] * y + m[9] * z, m[2] * x + m[6] * y + m[10] * z];
        }
        part.bone = None;
    }
    out.bones.clear();
    out
}

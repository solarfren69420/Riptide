//! H2Overdrive `mesh32.*` decoder.
//!
//! A mesh blob is a serialized memory image:
//! - `0x00` u32 4, u32 3, u32 fixup count, `"MESH"`, u32
//! - `0x14` fixups, 0x2c bytes each: `{u32 kind, u32 at, u32 target}`; kind 1 is an internal
//!   pointer (`body[at] = target`), kind 0 an external reference whose name (e.g.
//!   `txtr1.BT_Thunder_01`) sits at `+0x0c`.
//! - body (all offsets below are body-relative)
//!
//! Body: `[0x70]` -> geometry chunk `C`. `C+0x08` material count, `[C+0x0c]` -> materials,
//! 0x4d0 bytes each: shader reference at `+0x28`, texture references in `+0x2b4 + k*0x3c`, and at
//! `+0x4b0` the draw block `{n streams, names, n indices, index bytes, [indices], _, n sub, [sub]}`.
//! Vertex streams are records `{u32 count, u32 bytes, [data], u32 usage, ...}` between the chunk
//! header and the materials; usage 0 position, 1 normal, 2 binormal, 3 tangent, 4 colour,
//! 5 texcoord. Indices are absolute u16 into the shared streams, triangle list.
//! Source space is Direct3D (left-handed, Y up); output flips Z and the winding.

use crate::lux::{cstr, u32_at};
use crate::model::{Blend, Bone, MeshPart, Model};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

const MATERIAL_STRIDE: usize = 0x4d0;

struct Fixups {
    internal: HashMap<usize, usize>,
    external: Vec<(usize, String)>,
}

/// A mesh in its rest pose: skinned vertices placed by their bones, everything in one piece.
pub fn decode_mesh(name: &str, blob: &[u8]) -> Result<Model> {
    decode(name, blob, false)
}

/// A mesh for animating: [`Model::bones`] filled, and each part's vertices left in its bone's
/// space ([`MeshPart::bone`]); place a part with its bone's (animated) model-space matrix.
/// Parts whose mesh has no bones come out as from [`decode_mesh`].
pub fn decode_rigged(name: &str, blob: &[u8]) -> Result<Model> {
    decode(name, blob, true)
}

/// Direct3D row-major, row-vector matrix -> column-major matrix in output space (Z mirrored).
fn mirrored_cols(m: &[f32; 16]) -> [f32; 16] {
    // Read as column-major the array is already the column-vector transpose; mirroring Z is
    // S * M * S, which negates entries with exactly one index on the Z axis.
    // The stored matrices use only 3x4 affine entries. Their unused last
    // column (including m[15]) is zero, not a homogeneous matrix row.
    // Restore it before Bevy decomposes/inverts the skeleton transforms.
    std::array::from_fn(|k| {
        if k == 15 { 1.0 }
        else if k % 4 == 3 { 0.0 }
        else if (k / 4 == 2) != (k % 4 == 2) { -m[k] }
        else { m[k] }
    })
}

fn decode(name: &str, blob: &[u8], rigged: bool) -> Result<Model> {
    if blob.len() < 0x14 || &blob[0x0c..0x10] != b"MESH" {
        bail!("{name}: not a MESH blob");
    }
    let nfix = u32_at(blob, 8) as usize;
    let body_start = 0x14 + nfix * 0x2c;
    if body_start > blob.len() {
        bail!("{name}: fixup table overruns blob");
    }
    let body = &blob[body_start..];
    let mut fx = Fixups { internal: HashMap::new(), external: Vec::new() };
    for i in 0..nfix {
        let r = &blob[0x14 + i * 0x2c..0x14 + (i + 1) * 0x2c];
        let (kind, at, target) = (u32_at(r, 0), u32_at(r, 4) as usize, u32_at(r, 8) as usize);
        match kind {
            1 => {
                fx.internal.insert(at, target);
            }
            0 => fx.external.push((at, cstr(&r[0x0c..]))),
            _ => {}
        }
    }
    let rd = |o: usize| -> Option<u32> { (o + 4 <= body.len()).then(|| u32_at(body, o)) };
    let ptr = |o: usize| fx.internal.get(&o).copied().filter(|&t| t < body.len());

    let chunk = ptr(0x70).context("no geometry chunk")?;
    let nmat = rd(chunk + 0x08).unwrap_or(0) as usize;
    let mats = ptr(chunk + 0x0c).context("no material table")?;
    if nmat == 0 || nmat > 256 {
        bail!("{name}: implausible material count {nmat}");
    }

    // Vertex streams live between the chunk header and the first material.
    let mut streams: Vec<(usize, usize, usize, u32)> = Vec::new(); // (data, count, stride, usage)
    let mut slots: Vec<usize> = fx.internal.keys().copied().filter(|&a| a > chunk + 8 && a < mats).collect();
    slots.sort_unstable();
    for at in slots {
        let (Some(count), Some(bytes), Some(usage)) = (rd(at - 8), rd(at - 4), rd(at + 4)) else {
            continue;
        };
        let (count, bytes) = (count as usize, bytes as usize);
        if count == 0 || bytes == 0 || bytes % count != 0 || usage > 9 {
            continue;
        }
        let data = fx.internal[&at];
        if data + bytes > body.len() {
            continue;
        }
        streams.push((data, count, bytes / count, usage));
    }
    if std::env::var_os("RIPTIDE_MESH_DEBUG").is_some() {
        eprintln!("{name}: streams (data, count, stride, usage) {streams:?}");
    }
    let pos = streams
        .iter()
        .find(|s| s.3 == 0 && s.2 >= 12)
        .copied()
        .with_context(|| format!("{name}: no position stream"))?;
    let vcount = pos.1;
    let find = |usage: u32, stride: usize| streams.iter().find(|s| s.3 == usage && s.1 == vcount && s.2 >= stride).copied();
    let f32_at = |o: usize| f32::from_le_bytes(body[o..o + 4].try_into().unwrap());

    // Source-space (left-handed) positions/normals; the Z flip happens after skinning.
    let positions: Vec<[f32; 3]> = (0..vcount)
        .map(|i| {
            let o = pos.0 + i * pos.2;
            [f32_at(o), f32_at(o + 4), f32_at(o + 8)]
        })
        .collect();
    // Skinned meshes carry a palette slot in the byte after the position.
    let vbone: Vec<u8> = (0..vcount).map(|i| if pos.2 >= 16 { body[pos.0 + i * pos.2 + 12] } else { 0 }).collect();
    let normals: Option<Vec<[f32; 3]>> = find(1, 12).map(|s| {
        (0..vcount)
            .map(|i| {
                let o = s.0 + i * s.2;
                [f32_at(o), f32_at(o + 4), f32_at(o + 8)]
            })
            .collect()
    });
    // Bones: `C`-relative root block, `+0x40` count, `[+0x4c]` -> 0x170-byte records whose
    // bind-pose world matrix (row-major, row-vector convention) sits at `+0x60`.
    let nbones = rd(0x40).unwrap_or(0) as usize;
    let bones: Vec<[f32; 16]> = match ptr(0x4c) {
        Some(b) if nbones > 0 && nbones < 256 && b + nbones * 0x170 <= body.len() => (0..nbones)
            .map(|i| std::array::from_fn(|k| f32_at(b + i * 0x170 + 0x60 + k * 4)))
            .collect(),
        _ => Vec::new(),
    };
    // Bone records: `+0x04` name (0x20), `+0x24` own index, `+0x28` parent index (-1 = root).
    let rig: Vec<Bone> = match ptr(0x4c) {
        Some(b) if rigged && !bones.is_empty() => (0..bones.len())
            .map(|i| {
                let r = b + i * 0x170;
                let parent = rd(r + 0x28).filter(|&p| (p as usize) < bones.len()).map(|p| p as usize);
                Bone { name: cstr(&body[r + 4..r + 0x24]), parent, rest: mirrored_cols(&bones[i]) }
            })
            .collect(),
        _ => Vec::new(),
    };
    let vec3s = |usage: u32| -> Option<Vec<[f32; 3]>> {
        find(usage, 12).map(|s| (0..vcount).map(|i| { let o = s.0 + i * s.2; [f32_at(o), f32_at(o + 4), f32_at(o + 8)] }).collect())
    };
    let (binormals, tangents) = (vec3s(2), vec3s(3));
    // A second texcoord stream (two-texture terrain): the second usage-5 stream of this size.
    let uvs1: Option<Vec<[f32; 2]>> = streams.iter().filter(|s| s.3 == 5 && s.1 == vcount && s.2 >= 8).nth(1).map(|s| {
        (0..vcount).map(|i| { let o = s.0 + i * s.2; [f32_at(o), f32_at(o + 4)] }).collect()
    });
    if std::env::var_os("RIPTIDE_MESH_DEBUG").is_some() {
        for (k, set) in [("uv0", find(5, 8).map(|s| (0..vcount).map(|i| { let o = s.0 + i * s.2; [f32_at(o), f32_at(o + 4)] }).collect::<Vec<_>>())), ("uv1", uvs1.clone())] {
            if let Some(v) = set {
                let (lo, hi) = v.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
                eprintln!("{name}: {k} range {lo:?}..{hi:?} first {:?}", &v[..3.min(v.len())]);
            }
        }
    }
    let uvs: Option<Vec<[f32; 2]>> = find(5, 8).map(|s| {
        (0..vcount)
            .map(|i| {
                let o = s.0 + i * s.2;
                [f32_at(o), f32_at(o + 4)]
            })
            .collect()
    });
    let colors: Option<Vec<[f32; 4]>> = find(4, 4).map(|s| {
        (0..vcount)
            .map(|i| {
                let c = &body[s.0 + i * s.2..][..4];
                // D3DCOLOR is BGRA in memory.
                [srgb(c[2]), srgb(c[1]), srgb(c[0]), c[3] as f32 / 255.0]
            })
            .collect()
    });

    let mut model = Model { name: name.to_string(), ..Default::default() };
    for m in 0..nmat {
        let mb = mats + m * MATERIAL_STRIDE;
        if mb + MATERIAL_STRIDE > body.len() {
            break;
        }
        let ext_in = |lo: usize, hi: usize| fx.external.iter().filter(move |(at, _)| *at >= lo && *at < hi);
        let shader = ext_in(mb, mb + 0x40).find(|(_, n)| n.starts_with("shad4.")).map(|(_, n)| n.clone());
        let mut textures: Vec<(usize, &String)> =
            ext_in(mb + 0x40, mb + 0x4b0).filter(|(_, n)| n.starts_with("txtr1.")).map(|(a, n)| (*a, n)).collect();
        textures.sort_by_key(|t| t.0);
        if std::env::var_os("RIPTIDE_MESH_DEBUG").is_some() {
            eprintln!("  material {m}: shader {shader:?} textures {:?}", textures.iter().map(|t| (t.0 - mb, t.1)).collect::<Vec<_>>());
            let mut floats = String::new();
            for o in (0x2c..0x2b4).step_by(4) {
                if let Some(v) = body.get(mb + o..mb + o + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())) {
                    if v != 0.0 && v.is_finite() && v.abs() < 1e6 && v.abs() > 1e-6 {
                        floats += &format!(" {o:#x}={v}");
                    }
                }
            }
            eprintln!("    floats:{floats}");
        }
        // Slot 0 is the diffuse map; normal maps (`_N`) and lightmaps (`_LM`) follow.
        let texture = textures
            .iter()
            .map(|t| t.1)
            .find(|n| !n.ends_with("_N") && !n.contains("_LM") && !n.contains("skytest"))
            .or(textures.first().map(|t| t.1))
            .cloned();

        let nidx = rd(mb + 0x4b8).unwrap_or(0) as usize;
        let idx_bytes = rd(mb + 0x4bc).unwrap_or(0) as usize;
        let dbg = std::env::var_os("RIPTIDE_MESH_DEBUG").is_some();
        // 16-bit indices hang off 0x4c0, 32-bit ones off 0x4c4 (big sector meshes switch once their
        // vertices pass 65535; reading only 0x4c0 dropped every material after that point).
        let Some(idx_ptr) = ptr(mb + 0x4c0).or_else(|| ptr(mb + 0x4c4)) else {
            if dbg { eprintln!("  material {m}: skipped, no index pointer (nidx {nidx} bytes {idx_bytes})"); }
            continue;
        };
        if nidx == 0 || idx_bytes == 0 {
            if dbg { eprintln!("  material {m}: skipped, nidx {nidx} bytes {idx_bytes}"); }
            continue;
        }
        let isize = idx_bytes / nidx;
        if !(isize == 2 || isize == 4) || idx_ptr + idx_bytes > body.len() {
            continue;
        }
        let raw: Vec<u32> = (0..nidx)
            .map(|i| {
                let o = idx_ptr + i * isize;
                if isize == 2 {
                    u16::from_le_bytes([body[o], body[o + 1]]) as u32
                } else {
                    u32_at(body, o)
                }
            })
            .collect();

        // Submeshes: `{minv, nverts, first index, triangles, n, palette[n]}`, packed. The
        // palette maps a vertex's bone slot to a bone, per submesh.
        let nsub = rd(mb + 0x4c8).unwrap_or(0) as usize;
        let mut ranges: Vec<(usize, usize, Vec<usize>)> = Vec::new(); // (first index, count, palette)
        if let Some(mut sp) = ptr(mb + 0x4cc) {
            for _ in 0..nsub.min(64) {
                let (Some(first), Some(tris), Some(nb)) = (rd(sp + 8), rd(sp + 12), rd(sp + 16)) else { break };
                let nb = (nb as usize).min(256);
                let pal = (0..nb).filter_map(|k| rd(sp + 20 + k * 4).map(|v| v as usize)).collect();
                ranges.push((first as usize, tris as usize * 3, pal));
                sp += 20 + nb * 4;
            }
        }
        let palette_for = |index_pos: usize| -> Option<&Vec<usize>> {
            ranges.iter().find(|(f, n, _)| index_pos >= *f && index_pos < f + n).map(|r| &r.2)
        };

        // Compact the shared streams down to what this material draws: one part, or (rigged)
        // one per bone, each with its own vertex remap.
        let blank = MeshPart {
            texture: texture.map(|t| t.trim_start_matches("txtr1.").to_string()),
            textures: textures.iter().map(|t| t.1.trim_start_matches("txtr1.").to_string()).collect(),
            shader: shader.as_ref().map(|s| s.trim_start_matches("shad4.").to_string()),
            reflection_only: body.get(mb..mb + 0x20).is_some_and(|n| n.starts_with(b"REFLECTION_ONLY")),
            ..Default::default()
        };
        let mut split: Vec<(MeshPart, HashMap<(u32, usize), u32>)> = Vec::new();
        for (ti, tri) in raw.chunks_exact(3).enumerate() {
            if tri.iter().any(|&v| v as usize >= vcount) || tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                continue;
            }
            let bone_index = |vi: usize| -> Option<usize> {
                let pal = palette_for(ti * 3)?;
                let i = *pal.get(vbone[vi] as usize)?;
                (i < bones.len()).then_some(i)
            };
            // Rigged: the whole triangle rides its first vertex's bone (boat parts are rigid).
            let tri_bone = if rig.is_empty() { None } else { bone_index(tri[0] as usize) };
            let slot = match split.iter().position(|(p, _)| p.bone == tri_bone.map(|b| b as u16)) {
                Some(s) => s,
                None => {
                    split.push((MeshPart { bone: tri_bone.map(|b| b as u16), ..blank.clone() }, HashMap::new()));
                    split.len() - 1
                }
            };
            let (part, remap) = &mut split[slot];
            // Mirroring Z already turns Direct3D's clockwise fronts counter-clockwise.
            for &v in tri {
                let next = remap.len() as u32;
                let vi = v as usize;
                // Baked: place the vertex by its bone. Rigged: leave it in the bone's space.
                let b = if bones.is_empty() || !rig.is_empty() { None } else { bone_index(vi).map(|i| &bones[i]) };
                let key = (v, b.map_or(usize::MAX, |m| m.as_ptr() as usize));
                let id = *remap.entry(key).or_insert_with(|| {
                    let p = positions[vi];
                    let p = match b {
                        Some(m) => [
                            p[0] * m[0] + p[1] * m[4] + p[2] * m[8] + m[12],
                            p[0] * m[1] + p[1] * m[5] + p[2] * m[9] + m[13],
                            p[0] * m[2] + p[1] * m[6] + p[2] * m[10] + m[14],
                        ],
                        None => p,
                    };
                    part.positions.push([p[0], p[1], -p[2]]);
                    if let Some(n) = &normals {
                        let n = n[vi];
                        let n = match b {
                            Some(m) => [
                                n[0] * m[0] + n[1] * m[4] + n[2] * m[8],
                                n[0] * m[1] + n[1] * m[5] + n[2] * m[9],
                                n[0] * m[2] + n[1] * m[6] + n[2] * m[10],
                            ],
                            None => n,
                        };
                        part.normals.push([n[0], n[1], -n[2]]);
                    }
                    if let Some(t) = &uvs {
                        part.uvs.push(t[vi]);
                    }
                    if let Some(t) = &uvs1 {
                        part.uvs1.push(t[vi]);
                    }
                    if let (Some(n), Some(t), Some(bn)) = (&normals, &tangents, &binormals) {
                        let dir = |v: [f32; 3]| -> [f32; 3] {
                            let v = match b {
                                Some(m) => [v[0] * m[0] + v[1] * m[4] + v[2] * m[8], v[0] * m[1] + v[1] * m[5] + v[2] * m[9], v[0] * m[2] + v[1] * m[6] + v[2] * m[10]],
                                None => v,
                            };
                            [v[0], v[1], -v[2]]
                        };
                        let (nn, tt, bb) = (dir(n[vi]), dir(t[vi]), dir(bn[vi]));
                        let len = (tt[0] * tt[0] + tt[1] * tt[1] + tt[2] * tt[2]).sqrt().max(1e-9);
                        let tt = tt.map(|c| c / len);
                        let cross = [nn[1] * tt[2] - nn[2] * tt[1], nn[2] * tt[0] - nn[0] * tt[2], nn[0] * tt[1] - nn[1] * tt[0]];
                        let w = if cross[0] * bb[0] + cross[1] * bb[1] + cross[2] * bb[2] < 0.0 { -1.0 } else { 1.0 };
                        part.tangents.push([tt[0], tt[1], tt[2], w]);
                    }
                    if let Some(c) = &colors {
                        part.colors.push(c[vi]);
                    }
                    next
                });
                part.indices.push(id);
            }
        }
        for (mut part, _) in split {
            if part.indices.is_empty() {
                continue;
            }
            part.blend = match part.shader.as_deref() {
                Some("D_Cutout") => Blend::Cutout,
                Some(s) if s.starts_with("FX_Flare") || s.contains("Bolt") || s.contains("Glow") => Blend::Add,
                Some(s) if s.starts_with("FX_Water") || s.starts_with("FX_Particles") || s == "FX_Blur" => Blend::Blend,
                _ => Blend::Opaque,
            };
            model.parts.push(part);
        }
    }
    if model.parts.is_empty() {
        bail!("{name}: no drawable materials");
    }
    model.bones = rig;
    model.ensure_normals();
    Ok(model)
}

fn srgb(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[cfg(test)]
mod tests {
    use super::mirrored_cols;

    #[test]
    fn packed_affine_bone_preserves_homogeneous_points() {
        // The disk format leaves the fourth column zero. A translated point
        // must still have w=1 when consumed as a full matrix by the engine.
        let packed = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 4., 5., 6., 0.];
        let m = mirrored_cols(&packed);
        let p = [2., 3., -1., 1.];
        let placed: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m[c * 4 + r] * p[c]).sum());
        assert_eq!(placed, [6., 8., -7., 1.]);
    }
}

/// A spray surface on a boat hull (a mesh record named `spray`): triangles along the waterline
/// (sides, bow) or across the stern (the rooster tail). The original spaces water spray emitters
/// along it, each using the boat def's `Waterspray <index>` entry (CWaterspraySys, FUN_004f0a50).
/// Positions and normals are Riptide space (Z mirrored).
#[derive(Clone, Debug, Default)]
pub struct SprayLine {
    pub index: u32,
    pub triangles: Vec<[[f32; 3]; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// Every `spray` record in a mesh blob.
pub fn spray_lines(blob: &[u8]) -> Vec<SprayLine> {
    if blob.len() < 0x14 || &blob[0x0c..0x10] != b"MESH" {
        return Vec::new();
    }
    let nfix = u32_at(blob, 8) as usize;
    let body_start = 0x14 + nfix * 0x2c;
    if body_start > blob.len() {
        return Vec::new();
    }
    let body = &blob[body_start..];
    let mut internal: HashMap<usize, usize> = HashMap::new();
    for i in 0..nfix {
        let r = &blob[0x14 + i * 0x2c..0x14 + (i + 1) * 0x2c];
        if u32_at(r, 0) == 1 {
            internal.insert(u32_at(r, 4) as usize, u32_at(r, 8) as usize);
        }
    }
    let floats = |at: usize, n: usize| -> Vec<f32> {
        (0..n).filter_map(|k| body.get(at + k * 4..at + k * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()))).collect()
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + 80 <= body.len() {
        if &body[i..i + 6] == b"spray\0" {
            let idx = std::str::from_utf8(&body[i + 0x20..i + 0x40]).ok().and_then(|s| s.trim_end_matches('\0').parse::<u32>().ok());
            let (ca, cb) = (u32_at(body, i + 0x40) as usize, u32_at(body, i + 0x48) as usize);
            if let (Some(index), Some(&pa), Some(&pb)) = (idx, internal.get(&(i + 0x44)), internal.get(&(i + 0x4c))) {
                if ca < 4096 && cb < 4096 {
                    // Triangles: face normal + 3 u32 vertex indices (24 B); vertices: pos, normal, 4 spare (40 B).
                    let tris = floats(pa, ca * 6);
                    let verts = floats(pb, cb * 10);
                    let vert = |k: usize| verts.get(k * 10..k * 10 + 3).map(|p| [p[0], p[1], -p[2]]);
                    let mut line = SprayLine { index, ..Default::default() };
                    for t in tris.chunks_exact(6) {
                        let ids = [t[3], t[4], t[5]].map(|f| f.to_bits() as usize);
                        if let (Some(a), Some(b), Some(c)) = (vert(ids[0]), vert(ids[1]), vert(ids[2])) {
                            line.triangles.push([a, b, c]);
                            line.normals.push([t[0], t[1], -t[2]]);
                        }
                    }
                    out.push(line);
                }
            }
            i += 80;
        } else {
            i += 4;
        }
    }
    out
}

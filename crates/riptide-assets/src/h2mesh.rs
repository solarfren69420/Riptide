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
use crate::model::{Blend, MeshPart, Model};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

const MATERIAL_STRIDE: usize = 0x4d0;

struct Fixups {
    internal: HashMap<usize, usize>,
    external: Vec<(usize, String)>,
}

pub fn decode_mesh(name: &str, blob: &[u8]) -> Result<Model> {
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

    let mut model = Model { name: name.to_string(), parts: Vec::new() };
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
        let Some(idx_ptr) = ptr(mb + 0x4c0) else { continue };
        if nidx == 0 || idx_bytes == 0 {
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

        // Compact the shared streams down to what this material draws.
        let mut remap: HashMap<(u32, usize), u32> = HashMap::new();
        let mut part = MeshPart {
            texture: texture.map(|t| t.trim_start_matches("txtr1.").to_string()),
            shader: shader.as_ref().map(|s| s.trim_start_matches("shad4.").to_string()),
            ..Default::default()
        };
        for (ti, tri) in raw.chunks_exact(3).enumerate() {
            if tri.iter().any(|&v| v as usize >= vcount) || tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                continue;
            }
            let bone = |vi: usize| -> Option<&[f32; 16]> {
                let pal = palette_for(ti * 3)?;
                bones.get(*pal.get(vbone[vi] as usize)?)
            };
            // Mirroring Z already turns Direct3D's clockwise fronts counter-clockwise.
            for &v in tri {
                let next = remap.len() as u32;
                let vi = v as usize;
                let b = if bones.is_empty() { None } else { bone(vi) };
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
                    if let Some(c) = &colors {
                        part.colors.push(c[vi]);
                    }
                    next
                });
                part.indices.push(id);
            }
        }
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
    if model.parts.is_empty() {
        bail!("{name}: no drawable materials");
    }
    model.ensure_normals();
    Ok(model)
}

fn srgb(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

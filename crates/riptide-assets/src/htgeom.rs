//! Hydro Thunder `G*` geometry objects inside R2 archives.
//!
//! Header (body-relative): `+0x04` group count, `+0x08` draw-record count, `+0x0c` polygon
//! count, `+0x14` vertices, `+0x18` UVs, `+0x1c` corners, `+0x20` normals, `+0x28` [groups]
//! (0xc4 bytes each, `+0x04` record count, `+0x08` [records]), `+0x30` [polygons],
//! `+0x34` [materials], `+0x38` [vertices: f32 x,y,z + BGRA colour], `+0x3c` [UVs: f32 u,v],
//! `+0x40` [corners, 24 bytes, `+0` normal index], `+0x44` [normals: f32 x,y,z].
//!
//! Draw record (12 bytes): `{u32 polygon count, [polygons], [material]}`; the material holds the
//! texture reference at `+0x14`. Polygon (48 bytes): `f32 centre[3], radius, normal[3]`, then
//! three `{u16 vertex, u16 corner, u16 uv}` corners and a pad.

use crate::model::{Blend, MeshPart, Model};
use crate::r2::R2Object;
use anyhow::{bail, Context, Result};

pub fn decode_geometry(obj: &R2Object) -> Result<Model> {
    if obj.kind != b'G' {
        bail!("{} is not a geometry object", obj.name);
    }
    decode_geometry_at(obj, 0, false)
}

/// Decode a geometry header starting at body offset `base`. Objects draw their first group;
/// a track (`all_groups`) draws every group (one per sector), each draw record once.
pub fn decode_geometry_at(obj: &R2Object, base: usize, all_groups: bool) -> Result<Model> {
    let nvert = obj.u32(base + 0x14).context("header")? as usize;
    let nuv = obj.u32(base + 0x18).context("header")? as usize;
    let nnorm = obj.u32(base + 0x20).context("header")? as usize;
    let groups = obj.ptr(base + 0x28).context("no groups")?;
    let verts = obj.ptr(base + 0x38).context("no vertices")?;
    let uvs = obj.ptr(base + 0x3c);
    let corners = obj.ptr(base + 0x40);
    let normals = obj.ptr(base + 0x44);
    let ngroups = if all_groups { obj.u32(base + 4).unwrap_or(0) as usize } else { 1 };
    if nvert == 0 || nvert > 1_000_000 {
        bail!("{}: implausible vertex count {nvert}", obj.name);
    }

    let vertex = |i: usize| -> Option<([f32; 3], [f32; 4])> {
        if i >= nvert {
            return None;
        }
        let o = verts + i * 16;
        let p = [obj.f32(o)?, obj.f32(o + 4)?, -obj.f32(o + 8)?];
        let c = obj.body.get(o + 12..o + 16)?;
        Some((p, [srgb(c[2]), srgb(c[1]), srgb(c[0]), c[3] as f32 / 255.0]))
    };
    let uv = |i: usize| -> [f32; 2] {
        uvs.filter(|_| i < nuv)
            .and_then(|b| Some([obj.f32(b + i * 8)?, obj.f32(b + i * 8 + 4)?]))
            .unwrap_or([0.0, 0.0])
    };
    let normal = |corner: usize| -> Option<[f32; 3]> {
        let ni = obj.u32(corners? + corner * 24)? as usize;
        if ni >= nnorm {
            return None;
        }
        let b = normals? + ni * 12;
        Some([obj.f32(b)?, obj.f32(b + 4)?, -obj.f32(b + 8)?])
    };

    let mut records = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for g in 0..ngroups.min(4096) {
        let gb = groups + g * 0xc4;
        let nrec = obj.u32(gb + 4).unwrap_or(0) as usize;
        let Some(recs) = obj.ptr(gb + 8) else {
            if !all_groups {
                bail!("{}: group without records", obj.name);
            }
            continue;
        };
        for r in 0..nrec.min(if all_groups { 4096 } else { 64 }) {
            if seen.insert(recs + r * 12) {
                records.push(recs + r * 12);
            }
        }
    }
    let mut model = Model { name: obj.name.clone(), ..Default::default() };
    for rb in records {
        let npoly = obj.u32(rb).unwrap_or(0) as usize;
        let Some(polys) = obj.ptr(rb + 4) else { continue };
        let mat = obj.ptr(rb + 8);
        let texture = mat.and_then(|m| obj.external_at(m + 0x14)).map(str::to_string);
        // Material +0x00 low half: render flags. 0x10 ignores the texture's alpha (Tinytanic's hull
        // and windows, ramp tops: their alpha marks lights, mostly zero); 0x01 alone uses it as
        // cut-out coverage (flags, parrots, signs); 0x08 marks effects (fire, glows, gulls).
        let solid = mat.and_then(|m| obj.u32(m)).is_some_and(|w| w & 0x10 != 0);
        let mut part = MeshPart { texture, ..Default::default() };
        for p in 0..npoly.min(65_536) {
            let pb = polys + p * 48;
            let mut tri = [(0usize, 0usize, 0usize); 3];
            let mut ok = true;
            for (k, c) in tri.iter_mut().enumerate() {
                let o = pb + 28 + k * 6;
                match (obj.u16(o), obj.u16(o + 2), obj.u16(o + 4)) {
                    (Some(v), Some(cn), Some(t)) => *c = (v as usize, cn as usize, t as usize),
                    _ => ok = false,
                }
            }
            if !ok {
                continue;
            }
            let face_n = [
                obj.f32(pb + 16).unwrap_or(0.0),
                obj.f32(pb + 20).unwrap_or(1.0),
                -obj.f32(pb + 24).unwrap_or(0.0),
            ];
            // Hydro Thunder stores counter-clockwise fronts in its left-handed space; after the
            // Z mirror they need reversing to face outward.
            for &(v, cn, t) in &[tri[0], tri[2], tri[1]] {
                let Some((pos, col)) = vertex(v) else {
                    ok = false;
                    break;
                };
                part.positions.push(pos);
                part.colors.push(col);
                part.uvs.push(uv(t));
                part.normals.push(normal(cn).unwrap_or(face_n));
            }
            if !ok {
                let keep = part.indices.len();
                part.positions.truncate(keep);
                part.colors.truncate(keep);
                part.uvs.truncate(keep);
                part.normals.truncate(keep);
                continue;
            }
            let base = part.indices.len() as u32;
            part.indices.extend([base, base + 1, base + 2]);
        }
        if !part.indices.is_empty() {
            part.blend = if solid { Blend::Solid } else { Blend::Opaque };
            part.double_sided = true;
            model.parts.push(part);
        }
    }
    if model.parts.is_empty() {
        bail!("{}: no polygons decoded", obj.name);
    }
    if all_groups {
        // Thousands of records: one part per texture keeps the draw count sane.
        let mut merged: Vec<MeshPart> = Vec::new();
        for p in model.parts.drain(..) {
            match merged.iter_mut().find(|m| m.texture == p.texture && m.blend == p.blend) {
                Some(m) => {
                    let base = m.positions.len() as u32;
                    m.positions.extend(p.positions);
                    m.colors.extend(p.colors);
                    m.uvs.extend(p.uvs);
                    m.normals.extend(p.normals);
                    m.indices.extend(p.indices.iter().map(|i| i + base));
                }
                None => merged.push(p),
            }
        }
        model.parts = merged;
    }
    Ok(model)
}

fn srgb(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

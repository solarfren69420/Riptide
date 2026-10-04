//! Engine-neutral decoded geometry shared by both games' loaders.
//!
//! Coordinates are right-handed, Y up (Bevy's convention). Loaders convert on the way in.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Opaque,
    /// Alpha tested (foliage, fences, decals).
    Cutout,
    /// Alpha blended (glass, smoke, flames).
    Blend,
    /// Additive (glows, flares).
    Add,
    /// Opaque whatever the texture's alpha holds (Hydro Thunder materials that ignore it: the
    /// alpha marks lit windows and the like, not holes).
    Solid,
}

#[derive(Debug, Clone, Default)]
pub struct MeshPart {
    /// Rigged models: the bone whose space the vertices are in (see [`Model::bones`]).
    pub bone: Option<u16>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Linear RGBA vertex colour (pre-lit lighting on both games' geometry).
    pub colors: Vec<[f32; 4]>,
    /// Triangle list, counter-clockwise front faces.
    pub indices: Vec<u32>,
    /// Texture key in the owning archive, if any.
    pub texture: Option<String>,
    /// Source shader/material name, for blend heuristics.
    pub shader: Option<String>,
    pub blend: Blend,
    pub double_sided: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub name: String,
    pub parts: Vec<MeshPart>,
    /// Rigged models only: the skeleton, in the same order [`MeshPart::bone`] indexes.
    pub bones: Vec<Bone>,
}

impl Model {
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.parts.iter().flat_map(|p| p.positions.iter());
        let first = *it.next()?;
        let (mut lo, mut hi) = (first, first);
        for p in it {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Some((lo, hi))
    }

    pub fn triangle_count(&self) -> usize {
        self.parts.iter().map(|p| p.indices.len() / 3).sum()
    }

    /// Fill in face normals for parts that came without any.
    pub fn ensure_normals(&mut self) {
        for p in &mut self.parts {
            if p.normals.len() == p.positions.len() {
                continue;
            }
            let mut n = vec![[0.0f32; 3]; p.positions.len()];
            for t in p.indices.chunks_exact(3) {
                let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
                let (pa, pb, pc) = (p.positions[a], p.positions[b], p.positions[c]);
                let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
                let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
                let f = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                for &i in &[a, b, c] {
                    for k in 0..3 {
                        n[i][k] += f[k];
                    }
                }
            }
            for v in &mut n {
                let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                *v = if l > 1e-12 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 1.0, 0.0] };
            }
            p.normals = n;
        }
    }

    /// Wavefront OBJ dump for inspecting a decode in any 3D viewer.
    pub fn to_obj(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let mut base = 1;
        for (i, p) in self.parts.iter().enumerate() {
            let _ = writeln!(s, "o part{}_{}", i, p.texture.as_deref().unwrap_or("none"));
            for v in &p.positions {
                let _ = writeln!(s, "v {} {} {}", v[0], v[1], v[2]);
            }
            for t in &p.uvs {
                let _ = writeln!(s, "vt {} {}", t[0], 1.0 - t[1]);
            }
            let has_uv = p.uvs.len() == p.positions.len();
            for t in p.indices.chunks_exact(3) {
                let f = |k: usize| {
                    let i = t[k] as usize + base;
                    if has_uv { format!("{i}/{i}") } else { format!("{i}") }
                };
                let _ = writeln!(s, "f {} {} {}", f(0), f(1), f(2));
            }
            base += p.positions.len();
        }
        s
    }
}

/// One node of a rigged model's skeleton.
#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    /// Rest pose in model space (output space, Z mirrored), column-major.
    pub rest: [f32; 16],
}

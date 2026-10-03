//! Course collision geometry on parry3d: the course triangles go into parry triangle meshes
//! (each with a bounding-volume tree), and boats are a vertical cylinder (hull radius, from just
//! above the water to the wall probe height) tested against them:
//! - [`Set::push_out`]: exact cylinder-vs-triangle contacts, deepest one as a horizontal push;
//! - [`Set::sweep`]: the cylinder cast along a movement, first wall hit (no tunnelling);
//! - [`Set::down`] / [`Set::along`]: ray casts for floors and sight lines.
//!
//! Triangles keep their course-wide ids so callers can filter (scripted gates, corridors).

use bevy::prelude::*;
use parry3d::bounding_volume::{Aabb, BoundingVolume};
use parry3d::math::{Pose, Vector};
use parry3d::query::{self, Ray, RayCast, ShapeCastOptions};
use parry3d::shape::{Cylinder, TriMesh};

fn pv(v: Vec3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}

/// Course triangles of one kind (walls, floors, sight blockers).
#[derive(Default)]
pub struct Set {
    mesh: Option<TriMesh>,
    /// Course-wide triangle id of each mesh triangle.
    ids: Vec<u32>,
}

impl Set {
    /// The triangles of `tris` for which `keep(id)` holds.
    pub fn new(tris: &[[Vec3; 3]], keep: impl Fn(usize) -> bool) -> Self {
        let (mut verts, mut idx, mut ids) = (Vec::new(), Vec::new(), Vec::new());
        for (i, t) in tris.iter().enumerate().filter(|(i, _)| keep(*i)) {
            let base = verts.len() as u32;
            verts.extend(t.iter().map(|v| pv(*v)));
            idx.push([base, base + 1, base + 2]);
            ids.push(i as u32);
        }
        let mesh = if idx.is_empty() { None } else { TriMesh::new(verts, idx).map_err(|e| warn!("collision mesh: {e:?}")).ok() };
        Self { mesh, ids }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Mesh triangles whose bounds overlap `lo..hi`.
    fn near(&self, lo: Vec3, hi: Vec3) -> Vec<u32> {
        let Some(m) = &self.mesh else { return Vec::new() };
        let q = Aabb::new(pv(lo), pv(hi));
        m.bvh().leaves(|n| n.aabb().intersects(&q)).collect()
    }

    /// Push a vertical cylinder (`centre`, half height `half_h`, radius `r`) out of the
    /// triangles: the deepest contact as a horizontal push and its normal, plus the triangle
    /// id; `skip(id)` drops triangles.
    pub fn push_out(&self, centre: Vec3, half_h: f32, r: f32, skip: impl Fn(u32) -> bool) -> Option<(Vec2, Vec2, u32)> {
        let mesh = self.mesh.as_ref()?;
        let cyl = Cylinder::new(half_h, r);
        let pose = Pose::from_translation(pv(centre));
        let ext = Vec3::new(r, half_h, r);
        let mut best: Option<(f32, Vec2, u32)> = None;
        for i in self.near(centre - ext, centre + ext) {
            let id = self.ids[i as usize];
            if skip(id) {
                continue;
            }
            let tri = mesh.triangle(i);
            let Ok(Some(c)) = query::contact(&pose, &cyl, &Pose::IDENTITY, &tri, 0.0) else { continue };
            if c.dist >= 0.0 {
                continue;
            }
            // normal1 points from the hull into the wall: push the other way, across the water.
            let n = Vec2::new(-c.normal1.x, -c.normal1.z).normalize_or_zero();
            if n == Vec2::ZERO {
                continue;
            }
            let depth = -c.dist;
            if best.is_none_or(|b| depth > b.0) {
                best = Some((depth, n, id));
            }
        }
        best.map(|(d, n, id)| (n * d, n, id))
    }

    /// Cast the cylinder from `centre` along `delta`: the first hit as (fraction of `delta`,
    /// horizontal wall normal facing the hull, triangle id). Triangles the hull already
    /// overlaps are left to [`Self::push_out`].
    pub fn sweep(&self, centre: Vec3, delta: Vec3, half_h: f32, r: f32, skip: impl Fn(u32) -> bool) -> Option<(f32, Vec2, u32)> {
        let mesh = self.mesh.as_ref()?;
        if delta.length_squared() < 1e-6 {
            return None;
        }
        let cyl = Cylinder::new(half_h, r);
        let pose = Pose::from_translation(pv(centre));
        let ext = Vec3::new(r, half_h, r);
        let (lo, hi) = (centre.min(centre + delta) - ext, centre.max(centre + delta) + ext);
        let options = ShapeCastOptions { max_time_of_impact: 1.0, stop_at_penetration: false, ..Default::default() };
        let mut best: Option<(f32, Vec2, u32)> = None;
        for i in self.near(lo, hi) {
            let id = self.ids[i as usize];
            if skip(id) {
                continue;
            }
            let tri = mesh.triangle(i);
            let Ok(Some(hit)) = query::cast_shapes(&pose, pv(delta), &cyl, &Pose::IDENTITY, Vector::ZERO, &tri, options) else { continue };
            if hit.time_of_impact <= 0.0 {
                continue;
            }
            let n = Vec2::new(-hit.normal1.x, -hit.normal1.z).normalize_or_zero();
            // Only walls facing the movement stop it.
            if n == Vec2::ZERO || n.dot(Vec2::new(delta.x, delta.z)) >= 0.0 {
                continue;
            }
            if best.is_none_or(|b| hit.time_of_impact < b.0) {
                best = Some((hit.time_of_impact, n, id));
            }
        }
        best
    }

    /// Distance straight down from `from` to the first triangle, within `max`.
    pub fn down(&self, from: Vec3, max: f32) -> Option<f32> {
        let ray = Ray::new(pv(from), Vector::new(0.0, -1.0, 0.0));
        self.mesh.as_ref()?.cast_ray(&Pose::IDENTITY, &ray, max, true)
    }

    /// First crossing of `a -> b`, as a fraction of the segment.
    pub fn along(&self, a: Vec3, b: Vec3) -> Option<f32> {
        let ray = Ray::new(pv(a), pv(b - a));
        self.mesh.as_ref()?.cast_ray(&Pose::IDENTITY, &ray, 1.0, true)
    }
}

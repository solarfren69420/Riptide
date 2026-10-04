//! The navigation line: a path through the course that keeps clear of what is actually in the
//! water (pillars, bridge piers, wrecks, islands), built from the collision geometry when a race
//! loads. AI boats (and the autopilot) steer along it instead of the bare racing line, spread
//! across lanes only as far as the open water there allows.
//!
//! The course corridor (the racing line's quads) is rasterised into `nav_cell` cells. A cell is
//! blocked when a boat there would touch a wall by the physics' own test
//! ([`Collider::blocked`]). Each free cell gets its clearance (distance to the nearest blocked
//! or off-course cell), and a cheapest path runs from the start to the end of the course,
//! moving forward through the corridor's segments, with steps near obstacles costing more.

use crate::race::Collider;
use crate::sheets::physics as phy;
use crate::track::Track;
use bevy::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[derive(Resource)]
pub struct NavLine {
    pts: Vec<Vec3>,
    /// Distance along the line to each point.
    cum: Vec<f32>,
    /// Open water around each point (units to the nearest obstacle or corridor edge).
    clear: Vec<f32>,
    looped: bool,
}

const NONE: u32 = u32::MAX;

impl NavLine {
    pub fn build(track: &Track, col: &Collider) -> Option<Self> {
        let n_seg = track.edges.len().checked_sub(1)?;
        if n_seg == 0 {
            return None;
        }
        // Grid over the corridor's bounding box; coarser cells if it would be huge.
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for e in &track.edges {
            for p in [e.start, e.end] {
                lo = lo.min(Vec2::new(p[0], p[2]));
                hi = hi.max(Vec2::new(p[0], p[2]));
            }
        }
        let area = (hi - lo).x.max(1.0) * (hi - lo).y.max(1.0);
        let g = phy::NAV_CELL.max((area / phy::NAV_MAX_CELLS).sqrt());
        let (nx, nz) = (((hi.x - lo.x) / g) as usize + 2, ((hi.y - lo.y) / g) as usize + 2);
        let centre = |i: usize| lo + Vec2::new((i % nx) as f32 + 0.5, (i / nx) as f32 + 0.5) * g;
        let cell_of = |q: Vec2| {
            let c = ((q - lo) / g).floor();
            (c.x >= 0.0 && c.y >= 0.0 && (c.x as usize) < nx && (c.y as usize) < nz).then(|| c.y as usize * nx + c.x as usize)
        };

        // Corridor cells: their segment, distance along the course, water and course direction.
        let mut seg = vec![NONE; nx * nz];
        let mut progress = vec![0.0f32; nx * nz];
        let mut water = vec![0.0f32; nx * nz];
        let mut free = vec![false; nx * nz];
        // No riverbed (behind the banks): passable, at a cost.
        let mut land = vec![false; nx * nz];
        // Distance along the course: the nearest point of the centre line among nearby segments
        // (the quads overlap on bends and in wide basins, so the containing quad alone is no guide).
        let mids: Vec<Vec2> = track.edges.iter().map(|e| Vec2::new(e.start[0] + e.end[0], e.start[2] + e.end[2]) * 0.5).collect();
        let along = |q: Vec2, i: usize| {
            (i.saturating_sub(4)..(i + 5).min(n_seg))
                .map(|s| {
                    let (a, b) = (mids[s], mids[s + 1]);
                    let t = ((q - a).dot(b - a) / (b - a).length_squared().max(1e-6)).clamp(0.0, 1.0);
                    (q.distance(a + (b - a) * t), track.dist[s] + (track.dist[s + 1] - track.dist[s]) * t)
                })
                .min_by(|x, y| x.0.total_cmp(&y.0))
                .map_or(0.0, |(_, d)| d)
        };
        for i in 0..n_seg {
            let (a, b) = (&track.edges[i], &track.edges[i + 1]);
            let (mut qlo, mut qhi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
            for p in [a.start, a.end, b.start, b.end] {
                qlo = qlo.min(Vec2::new(p[0], p[2]));
                qhi = qhi.max(Vec2::new(p[0], p[2]));
            }
            let (c0, c1) = (((qlo - lo) / g).floor(), ((qhi - lo) / g).floor());
            for cz in c0.y.max(0.0) as usize..=(c1.y as usize).min(nz - 1) {
                for cx in c0.x.max(0.0) as usize..=(c1.x as usize).min(nx - 1) {
                    let k = cz * nx + cx;
                    let q = centre(k);
                    if seg[k] != NONE || !track.contains(i, q) {
                        continue;
                    }
                    let tp = track.locate(Vec3::new(q.x, 0.0, q.y), i);
                    seg[k] = i as u32;
                    progress[k] = along(q, i);
                    water[k] = tp.water;
                    let p = Vec3::new(q.x, tp.water, q.y);
                    land[k] = col.land(p);
                    free[k] = !col.blocked(p, g * 0.75, tp.forward);
                }
            }
        }

        // Clearance: chamfer distance to the nearest blocked or off-course cell.
        let mut clear: Vec<f32> = free.iter().map(|f| if *f { f32::MAX } else { 0.0 }).collect();
        let (d1, d2) = (g, g * std::f32::consts::SQRT_2);
        for k in 0..nx * nz {
            let (x, z) = (k % nx, k / nx);
            let mut v = clear[k];
            if x > 0 { v = v.min(clear[k - 1] + d1); }
            if z > 0 {
                v = v.min(clear[k - nx] + d1);
                if x > 0 { v = v.min(clear[k - nx - 1] + d2); }
                if x + 1 < nx { v = v.min(clear[k - nx + 1] + d2); }
            }
            clear[k] = v;
        }
        for k in (0..nx * nz).rev() {
            let (x, z) = (k % nx, k / nx);
            let mut v = clear[k];
            if x + 1 < nx { v = v.min(clear[k + 1] + d1); }
            if z + 1 < nz {
                v = v.min(clear[k + nx] + d1);
                if x + 1 < nx { v = v.min(clear[k + nx + 1] + d2); }
                if x > 0 { v = v.min(clear[k + nx - 1] + d2); }
            }
            clear[k] = v;
        }

        // Cheapest path from the start to the last stretch of the course, moving between cells
        // close together along the course (`nav_step_window`).
        let start_q = track.point(0, 0.3, 0.5).xz();
        let start = (0..nx * nz)
            .filter(|&k| free[k] && seg[k] != NONE && (seg[k] as usize) < 2)
            .min_by(|&a, &b| centre(a).distance(start_q).total_cmp(&centre(b).distance(start_q)))?;
        // The goal: past the finish plane (point to point), or round to the start line (circuits).
        let goal_at = match track.finish {
            Some((_, _, at)) if !track.looped => at.min(track.length() - 3.0 * g),
            _ => track.length() - 3.0 * g,
        };
        let pass = |k: usize, need: f32| free[k] && clear[k] >= need;
        let mut need = phy::BOAT_RADIUS;
        let (came, end) = loop {
            let mut cost = vec![f32::MAX; nx * nz];
            let mut came = vec![NONE; nx * nz];
            let mut heap = BinaryHeap::new();
            cost[start] = 0.0;
            heap.push(Reverse((0u32, start)));
            let mut end = None;
            let mut reached = start;
            while let Some(Reverse((c, k))) = heap.pop() {
                let c = f32::from_bits(c);
                if progress[k] > progress[reached] {
                    reached = k;
                }
                if c > cost[k] {
                    continue;
                }
                if progress[k] >= goal_at {
                    end = Some(k);
                    break;
                }
                let (x, z) = ((k % nx) as isize, (k / nx) as isize);
                for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                    let (ax, az) = (x + dx, z + dz);
                    if ax < 0 || az < 0 || ax as usize >= nx || az as usize >= nz {
                        continue;
                    }
                    let j = az as usize * nx + ax as usize;
                    // Neighbours in far-apart segments and far apart along the course are different
                    // laps of a loop (or the far side of a hairpin): no stepping across.
                    let near_seg = (seg[j] as i64 - seg[k] as i64).abs() <= 1;
                    if !pass(j, need) || !(near_seg || (progress[j] - progress[k]).abs() <= phy::NAV_STEP_WINDOW) {
                        continue;
                    }
                    let step = if dx != 0 && dz != 0 { d2 } else { d1 };
                    let near = (g / clear[j].max(1.0)).min(1.0);
                    let dry = if land[j] { phy::NAV_LAND_COST } else { 1.0 };
                    let nc = c + step * dry * (1.0 + phy::NAV_AVOID * near * near);
                    if nc < cost[j] {
                        cost[j] = nc;
                        came[j] = k as u32;
                        // Costs are positive, so their bit patterns order like the floats.
                        heap.push(Reverse((nc.to_bits(), j)));
                    }
                }
            }
            match end {
                Some(e) => break (came, e),
                // Too tight for a whole boat somewhere: any open cell will do.
                None if need > 0.0 => need = 0.0,
                None => {
                    let q = centre(reached);
                    warn!("nav line: no way through after {:.0} of {:.0} units (stuck near {:.0} {:.0})", progress[reached], track.length(), q.x, q.y);
                    // What closes the way: the walls just past the furthest cell reached.
                    let mut seen = std::collections::BTreeSet::new();
                    let (rx, rz) = ((reached % nx) as isize, (reached / nx) as isize);
                    for dz in -3..=3isize {
                        for dx in -3..=3isize {
                            let (ax, az) = (rx + dx, rz + dz);
                            if ax < 0 || az < 0 || ax as usize >= nx || az as usize >= nz {
                                continue;
                            }
                            let j = az as usize * nx + ax as usize;
                            if seg[j] == NONE || free[j] {
                                continue;
                            }
                            let c = centre(j);
                            let tp = track.locate(Vec3::new(c.x, 0.0, c.y), seg[j] as usize);
                            if let Some((h, id, t)) = col.blocker(Vec3::new(c.x, water[j], c.y), g * 0.75, tp.forward) {
                                if seen.insert(id) {
                                    warn!("nav line:   blocked by triangle {id} (cut {h:.0} above water {:.0}): {:?}", water[j], t.map(|v| v.round().to_array()));
                                }
                            }
                        }
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(path) = std::env::var_os("RIPTIDE_NAV_DUMP") {
                        dump(std::path::Path::new(&path), nx, nz, &seg, &free, &land, &clear, &[], start, reached);
                    }
                    return None;
                }
            }
        };
        let mut cells = vec![end];
        while let Some(&k) = cells.last() {
            match came[k] {
                NONE => break,
                p => cells.push(p as usize),
            }
        }
        cells.reverse();

        // Smooth the staircase (a few averaging passes), keeping the ends.
        let mut pts: Vec<Vec2> = cells.iter().map(|&k| centre(k)).collect();
        for _ in 0..phy::NAV_SMOOTH as usize {
            let prev = pts.clone();
            for i in 1..pts.len().saturating_sub(1) {
                pts[i] = (prev[i - 1] + prev[i] * 2.0 + prev[i + 1]) * 0.25;
            }
        }
        let mut line = NavLine { pts: Vec::new(), cum: Vec::new(), clear: Vec::new(), looped: track.looped };
        let mut total = 0.0;
        for (i, p) in pts.iter().enumerate() {
            let k = cell_of(*p).filter(|&k| seg[k] != NONE).unwrap_or(cells[i]);
            if let Some(last) = line.pts.last() {
                total += Vec2::new(last.x, last.z).distance(*p);
            }
            line.pts.push(Vec3::new(p.x, water[k], p.y));
            line.cum.push(total);
            line.clear.push(clear[cells[i]]);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = std::env::var_os("RIPTIDE_NAV_DUMP") {
            dump(std::path::Path::new(&path), nx, nz, &seg, &free, &land, &clear, &cells, start, end);
        }
        // Much shorter than the racing line: it cut through overlapping cross-sections somewhere the
        // course doesn't go. Leave the AI on the racing line there.
        if total < goal_at * phy::NAV_MIN_LENGTH {
            warn!("nav line: {total:.0} units against {goal_at:.0} along the course, a shortcut: not used");
            return None;
        }
        let blocked = free.iter().zip(&seg).filter(|(f, s)| **s != NONE && !**f).count();
        info!(
            "nav line: {} points, {:.0} units ({:.0}-unit cells, {} of {} corridor cells blocked){}",
            line.pts.len(),
            total,
            g,
            blocked,
            seg.iter().filter(|s| **s != NONE).count(),
            if need == 0.0 { ", squeezes through a gap narrower than a boat" } else { "" }
        );
        Some(line)
    }

    pub fn length(&self) -> f32 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    /// The point nearest `p`, searching around `hint` first (a boat's last index).
    pub fn nearest(&self, p: Vec3, hint: usize) -> usize {
        let d = |i: usize| self.pts[i].xz().distance_squared(p.xz());
        let n = self.pts.len();
        let lo = hint.saturating_sub(40).min(n.saturating_sub(1));
        let hi = (hint + 120).min(n);
        let local = (lo..hi).min_by(|a, b| d(*a).total_cmp(&d(*b))).unwrap_or(0);
        // Far off the line (a respawn, a shortcut, the first call): search everything.
        if d(local) > (4.0 * phy::AI_LOOK_MAX).powi(2) || hint == usize::MAX {
            return (0..n).min_by(|a, b| d(*a).total_cmp(&d(*b))).unwrap_or(0);
        }
        local
    }

    /// Where to aim from point `i`: `ahead` units further along, moved sideways by `lane`
    /// (0..1 across) only as far as the open water there allows.
    pub fn aim(&self, i: usize, ahead: f32, lane: f32) -> Vec3 {
        let mut at = self.cum[i] + ahead;
        if self.looped && at > self.length() {
            at -= self.length();
        }
        let j = self.cum.partition_point(|c| *c < at).min(self.pts.len() - 1);
        let p = self.pts[j];
        let dir = (self.pts[(j + 1).min(self.pts.len() - 1)] - self.pts[j.saturating_sub(1)]).xz().normalize_or_zero();
        let room = (self.clear[j] - 2.0 * phy::BOAT_RADIUS).max(0.0);
        let side = Vec2::new(-dir.y, dir.x) * (lane - 0.5) * 2.0 * room;
        p + Vec3::new(side.x, 0.0, side.y)
    }
}

/// `RIPTIDE_NAV_DUMP=out.png`: the grid seen from above (north up): off-course clear, blocked
/// red, dry land brown, open water blue (brighter = more room), the line white, start green, end magenta.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn dump(path: &std::path::Path, nx: usize, nz: usize, seg: &[u32], free: &[bool], land: &[bool], clear: &[f32], cells: &[usize], start: usize, end: usize) {
    let step = nx.max(nz).div_ceil(2048).max(1);
    let (w, h) = (nx.div_ceil(step), nz.div_ceil(step));
    let mut img = vec![0u8; w * h * 4];
    let mut put = |k: usize, c: [u8; 3]| {
        let (x, z) = ((k % nx) / step, (k / nx) / step);
        let i = (z * w + x) * 4;
        img[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
    };
    for k in 0..nx * nz {
        if seg[k] == NONE {
            continue;
        }
        if !free[k] {
            put(k, [200, 40, 40]);
        } else if land[k] {
            put(k, [130, 95, 50]);
        } else {
            let v = (clear[k] / 400.0).clamp(0.0, 1.0);
            put(k, [20, (60.0 + 80.0 * v) as u8, (90.0 + 165.0 * v) as u8]);
        }
    }
    for &k in cells {
        put(k, [255, 255, 255]);
    }
    for (k, c) in [(start, [40, 255, 40]), (end, [255, 40, 255])] {
        for dz in -3isize..=3 {
            for dx in -3isize..=3 {
                let (x, z) = ((k % nx) as isize + dx * step as isize, (k / nx) as isize + dz * step as isize);
                if x >= 0 && z >= 0 && (x as usize) < nx && (z as usize) < nz {
                    put(z as usize * nx + x as usize, c);
                }
            }
        }
    }
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&img)?;
        Ok(())
    };
    match write() {
        Ok(()) => info!("nav dump: {} ({w}x{h})", path.display()),
        Err(e) => warn!("nav dump {}: {e}", path.display()),
    }
}

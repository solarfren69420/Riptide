//! The drivable corridor, built from the level's AI racing line.
//!
//! The line is a list of cross-sections (`start`/`end` banks plus a water height). A boat's
//! place on the track is `(segment, s, u)`: which pair of cross-sections it is between, how far
//! along (0..1), and how far across from the start bank (0..1).

use bevy::prelude::*;
use riptide_assets::h2level::Edge;

#[derive(Resource, Clone)]
pub struct Track {
    pub edges: Vec<Edge>,
    /// Cumulative centre-line distance at each cross-section.
    pub dist: Vec<f32>,
    /// The line closes on itself (a circuit raced over several laps).
    pub looped: bool,
    pub laps: u32,
    /// Authored start slots (position, yaw); empty = build a grid from the first cross-section.
    pub starts: Vec<(Vec3, f32)>,
    /// Per cross-section AI driving band (fractions start..end bank); empty = whole width.
    pub lanes: Vec<[f32; 2]>,
    /// Finish plane (point, course direction, race distance) for point-to-point courses.
    pub finish: Option<(Vec2, Vec2, f32)>,
    /// The course's own gravity (units/s²) when boats don't bring theirs: Hydro Thunder courses,
    /// whose physics isn't decoded (H2Overdrive boat defs carry their own).
    pub gravity: Option<f32>,
    /// Open water (Hackworld): no banks, floors anywhere, the line only guides the AI.
    pub open: bool,
    /// Additional playable corridors; the main path still defines AI and race progress.
    pub branches: Vec<[Edge; 2]>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TrackPos {
    pub seg: usize,
    pub s: f32,
    pub u: f32,
    pub water: f32,
    /// Distance along the centre line from the first cross-section.
    pub progress: f32,
    /// Unit vector along the track (XZ).
    pub forward: Vec2,
    /// Physical side-route sector and its local along fraction.
    pub branch: Option<(usize, f32)>,
}

fn v2(p: [f32; 3]) -> Vec2 {
    Vec2::new(p[0], p[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edge(x: f32, z: f32, water: f32) -> Edge {
        Edge { start: [x,water,z], end: [x+10.0,water,z], water }
    }
    #[test]
    fn side_route_keeps_its_water_and_bank_position() {
        let mut track = Track::new(vec![edge(0.0,0.0,0.0),edge(0.0,10.0,0.0),edge(0.0,20.0,0.0)]);
        track.branches.push([edge(12.0,0.0,30.0),edge(12.0,10.0,20.0)]);
        let at = track.locate(Vec3::new(15.0,25.0,5.0),0);
        assert_eq!(at.branch, Some((0,0.5)));
        assert!((at.water-25.0).abs()<0.001);
        assert!((at.u-0.3).abs()<0.001);
        assert_eq!(track.point_at(at,0.0),Vec3::new(12.0,25.0,5.0));
        let outside = track.locate(Vec3::new(23.0,25.0,5.0),0);
        assert!(outside.branch.is_some());
        assert_eq!(track.point_at(outside,1.0).x,22.0);
        assert!(track.locate(Vec3::new(5.0,0.0,5.0),0).branch.is_none());
    }

    #[test]
    fn reversed_cross_sections_are_turned_by_majority() {
        let e = |z: f32, flip: bool| {
            let (a, b) = ([0.0, 0.0, z], [10.0, 0.0, z]);
            let (start, end) = if flip { (b, a) } else { (a, b) };
            Edge { start, end, water: 0.0 }
        };
        // A reversed edge in the middle.
        let mut t = Track::new(vec![e(0.0, false), e(10.0, false), e(20.0, true), e(30.0, false)]);
        t.lanes = vec![[0.1, 0.4]; 4];
        assert_eq!(t.orient(), 1);
        assert_eq!(t.edges[2].start, [0.0, 0.0, 20.0]);
        assert_eq!(t.lanes[2], [0.6, 0.9]);
        // The first edge is the odd one out: it turns, not the rest.
        let mut t = Track::new(vec![e(0.0, true), e(10.0, false), e(20.0, false), e(30.0, false)]);
        assert_eq!(t.orient(), 1);
        assert_eq!(t.edges[0].start, [0.0, 0.0, 0.0]);
    }
}

impl Track {
    pub fn new(edges: Vec<Edge>) -> Self {
        let mut dist = vec![0.0];
        for i in 1..edges.len() {
            let d = Self::mid_of(&edges[i]).distance(Self::mid_of(&edges[i - 1]));
            dist.push(dist[i - 1] + d);
        }
        let looped = edges.len() > 3 && Self::mid_of(&edges[0]).distance(Self::mid_of(&edges[edges.len() - 1])) < 2500.0;
        Self { edges, dist, looped, laps: if looped { 3 } else { 1 }, starts: Vec::new(), lanes: Vec::new(), finish: None, open: false, branches: Vec::new(), gravity: None }
    }

    /// Make every cross-section run the same way across the course as the one before it (a
    /// reversed one twists both neighbouring quads into bow-ties and flips the lanes there:
    /// Ship Graveyard's edge 94). The AI lane band flips with it. Returns how many were turned.
    pub fn orient(&mut self) -> usize {
        let across = |e: &Edge| Vec2::new(e.end[0] - e.start[0], e.end[2] - e.start[2]);
        // Which edges disagree with the first, following the chain; the majority way wins (New
        // York's first edge is the reversed one).
        let mut flip = vec![false; self.edges.len()];
        for i in 1..self.edges.len() {
            let agree = across(&self.edges[i]).dot(across(&self.edges[i - 1])) >= 0.0;
            flip[i] = if agree { flip[i - 1] } else { !flip[i - 1] };
        }
        if flip.iter().filter(|f| **f).count() * 2 > flip.len() {
            flip.iter_mut().for_each(|f| *f = !*f);
        }
        for (i, _) in flip.iter().enumerate().filter(|(_, f)| **f) {
            let e = &mut self.edges[i];
            std::mem::swap(&mut e.start, &mut e.end);
            if let Some(l) = self.lanes.get_mut(i) {
                *l = [1.0 - l[1], 1.0 - l[0]];
            }
        }
        flip.iter().filter(|f| **f).count()
    }

    /// Total race distance covered by a boat on `lap` at `progress`.
    pub fn race_distance(&self, lap: u32, progress: f32) -> f32 {
        lap as f32 * self.length() + progress
    }

    fn mid_of(e: &Edge) -> Vec2 {
        (v2(e.start) + v2(e.end)) * 0.5
    }

    pub fn length(&self) -> f32 {
        *self.dist.last().unwrap_or(&0.0)
    }

    pub fn last_seg(&self) -> usize {
        self.edges.len().saturating_sub(2)
    }

    /// AI band at fraction `s` of segment `seg` (interpolated between its cross-sections).
    pub fn lane_band(&self, seg: usize, s: f32) -> [f32; 2] {
        match (self.lanes.get(seg), self.lanes.get(seg + 1)) {
            (Some(a), Some(b)) => [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s],
            _ => [0.0, 1.0],
        }
    }

    /// Point on the cross-section at fraction `s` of segment `seg`, `u` across, on the water.
    pub fn point(&self, seg: usize, s: f32, u: f32) -> Vec3 {
        let seg = seg.min(self.last_seg());
        let (a, b) = (&self.edges[seg], &self.edges[seg + 1]);
        let l = v2(a.start).lerp(v2(b.start), s);
        let r = v2(a.end).lerp(v2(b.end), s);
        let p = l.lerp(r, u);
        // Same waterfall rule as `locate`: the upper level holds to the edge.
        let water = if a.water - b.water > crate::sheets::physics::WATERFALL_DROP { a.water } else { a.water + (b.water - a.water) * s };
        Vec3::new(p.x, water, p.y)
    }

    pub fn point_at(&self, at: TrackPos, u: f32) -> Vec3 {
        if let Some((i, s)) = at.branch {
            let [a, b] = self.branches[i];
            let p = Vec3::from(a.start).lerp(Vec3::from(a.end), u)
                .lerp(Vec3::from(b.start).lerp(Vec3::from(b.end), u), s.clamp(0.0, 1.0));
            return p;
        }
        self.point(at.seg, at.s.clamp(0.0, 1.0), u)
    }

    /// Locate `p` with no hint: the segment whose quad holds it, else the nearest cross-section.
    pub fn locate_anywhere(&self, p: Vec3) -> TrackPos {
        let q = Vec2::new(p.x, p.z);
        let seg = (0..=self.last_seg()).find(|&s| self.contains(s, q)).unwrap_or_else(|| {
            (0..=self.last_seg())
                .min_by(|&a, &b| Self::mid_of(&self.edges[a]).distance(q).total_cmp(&Self::mid_of(&self.edges[b]).distance(q)))
                .unwrap_or(0)
        });
        self.locate(p, seg)
    }

    /// Locate `p` relative to the track, starting the search at `hint`.
    pub fn locate(&self, p: Vec3, hint: usize) -> TrackPos {
        let q = Vec2::new(p.x, p.z);
        let mut seg = hint.min(self.last_seg());
        // Prefer a segment whose quad actually contains the point: the current one, then
        // forward, then back. Where cross-sections overlap (hairpins around a pier) this keeps
        // the boat in the segment it is really in instead of extrapolating backwards.
        for d in [0isize, 1, 2, -1, 3, -2] {
            let k = seg as isize + d;
            if k >= 0 && (k as usize) <= self.last_seg() && self.contains(k as usize, q) {
                seg = k as usize;
                break;
            }
        }
        let inside = self.contains(seg, q);
        // Inside a (possibly non-convex) quad the bisector fraction can overshoot: clamp it.
        let mut s = if inside { self.along(seg, q).clamp(0.0, 1.0) } else { self.along(seg, q) };
        for _ in 0..if inside { 0 } else { 8 } {
            s = self.along(seg, q);
            if s > 1.0 && seg < self.last_seg() {
                seg += 1;
            } else if s < 0.0 && seg > 0 {
                seg -= 1;
            } else {
                break;
            }
        }
        let (a, b) = (&self.edges[seg], &self.edges[seg + 1]);
        let sc = s.clamp(0.0, 1.0);
        let l = v2(a.start).lerp(v2(b.start), sc);
        let r = v2(a.end).lerp(v2(b.end), sc);
        let lr = r - l;
        let width = lr.length().max(1.0);

        let u = (q - l).dot(lr) / (width * width);
        let ma = Self::mid_of(a);
        let mb = Self::mid_of(b);
        let forward = (mb - ma).normalize_or(Vec2::NEG_Y);
        let mut at = TrackPos {
            seg,
            s,
            u,
            // A waterfall holds the upper level to the edge instead of sloping down to the pool.
            water: if a.water - b.water > crate::sheets::physics::WATERFALL_DROP { a.water } else { a.water + (b.water - a.water) * sc },
            progress: self.dist[seg] + (self.dist[seg + 1] - self.dist[seg]) * s,
            forward,
            branch: None,
        };
        if !self.contains(seg, q) {
            let nearest = |a: &Edge, b: &Edge| {
                let s = Self::along_edges(a, b, q).clamp(0.0, 1.0);
                let l = v2(a.start).lerp(v2(b.start), s);
                let r = v2(a.end).lerp(v2(b.end), s);
                let u = (q-l).dot(r-l)/(r-l).length_squared().max(1.0);
                q.distance_squared(l.lerp(r, u.clamp(0.0, 1.0)))
            };
            if let Some((i, [a, b])) = self.branches.iter().enumerate()
                .filter(|(_, [ba, bb])| Self::contains_edges(ba, bb, q) || nearest(ba, bb) < nearest(a, b))
                .min_by(|(_, [a, b]), (_, [c, d])| nearest(a,b).total_cmp(&nearest(c,d))) {
                let s = Self::along_edges(a, b, q).clamp(0.0, 1.0);
                let l = v2(a.start).lerp(v2(b.start), s);
                let r = v2(a.end).lerp(v2(b.end), s);
                at.u = (q - l).dot(r - l) / (r - l).length_squared().max(1.0);
                at.water = if a.water - b.water > crate::sheets::physics::WATERFALL_DROP { a.water } else { a.water + (b.water - a.water) * s };
                at.forward = (Self::mid_of(b) - Self::mid_of(a)).normalize_or(forward);
                at.branch = Some((i, s));
            }
        }
        at
    }

    /// Is `q` inside the quad between cross-sections `seg` and `seg + 1`?
    pub fn contains(&self, seg: usize, q: Vec2) -> bool {
        let (a, b) = (&self.edges[seg], &self.edges[seg + 1]);
        Self::contains_edges(a, b, q)
    }

    fn contains_edges(a: &Edge, b: &Edge, q: Vec2) -> bool {
        let (p0, p1, p2, p3) = (v2(a.start), v2(a.end), v2(b.end), v2(b.start));
        let tri = |a: Vec2, b: Vec2, c: Vec2| {
            let (d1, d2, d3) = ((b - a).perp_dot(q - a), (c - b).perp_dot(q - b), (a - c).perp_dot(q - c));
            (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
        };
        tri(p0, p1, p2) || tri(p0, p2, p3)
    }

    /// Fraction along segment `seg` of the projection of `q` onto the segment's centre line,
    /// measured along the bisector of the two cross-sections so neighbouring segments agree.
    fn along(&self, seg: usize, q: Vec2) -> f32 {
        let (a, b) = (&self.edges[seg], &self.edges[seg + 1]);
        Self::along_edges(a, b, q)
    }

    fn along_edges(a: &Edge, b: &Edge, q: Vec2) -> f32 {
        let (ma, mb) = (Self::mid_of(a), Self::mid_of(b));
        // Signed distance from each cross-section line, oriented along travel.
        let da = Self::side(a, q, mb - ma);
        let db = Self::side(b, q, mb - ma);
        if (da - db).abs() < 1e-3 {
            return 0.0;
        }
        da / (da - db)
    }

    fn side(e: &Edge, q: Vec2, dir: Vec2) -> f32 {
        let along = (v2(e.end) - v2(e.start)).normalize_or(Vec2::X);
        let mut n = Vec2::new(-along.y, along.x);
        if n.dot(dir) < 0.0 {
            n = -n;
        }
        (q - v2(e.start)).dot(n)
    }

    /// Start slot `i`: the authored slot when the course has them, else rows of four across the
    /// first cross-section, nose down the track.
    pub fn grid(&self, i: usize) -> (Vec3, f32) {
        if let Some(&s) = self.starts.get(i) {
            return s;
        }
        let row = i / 4;
        let col = i % 4;
        let base = self.locate(self.point(0, 0.0, 0.5), 0);
        let fwd = base.forward;
        let u = 0.2 + 0.2 * col as f32;
        let back = 140.0 * row as f32 + 60.0 * (col % 2) as f32;
        let mut p = self.point(0, 0.35, u);
        p.x -= fwd.x * back;
        p.z -= fwd.y * back;
        // Bevy forward is -Z; yaw so -Z aligns with the track direction.
        let yaw = (-fwd.x).atan2(-fwd.y);
        (p, yaw)
    }
}

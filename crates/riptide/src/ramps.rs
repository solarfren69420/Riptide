//! Ramp audit: find every ramp on a course in its collision data and drive the player over one.
//!
//! `RIPTIDE_RAMPS=1` logs every ramp (`RAMP <i> ...`). `RIPTIDE_TEST_RAMP=<i>` (with the
//! autopilot, i.e. a capture run) puts the player `ramp_test_runup` before ramp `i`, aimed up its
//! slope, full throttle and boost, and reports what happened (`RAMP TEST <i> ...`): wall contacts
//! on the way, whether the boat ever sank below the ramp's surface (clipping through it), the
//! launch climb rate, the peak height above the water and the time in the air.

use crate::race::{Boat, Collider, RaceClock};
use crate::sheets::physics as phy;
use crate::track::Track;
use bevy::prelude::*;

#[derive(Clone, Debug)]
pub struct Ramp {
    pub centre: Vec3,
    /// Horizontal direction up the slope.
    pub uphill: Vec2,
    /// Lowest and highest point of the ramp surface.
    pub base: f32,
    pub top: f32,
    /// Distance along the course.
    pub progress: f32,
    pub triangles: usize,
    /// Rise per unit run (area-weighted).
    pub slope: f32,
}

/// Sloped floor triangles in the course corridor that rise above the water, grouped into ramps
/// (neighbouring triangles facing roughly the same way), in course order.
pub fn find(col: &Collider, track: &Track) -> Vec<Ramp> {
    struct Tri {
        c: Vec3,
        n: Vec3,
        area: f32,
        lo: f32,
        hi: f32,
    }
    let mut tris = Vec::new();
    for (i, t) in col.tris.iter().enumerate() {
        if !col.floor_tri.get(i).copied().unwrap_or(false) {
            continue;
        }
        let cross = (t[1] - t[0]).cross(t[2] - t[0]);
        let area = cross.length() * 0.5;
        if area < 1.0 {
            continue;
        }
        let mut n = cross.normalize();
        if n.y < 0.0 {
            n = -n;
        }
        if !(phy::RAMP_FIND_MIN_NY..phy::RAMP_FIND_MAX_NY).contains(&n.y) {
            continue;
        }
        let c = (t[0] + t[1] + t[2]) / 3.0;
        let tp = track.locate_anywhere(c);
        // Inside the channel (banks are at its edges) and facing along the course.
        if !track.contains(tp.seg, c.xz()) || !(phy::RAMP_FIND_EDGE..1.0 - phy::RAMP_FIND_EDGE).contains(&tp.u) {
            continue;
        }
        if (-n.xz()).normalize_or_zero().dot(tp.forward).abs() < phy::RAMP_FIND_ALONG {
            continue;
        }
        let (lo, hi) = (t[0].y.min(t[1].y).min(t[2].y), t[0].y.max(t[1].y).max(t[2].y));
        if hi - tp.water < phy::RAMP_FIND_MIN_RISE || lo - tp.water > phy::RAMP_FIND_MAX_BASE {
            continue;
        }
        // Rock overhead (an arch, an overhang): not something a boat drives up.
        let up = c + n * 2.0;
        if col.hit(up, up + Vec3::Y * phy::RAMP_FIND_HEADROOM).is_some() {
            continue;
        }
        tris.push(Tri { c, n, area, lo, hi });
    }
    // Union-find: neighbours (centres close) facing the same way are one ramp.
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    fn root(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..tris.len() {
        for j in i + 1..tris.len() {
            if tris[i].c.distance(tris[j].c) < phy::RAMP_FIND_JOIN && tris[i].n.dot(tris[j].n) > 0.8 {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..tris.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut ramps: Vec<Ramp> = groups
        .values()
        .map(|g| {
            let area: f32 = g.iter().map(|&i| tris[i].area).sum();
            let centre = g.iter().map(|&i| tris[i].c * tris[i].area).sum::<Vec3>() / area;
            let n = g.iter().map(|&i| tris[i].n * tris[i].area).sum::<Vec3>().normalize();
            let tp = track.locate_anywhere(centre);
            Ramp {
                centre,
                uphill: -n.xz().normalize_or_zero(),
                base: g.iter().map(|&i| tris[i].lo).fold(f32::MAX, f32::min),
                top: g.iter().map(|&i| tris[i].hi).fold(f32::MIN, f32::max),
                progress: tp.progress,
                triangles: g.len(),
                slope: (1.0 - n.y * n.y).max(0.0).sqrt() / n.y.max(1e-3),
            }
        })
        .filter(|r| r.triangles >= phy::RAMP_FIND_MIN_TRIS as usize)
        .collect();
    ramps.sort_by(|a, b| a.progress.total_cmp(&b.progress));
    ramps
}

#[derive(Default)]
pub struct RampTest {
    ramp: Option<Ramp>,
    placed: bool,
    t: f32,
    contacts0: u32,
    clipped: f32,
    launch_vy: f32,
    peak: f32,
    air: f32,
    reported: bool,
    closest: f32,
    lip_speed: f32,
}

/// List ramps (RIPTIDE_RAMPS) and run RIPTIDE_TEST_RAMP. Runs right after the AI picks controls.
pub fn ramp_test(
    time: Res<Time>,
    clock: Res<RaceClock>,
    track: Res<Track>,
    collider: Option<Res<Collider>>,
    mut boats: Query<&mut Boat>,
    mut st: Local<RampTest>,
    mut listed: Local<bool>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(col) = collider else { return };
    if !*listed {
        *listed = true;
        if std::env::var_os("RIPTIDE_RAMPS").is_some() {
            for (i, r) in find(&col, &track).iter().enumerate() {
                info!(
                    "RAMP {i} at {:.0} {:.0} {:.0} rise {:.0} (base {:.0} top {:.0} base_over_water {:.0}) slope {:.2} uphill {:.2} {:.2} progress {:.0} tris {}",
                    r.centre.x, r.centre.y, r.centre.z, r.top - r.base, r.base, r.top, r.base - track.locate_anywhere(r.centre).water, r.slope, r.uphill.x, r.uphill.y, r.progress, r.triangles
                );
            }
        }
    }
    let Some(index) = std::env::var("RIPTIDE_TEST_RAMP").ok().and_then(|s| s.parse::<usize>().ok()) else { return };
    let Some(mut b) = boats.iter_mut().find(|b| b.player) else { return };
    if st.ramp.is_none() {
        st.ramp = find(&col, &track).get(index).cloned();
        if st.ramp.is_none() {
            info!("RAMP TEST {index}: no such ramp");
            exit.write(AppExit::Success);
            return;
        }
    }
    let r = st.ramp.clone().unwrap();
    if clock.t < 0.0 {
        return;
    }
    let dt = time.delta_secs().min(1.0 / 20.0);
    if !st.placed {
        st.placed = true;
        // Back along the course from the ramp, in its lane (a straight line down its slope can
        // start inside the bank), facing the ramp.
        let at = track.locate_anywhere(r.centre);
        let back = (at.progress - phy::RAMP_TEST_RUNUP).max(0.0);
        let seg = track.dist.windows(2).position(|w| back < w[1]).unwrap_or(0);
        let s = ((back - track.dist[seg]) / (track.dist[seg + 1] - track.dist[seg]).max(1e-3)).clamp(0.0, 1.0);
        let p = track.point(seg, s, at.u.clamp(0.15, 0.85));
        b.pos = p;
        // Facing along the course: the autopilot drives the run-up.
        let to = track.locate_anywhere(p).forward;
        b.yaw = (-to.x).atan2(-to.y);
        let water = p.y;
        b.vel = Vec2::ZERO;
        b.vy = 0.0;
        b.airborne = false;
        b.surface = water;
        b.tp = track.locate_anywhere(b.pos);
        st.contacts0 = b.contacts;
        st.closest = f32::MAX;
    }
    // The autopilot follows the course until ramp_test_takeover from the ramp, then the boat
    // heads up its slope (at a point past its centre), flat out all the way.
    let near = (b.pos.xz() - r.centre.xz()).length();
    st.closest = st.closest.min(near);
    if near < phy::RAMP_TEST_TAKEOVER {
        // A point on the ramp's centreline 300 ahead of the boat: steering converges onto the line.
        let along = (b.pos.xz() - r.centre.xz()).dot(r.uphill);
        let aim = r.centre.xz() + r.uphill * (along + 300.0);
        let to = (aim - b.pos.xz()).normalize_or(r.uphill);
        let heading = Vec2::new(-b.yaw.sin(), -b.yaw.cos());
        b.control.steer = (heading.perp_dot(to) * 2.2).clamp(-1.0, 1.0);
    }
    b.control.throttle = 1.0;
    b.control.boost = true;
    b.fuel = b.fuel.max(1.0e3);
    st.t += dt;
    // Clipping: the boat below the ramp surface right under it, while not in the air.
    // Only surface just above the boat: an arch or bridge overhead isn't a ramp being clipped.
    if let Some(floor) = col.floor(b.pos.x, b.pos.z, b.pos.y + phy::RAMP_TEST_CLIP * 4.0) {
        let over = (b.pos.xz() - r.centre.xz()).length() < (r.top - r.base).max(200.0) * 3.0;
        if over && floor > b.pos.y + phy::RAMP_TEST_CLIP {
            st.clipped = st.clipped.max(floor - b.pos.y);
        }
    }
    if b.airborne {
        st.air += dt;
        if st.launch_vy == 0.0 {
            st.launch_vy = b.vy;
        }
        st.peak = st.peak.max(b.pos.y - r.top);
    }
    // Past: further along the course than the ramp by ramp_test_past, back on the water.
    let past = b.tp.progress > r.progress + phy::RAMP_TEST_PAST && b.tp.progress < r.progress + phy::RAMP_TEST_RUNUP * 2.0 && !b.airborne;
    if !b.airborne && b.tp.progress < r.progress {
        st.lip_speed = b.speed;
    }
    if !st.reported && (past || st.t > phy::RAMP_TEST_TIME) {
        st.reported = true;
        info!(
            "RAMP TEST {index}: rise {:.0} slope {:.2} closest {:.0} contacts {} clipped {:.0} launch_vy {:.0} (slope x lip speed {:.0}) peak_over_top {:.0} air {:.2}s speed {:.0} {}",
            r.top - r.base,
            r.slope,
            st.closest,
            b.contacts - st.contacts0,
            st.clipped,
            st.launch_vy,
            r.slope * st.lip_speed,
            st.peak,
            st.air,
            b.speed,
            if past { "passed" } else { "did not get past" }
        );
        exit.write(AppExit::Success);
    }
}

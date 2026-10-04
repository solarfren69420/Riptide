//! H2Overdrive's boat recovery (sdaemon.exe FUN_004bfed0 / FUN_004bfb30 / FUN_004c0060), from
//! its `TritonGame` globals:
//! - **Slow Driver**: slower than `Slow Driver Max MPH` for `Slow Driver Trigger Time`: the boat
//!   is turned to face along the course over `Slow Driver Turnaround Time`, and watched.
//! - **Wrong Way**: heading against the course for `Wrong Way Trigger Time`: turned round over
//!   `Wrong Way Turnaround Time`.
//! - **AntiStuck**: a watched boat that stays within `AntiStuck Radius` (and `AntiStuck Height`)
//!   of where it was for `AntiStuck Check Time`, while its collision meter (the share of the last
//!   64 frames spent touching something) is over `AntiStuck Coll Geiger`, is put back on the
//!   course. Leaving that circle ends the watch.

use crate::cheats::Tuning;
use crate::race::{Boat, Collider, RaceClock};
use crate::sheets::physics as phy;
use crate::track::Track;
use bevy::prelude::*;

#[derive(Component, Default)]
pub struct Recovery {
    /// Wall contacts counted so far, and the last 64 frames as bits (1 = touched something).
    contacts: u32,
    meter: u64,
    slow: f32,
    wrong: f32,
    /// Watched since Slow Driver: where it was, and for how long it has stayed there.
    watch: Option<(Vec3, f32)>,
    /// Turning to face the course: target yaw and seconds left.
    turn: Option<(f32, f32)>,
    pub respawns: u32,
    /// Race distance of the last recovery (a second one close by moves the boat on).
    last_at: Option<f32>,
    /// Furthest race distance reached, and seconds since it last grew by recovery_progress.
    best: f32,
    since: f32,
    /// Recoveries in a row with no race progress in between: each one moves the boat further on
    /// (a boat respawned into the same bad line kept being put back at the same spot).
    chain: u32,
    /// Nearest nav line point last time (the search starts there).
    nav_i: usize,
    /// Just respawned: the next measurement re-bases `best` (not counted as progress).
    rebase: bool,
}

fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// Yaw that faces along the course here (Bevy forward is -Z).
fn course_yaw(forward: Vec2) -> f32 {
    (-forward.x).atan2(-forward.y)
}

pub fn recover(
    time: Res<Time>,
    clock: Res<RaceClock>,
    track: Res<Track>,
    tuning: Res<Tuning>,
    collider: Option<Res<Collider>>,
    nav: Option<Res<crate::nav::NavLine>>,
    mut boats: Query<(&mut Boat, &mut Recovery)>,
) {
    let g = &tuning.0;
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }
    for (mut b, mut r) in &mut boats {
        let touched = b.contacts > r.contacts;
        r.contacts = b.contacts;
        r.meter = (r.meter << 1) | touched as u64;
        let racing = clock.t >= 0.0 && b.finished.is_none() && !b.timed_out;
        // A ramp test (RIPTIDE_TEST_RAMP) aims the player itself.
        let testing = b.player && std::env::var_os("RIPTIDE_TEST_RAMP").is_some();
        if !racing || testing || (b.player && phy::RECOVERY_PLAYER < 0.5) {
            *r = Recovery { contacts: r.contacts, meter: r.meter, respawns: r.respawns, ..default() };
            continue;
        }

        // A turnaround in progress: swing the boat (and where it is going) toward the course.
        if let Some((target, left)) = r.turn {
            let k = (dt / left.max(dt)).min(1.0);
            let turn = wrap(target - b.yaw) * k;
            b.yaw = wrap(b.yaw + turn);
            b.vel = Vec2::from_angle(-turn).rotate(b.vel);
            r.turn = (left > dt).then_some((target, left - dt));
        }

        let forward = b.tp.forward;
        let heading = Vec2::new(-b.yaw.sin(), -b.yaw.cos());
        let going = if b.vel.length() > phy::AI_STUCK_SPEED { b.vel.normalize() } else { heading };
        // Only where the boat really is in the course's corridor does "along the course" mean
        // anything (off it, on a side route, the nearest segment's direction can point anywhere).
        let located = b.tp.branch.is_none() && (0.0..=1.0).contains(&b.tp.s);
        let steady = located && !b.airborne && b.wipeout <= 0.0 && r.turn.is_none();

        // Wrong Way.
        r.wrong = if steady && going.dot(forward) < 0.0 { r.wrong + dt } else { 0.0 };
        if r.wrong >= g.wrong_way_trigger_time {
            r.wrong = 0.0;
            r.turn = Some((course_yaw(forward), g.wrong_way_turnaround_time));
        }

        // Slow Driver.
        let mph = b.vel.length() * g.digital_mph_scale_factor;
        r.slow = if steady && r.watch.is_none() && mph < g.slow_driver_max_mph { r.slow + dt } else { 0.0 };
        if r.slow >= g.slow_driver_trigger_time {
            r.slow = 0.0;
            r.turn = Some((course_yaw(forward), g.slow_driver_turnaround_time));
            r.watch = Some((b.pos, 0.0));
        }

        // AntiStuck.
        if let Some((anchor, t)) = r.watch {
            let away = (b.pos - anchor).xz().length() > g.antistuck_radius || (b.pos.y - anchor.y).abs() > g.antistuck_height;
            if away {
                r.watch = None;
            } else if t + dt >= g.antistuck_check_time {
                r.watch = Some((b.pos, 0.0));
                let geiger = r.meter.count_ones() as f32 / 64.0;
                if geiger > g.antistuck_coll_geiger {
                    recover_boat(&mut b, &mut r, &track, collider.as_deref());
                }
            } else {
                r.watch = Some((anchor, t + dt));
            }
        }

        // No progress at all (circling, wedged in the air): Riptide's backstop to the original rules.
        // Measured along the nav line where there is one: it follows the river, where a course of
        // a few huge cross-sections (Revenge of the Nile) reads a meander as going nowhere.
        let here = match nav.as_deref() {
            Some(n) => {
                r.nav_i = n.nearest(b.pos, r.nav_i);
                b.lap as f32 * n.length() + n.distance_at(r.nav_i)
            }
            None => track.race_distance(b.lap, b.tp.progress),
        };
        if r.rebase {
            r.rebase = false;
            r.best = here;
            r.since = 0.0;
        } else if here > r.best + phy::RECOVERY_PROGRESS {
            r.best = here;
            r.since = 0.0;
            r.chain = 0;
        } else {
            r.since += dt;
        }
        if r.since > phy::RECOVERY_NO_PROGRESS_TIME {
            recover_boat(&mut b, &mut r, &track, collider.as_deref());
        }
    }
}

/// Put the boat back on the course; a second recovery close to the last one moves it on.
fn recover_boat(b: &mut Boat, r: &mut Recovery, track: &Track, col: Option<&Collider>) {
    let here = track.race_distance(b.lap, b.tp.progress);
    let near_last = r.last_at.is_some_and(|d| (here - d).abs() < phy::RESPAWN_SKIP);
    r.chain += 1;
    let skip = if r.chain > 1 { phy::RESPAWN_SKIP * (r.chain - 1) as f32 } else if near_last { phy::RESPAWN_SKIP } else { 0.0 };
    respawn(b, track, col, skip);
    r.last_at = Some(here);
    r.respawns += 1;
    r.turn = None;
    r.watch = None;
    r.meter = 0;
    r.since = 0.0;
    r.rebase = true;
}

/// Back on the course where the boat is, facing along it: the middle of the course, or the
/// clearest lane nearby when something stands there. (The original uses the boat's last path
/// node, FUN_004bf3e0; the start of Riptide's much longer segments threw boats back too far:
/// 13 finishes instead of 18.)
fn respawn(b: &mut Boat, track: &Track, col: Option<&Collider>, skip: f32) {
    let tp = if skip > 0.0 {
        // `skip` further along the course (not past its end).
        let at = (b.tp.progress + skip).min(track.length() - 1.0);
        let seg = track.dist.windows(2).position(|w| at < w[1]).unwrap_or(track.last_seg());
        let s = (at - track.dist[seg]) / (track.dist[seg + 1] - track.dist[seg]).max(1e-3);
        track.locate(track.point(seg, s.clamp(0.0, 1.0), 0.5), seg)
    } else {
        b.tp
    };
    let r = phy::BOAT_RADIUS * b.info.scale.min(1.3);
    let lanes: Vec<Vec3> = [0.5, 0.35, 0.65, 0.2, 0.8].into_iter().map(|u| track.point_at(tp, u)).collect();
    let free = |p: &Vec3| col.is_none_or(|c| !c.blocked(*p, r, tp.forward));
    // Prefer a lane with open water ahead: one that is free but faces a wall repeats the crash.
    let open = |p: &Vec3| {
        col.is_none_or(|c| {
            let eye = *p + Vec3::Y * c.cuts()[0];
            c.hit(eye, eye + Vec3::new(tp.forward.x, 0.0, tp.forward.y) * phy::RESPAWN_CLEAR_AHEAD).is_none()
        })
    };
    let spot = lanes
        .iter()
        .find(|p| free(p) && open(p))
        .or_else(|| lanes.iter().find(|p| free(p)))
        .copied()
        .unwrap_or(lanes[0]);
    if std::env::var_os("RIPTIDE_PROBE").is_some() && b.player {
        info!("RECOVER respawn from {:.0} {:.0} {:.0} to {:.0} {:.0} {:.0} (seg {})", b.pos.x, b.pos.y, b.pos.z, spot.x, spot.y, spot.z, tp.seg);
    }
    b.pos = spot;
    b.vel = Vec2::ZERO;
    b.vy = 0.0;
    b.speed = 0.0;
    b.yaw = course_yaw(tp.forward);
    b.airborne = false;
    b.surface = spot.y;
    b.tp = track.locate(spot, tp.seg);
}

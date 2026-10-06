//! H2Overdrive's floating physics objects (mesh def Physics Type 2: logs, rafts, crates,
//! houseboats, cargo containers): they ride the water and boats push them around. A boat that
//! hits one fast rides up over it and is thrown into the air (many of the original's jumps are
//! off floating logs). Moored boats (Physics Type 1) stay fixed in the static collider.

use crate::race::{Boat, Collider, RaceClock};
use crate::sheets::physics as phy;
use crate::track::Track;
use bevy::prelude::*;

#[derive(Component)]
pub struct Floater {
    /// Horizontal velocity (XZ) and spin (radians per second about Y).
    pub vel: Vec2,
    pub spin: f32,
    /// Footprint radius and height above its waterline (world units).
    pub radius: f32,
    pub height: f32,
    /// `Coll Mass` from the mesh def.
    pub mass: f32,
    /// Height of the mesh origin above the water when placed (keeps its authored draft).
    pub draft: f32,
    pub phase: f32,
    /// Anchored on a bungee (buoys, moored boats, drums: mesh def Bungee Force XZ): springs back here.
    pub anchor: Option<Vec2>,
}

pub(crate) fn float_and_push(
    time: Res<Time>,
    clock: Option<Res<RaceClock>>,
    track: Option<Res<Track>>,
    col: Option<Res<Collider>>,
    mut floaters: Query<(&mut Floater, &mut Transform)>,
    mut boats: Query<&mut Boat>,
) {
    let (Some(track), Some(clock)) = (track, clock) else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }
    let t = time.elapsed_secs();
    for (mut f, mut tf) in &mut floaters {
        // Boats push it: overlap in XZ (footprint vs hull), resolved by mass.
        if clock.t >= 0.0 {
            for mut b in &mut boats {
                let d = tf.translation.xz() - b.pos.xz();
                let reach = f.radius + phy::BOAT_RADIUS * b.info.scale;
                let dist = d.length();
                if dist >= reach || dist < 1e-3 || b.pos.y > tf.translation.y + f.height + phy::FLOATER_CLEAR {
                    continue;
                }
                let n = d / dist;
                let boat_vel = b.vel;
                let closing = (boat_vel - f.vel).dot(n);
                if closing <= 0.0 {
                    continue;
                }
                let boat_mass = phy::FLOATER_BOAT_MASS;
                let share = boat_mass / (boat_mass + f.mass.max(1.0));
                f.vel += n * closing * (1.0 + phy::FLOATER_BOUNCE) * share;
                f.spin += n.perp_dot(boat_vel.normalize_or_zero()) * closing * phy::FLOATER_SPIN / f.radius.max(1.0);
                // Push the floater out of the hull; the boat loses some speed.
                tf.translation += (n * (reach - dist) * share).extend(0.0).xzy();
                // Knockable bungee props barely slow a boat (their Coll Obj Resistance is 0.2).
                let resist = if f.anchor.is_some() { phy::BUNGEE_RESISTANCE } else { 1.0 };
                b.speed *= 1.0 - phy::FLOATER_DRAG_ON_BOAT * (1.0 - share) * resist;
                // Fast enough: the hull rides up over it and is thrown into the air.
                if !b.airborne && closing > phy::FLOATER_LAUNCH_SPEED && f.height > phy::FLOATER_MIN_RAMP_HEIGHT {
                    b.vy = b.vy.max((closing * phy::FLOATER_LAUNCH).min(phy::FLOATER_LAUNCH_MAX));
                    b.airborne = true;
                }
            }
        }
        // Drift, drag, spin; it stays on water (stops at a bank).
        let next = tf.translation.xz() + f.vel * dt;
        let tp = track.locate_anywhere(Vec3::new(next.x, tf.translation.y, next.y));
        let on_land = col.as_ref().is_some_and(|c| c.land(Vec3::new(next.x, tp.water, next.y)));
        if on_land {
            f.vel = Vec2::ZERO;
        } else {
            tf.translation.x = next.x;
            tf.translation.z = next.y;
        }
        if let Some(home) = f.anchor {
            f.vel += (home - tf.translation.xz()) * phy::BUNGEE_SPRING * dt;
        }
        f.vel *= (1.0 - phy::FLOATER_WATER_DRAG * dt).max(0.0);
        f.spin *= (1.0 - phy::FLOATER_WATER_DRAG * dt).max(0.0);
        tf.rotate_y(f.spin * dt);
        let bob = phy::FLOATER_BOB * (t * 1.7 + f.phase).sin();
        tf.translation.y = tp.water + f.draft + bob;
    }
}

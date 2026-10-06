//! H2Overdrive geysers (CGeyser, Wild America): a rocket flame def that erupts on a cycle,
//! Off -> Low (a trickle) -> High (the eruption), and throws a boat over it into the air while
//! erupting. The cycle and effect are the level's data; the launch speed is riptide design
//! (physics.geyser_launch, not decoded from the original yet).

use crate::effects::LevelFire;
use crate::race::{Boat, RaceClock};
use crate::sheets::physics as phy;
use bevy::prelude::*;

#[derive(Component)]
pub struct Geyser {
    pub off: f32,
    pub low: f32,
    pub high: f32,
    pub low_intensity: f32,
    pub high_intensity: f32,
    pub phase: f32,
    pub radius: f32,
}

impl Geyser {
    /// 0 off, 1 low, 2 high, at race time `t`.
    fn state(&self, t: f32) -> u8 {
        let cycle = (self.off + self.low + self.high).max(0.1);
        let x = (t + self.phase).rem_euclid(cycle);
        if x < self.off { 0 } else if x < self.off + self.low { 1 } else { 2 }
    }
}

pub fn geysers(
    clock: Res<RaceClock>,
    mut sfx: crate::sound::Sfx,
    mut q: Query<(&Geyser, &Transform, &mut LevelFire)>,
    mut boats: Query<&mut Boat>,
) {
    for (g, tf, mut fire) in &mut q {
        let state = g.state(clock.t.max(0.0));
        fire.intensity = match state {
            0 => 0.0,
            1 => g.low_intensity,
            _ => g.high_intensity,
        };
        if state != 2 || clock.t < 0.0 {
            continue;
        }
        let reach = g.radius.max(phy::GEYSER_MIN_REACH);
        for mut b in &mut boats {
            if !b.airborne && b.pos.xz().distance(tf.translation.xz()) < reach {
                b.vy = b.vy.max(phy::GEYSER_LAUNCH);
                if std::env::var_os("RIPTIDE_PROBE").is_some() {
                    info!("GEYSER launch player {}", b.player);
                }
                b.airborne = true;
                if b.player {
                    if let Some(d) = crate::sound::Sfx::def_id("wa_GeyserHitBoat") {
                        sfx.def(d, 1.0);
                    }
                }
            }
        }
    }
}

//! H2Overdrive level sounds (`<code>_sounds`): positional loops (CSSound: fires, saws, cranes,
//! full volume inside the inner radius, silent past the outer) and one-shot gates
//! (CSMusicTripwire with a Sound Def: Wild America's Tarzan yell on the final drop), heard by the
//! player's boat.

use crate::race::{Boat, RaceClock};
use crate::sheets::{physics as phy, H2_SOUNDDEFS};
use crate::track::Track;
use bevy::audio::{AudioSink, AudioSinkPlayback, Volume};
use bevy::prelude::*;

#[derive(Component)]
pub struct LevelLoop {
    def: usize,
    inner: f32,
    outer: f32,
    gain: f32,
    playing: Option<Entity>,
}

#[derive(Component)]
pub struct LevelGate {
    def: usize,
    point: Vec2,
    normal: Vec2,
    gain: f32,
    done: bool,
}

/// Spawn the level's sounds (race setup).
pub fn spawn(commands: &mut Commands, level: &riptide_assets::h2level::H2Level, track: &Track, scope: impl Bundle + Clone) -> usize {
    let mut n = 0;
    for s in &level.sounds {
        let Some(def) = crate::sound::Sfx::def_id(&s.sound) else { continue };
        // Music tripwires (`wa_mus2`) switch the score: not one-shots over the race music.
        if s.radii.is_none() && s.sound.to_ascii_lowercase().contains("_mus") {
            continue;
        }
        let at = Vec3::from(s.position);
        match s.radii {
            Some((inner, outer)) => {
                commands.spawn((Transform::from_translation(at), LevelLoop { def, inner, outer: outer.max(inner + 1.0), gain: s.volume, playing: None }, scope.clone()));
            }
            None => {
                let normal = track.locate_anywhere(at).forward;
                commands.spawn((LevelGate { def, point: at.xz(), normal, gain: s.volume, done: false }, scope.clone()));
            }
        }
        n += 1;
    }
    n
}

pub fn level_sounds(
    mut commands: Commands,
    mut sfx: crate::sound::Sfx,
    time: Res<Time>,
    clock: Res<RaceClock>,
    boats: Query<&Boat>,
    mut loops: Query<(&Transform, &mut LevelLoop)>,
    mut gates: Query<&mut LevelGate>,
    mut sinks: Query<&mut AudioSink>,
) {
    let Some(b) = boats.iter().find(|b| b.player) else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let (now, prev) = (b.pos.xz(), b.pos.xz() - b.vel * dt);
    if clock.t >= 0.0 {
        for mut g in &mut gates {
            if g.done {
                continue;
            }
            let (d0, d1) = ((prev - g.point).dot(g.normal), (now - g.point).dot(g.normal));
            let across = (now - g.point).perp_dot(g.normal).abs();
            if d0 < 0.0 && d1 >= 0.0 && across < phy::SOUND_GATE_RADIUS {
                g.done = true;
                sfx.def(g.def, g.gain);
            }
        }
    }
    for (tf, mut l) in &mut loops {
        let d = tf.translation.distance(b.pos);
        if d > l.outer * 1.1 {
            if let Some(e) = l.playing.take().filter(|e| *e != Entity::PLACEHOLDER) {
                commands.entity(e).try_despawn();
            }
            continue;
        }
        let fall = if d <= l.inner { 1.0 } else { (1.0 - (d - l.inner) / (l.outer - l.inner)).clamp(0.0, 1.0) };
        let base = H2_SOUNDDEFS[l.def].volume_2d * l.gain;
        match l.playing {
            // Muted runs get no entity: remember a placeholder so the loop is not restarted every frame.
            None if fall > 0.0 => l.playing = Some(sfx.def_loop(l.def, l.gain * fall, DespawnOnExit(crate::Screen::Race)).unwrap_or(Entity::PLACEHOLDER)),
            Some(e) => {
                if let Ok(mut sink) = sinks.get_mut(e) {
                    sink.set_volume(Volume::Linear(base * fall));
                }
            }
            _ => {}
        }
    }
}

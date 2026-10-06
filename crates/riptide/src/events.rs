//! H2Overdrive scripted level events (`<code>_tripwires`): crossing a tripwire runs its actions
//! after their delays (Quake Canyon's dam collapse: leaks off, eruptions, big splashes; rock
//! slides; tidal waves). Shape 4 is a plane across the course, shape 5 a volume around the
//! point, shape 0 fires only when another tripwire's `Tripwire` action chains to it.
//!
//! Emitters (level fires) that any action turns on start off; those only ever turned off, or
//! never touched, start on (the original's Init Flags string has no traceable reader; this rule
//! fits every chain in the data).

use crate::effects::LevelFire;
use crate::race::{Boat, RaceClock};
use crate::sheets::physics as phy;
use crate::track::Track;
use bevy::prelude::*;
use riptide_assets::h2level::{TripAction, Tripwire};
use std::collections::{HashMap, HashSet};

struct Trip {
    wire: Tripwire,
    /// Plane normal (course direction there) for shape 4.
    normal: Vec2,
    /// Reached only through a chain (shape 0, or another tripwire's target).
    chained: bool,
    done: bool,
}

#[derive(Resource, Default)]
pub struct LevelEvents {
    trips: Vec<Trip>,
    by_name: HashMap<String, usize>,
    fires: HashMap<String, Entity>,
    /// Animated props by name (StartAnim targets).
    props: HashMap<String, Entity>,
    /// Tidal waves by name (TidalWave targets).
    tidals: HashMap<String, riptide_assets::h2level::TidalDef>,
    /// (race time due, action).
    pending: Vec<(f32, TripAction)>,
}

impl LevelEvents {
    pub fn new(wires: &[Tripwire], track: &Track) -> Self {
        let chained: HashSet<String> = wires
            .iter()
            .flat_map(|w| w.actions.iter())
            .filter(|a| a.script.eq_ignore_ascii_case("Tripwire"))
            .map(|a| a.target.clone())
            .collect();
        let trips: Vec<Trip> = wires
            .iter()
            .map(|w| Trip {
                normal: track.locate_anywhere(Vec3::from(w.position)).forward,
                chained: w.shape == 0 || chained.contains(&w.name),
                done: false,
                wire: w.clone(),
            })
            .collect();
        let by_name = trips.iter().enumerate().map(|(i, t)| (t.wire.name.clone(), i)).collect();
        Self { trips, by_name, ..default() }
    }

    /// Fires some action turns on: they start off.
    pub fn starts_off(wires: &[Tripwire]) -> HashSet<String> {
        wires
            .iter()
            .flat_map(|w| w.actions.iter())
            .filter(|a| a.script.eq_ignore_ascii_case("Fire") && a.int != 0 && !a.target.is_empty())
            .map(|a| a.target.clone())
            .collect()
    }

    pub fn add_fire(&mut self, name: &str, e: Entity) {
        self.fires.insert(name.to_string(), e);
    }

    pub fn add_tidals(&mut self, defs: &[riptide_assets::h2level::TidalDef]) {
        self.tidals = defs.iter().map(|d| (d.name.clone(), d.clone())).collect();
    }

    pub fn add_prop(&mut self, name: &str, e: Entity) {
        self.props.insert(name.to_string(), e);
    }

    fn fire(&mut self, i: usize, now: f32) {
        let t = &mut self.trips[i];
        if t.done {
            return;
        }
        t.done = true;
        if std::env::var_os("RIPTIDE_DEBUG").is_some() {
            info!("event: tripwire {} at {now:.1}s", t.wire.name);
        }
        for a in t.wire.actions.clone() {
            self.pending.push((now + a.delay.max(0.0), a));
        }
    }
}

pub fn run_events(
    time: Res<Time>,
    clock: Res<RaceClock>,
    events: Option<ResMut<LevelEvents>>,
    boats: Query<&Boat>,
    mut fires: Query<&mut LevelFire>,
    mut skies: Query<(&crate::race::SkyIndex, &mut Visibility)>,
    mut shake: ResMut<crate::race::CameraShake>,
    mut rigs: Query<&mut crate::boatrig::PropRig>,
    mut tides: ResMut<crate::h2water::Tides>,
) {
    let Some(mut ev) = events else { return };
    if clock.t < 0.0 {
        return;
    }
    let Some(b) = boats.iter().find(|b| b.player) else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let (now, prev) = (b.pos.xz(), b.pos.xz() - b.vel * dt);
    // Physical triggers.
    let mut hit = Vec::new();
    for (i, t) in ev.trips.iter().enumerate() {
        if t.done || t.chained {
            continue;
        }
        let p = Vec2::new(t.wire.position[0], t.wire.position[2]);
        let crossed = match t.wire.shape {
            4 => {
                let (d0, d1) = ((prev - p).dot(t.normal), (now - p).dot(t.normal));
                d0 < 0.0 && d1 >= 0.0 && (now - p).perp_dot(t.normal).abs() < phy::TRIP_GATE_RADIUS
            }
            5 => now.distance(p) < (t.wire.scale * phy::TRIP_VOLUME_SCALE).max(phy::TRIP_VOLUME_MIN),
            _ => false,
        };
        if crossed {
            hit.push(i);
        }
    }
    for i in hit {
        ev.fire(i, clock.t);
    }
    // Due actions (a chained Tripwire queues more).
    loop {
        let Some(k) = ev.pending.iter().position(|(due, _)| *due <= clock.t) else { break };
        let (_, a) = ev.pending.swap_remove(k);
        match a.script.to_ascii_lowercase().as_str() {
            "fire" => {
                if let Some(mut f) = ev.fires.get(&a.target).and_then(|e| fires.get_mut(*e).ok()) {
                    f.intensity = if a.int != 0 { 1.0 } else { 0.0 };
                }
            }
            "skybox" => {
                // An index into the level's skyboxes; 999 and up hide the sky (inside the temple).
                let n = skies.iter().count();
                let show = if a.int >= 999 || n == 0 { None } else { Some((a.int.max(0) as usize).min(n - 1)) };
                for (i, mut v) in &mut skies {
                    *v = if Some(i.0) == show { Visibility::Inherited } else { Visibility::Hidden };
                }
            }
            "tidalwave" => {
                if let Some(d) = ev.tidals.get(&a.target) {
                    tides.0.push(crate::h2water::Tide {
                        epicentre: Vec3::from(d.position),
                        length: d.length,
                        speed: d.speed,
                        height: d.height,
                        dist_start: d.dist_start,
                        dist_max: d.dist_max,
                        dist_fade: d.dist_fade,
                        started: clock.t,
                    });
                }
            }
            "startanim" => {
                if let Some(mut r) = ev.props.get(&a.target).and_then(|e| rigs.get_mut(*e).ok()) {
                    r.held = false;
                }
            }
            "camerashake" => {
                shake.amp = (a.int as f32 * phy::CAM_SHAKE_PER_UNIT).min(phy::CAM_SHAKE_MAX);
                shake.start = clock.t;
                shake.until = clock.t + a.float.max(0.1);
            }
            "tripwire" => {
                if let Some(&i) = ev.by_name.get(&a.target) {
                    ev.fire(i, clock.t);
                }
            }
            other => {
                if std::env::var_os("RIPTIDE_DEBUG").is_some() {
                    info!("event: {other} {} (not run yet)", a.target);
                }
            }
        }
    }
}

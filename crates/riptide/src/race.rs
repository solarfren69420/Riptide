//! The race: level spawn, boat physics, AI, pickups, chase camera and HUD.

use crate::cheats::{Cheats, Tuning};
use crate::content::{attach, BoatInfo, CourseSource, Models};
use crate::controls::Input;
use crate::sheets::{controls_ids as ctl, physics as phy, CheatsEffect, TracksAiLine, TracksCollision, CHECKPOINTS, HackworldKind, HACKWORLD, H2_GLOBALS, H2_LEVELS, H2_TRIPWIRES, PICKUPS, TRACKS};
use crate::track::{Track, TrackPos};
use crate::{Autopilot, Screen, Selection};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::asset::RenderAssetUsages;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use riptide_assets::h2level::{load_level, BoostKind, Edge, H2Level, Quad};

/// The `pickups` row for an H2Overdrive `CBooster` type.
fn pickup_row(kind: BoostKind) -> Option<usize> {
    let ty = match kind {
        BoostKind::Blue => 0,
        BoostKind::Red => 1,
        BoostKind::Gold => 2,
    };
    PICKUPS.iter().position(|p| p.engine_type == ty && p.status.is_ok())
}

pub struct RacePlugin;

impl Plugin for RacePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CamView>().init_resource::<CameraShake>().init_resource::<crate::h2water::Tides>();
        app.add_systems(Update, probe.run_if(in_state(Screen::Race)))
            .add_systems(OnEnter(Screen::Race), (spawn_race, choose_collision, start_race_audio, crate::net::race_ready).chain())
            .add_systems(
                Update,
                (
                    player_input,
                    ai_drive,
                    crate::ramps::ramp_test,
                    boat_physics,
                    boat_contacts,
                    crate::floating::float_and_push,
                    crate::recovery::recover,
                    crate::net::send_state,
                    crate::net::apply_remote,
                    pickups,
                    race_clock,
                    arcade_timer,
                    place_boats,
                    chase_camera,
                    sky_follow,
                    hud,
                    race_keys,
                    water_flow,
                    move_props,
                    crate::levelsound::level_sounds,
                )
                    .chain()
                    .run_if(in_state(Screen::Race)),
            );
        app.add_systems(Update, (race_audio, engine_audio, crate::sound::fade_music, crate::geyser::geysers, crate::events::run_events, crate::h2water::apply_tides, crate::htmotion::animate).run_if(in_state(Screen::Race)));
    }
}

// World units are H2Overdrive's (~10 cm). Every tunable comes from the sheets: `phy::*` (Riptide's
// own constants), the boat's `h2_boatdefs` row, and H2Overdrive's globals through `Tuning`.

/// Hull Crusher phases, timed by the `HullCrush Time *` globals.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Crush {
    #[default]
    Off,
    Deploy(f32),
    Active(f32),
    Stow(f32),
}

impl Crush {
    /// Hull fully out: it smashes whatever it touches.
    pub fn smashing(&self) -> bool {
        matches!(self, Crush::Active(_))
    }
    /// 0..1 how far the hull is grown, for the visual scale.
    pub fn grown(&self, t: &Tuning) -> f32 {
        let g = &t.0;
        match *self {
            Crush::Off => 0.0,
            Crush::Deploy(left) => 1.0 - left / g.hullcrush_time_deploy.max(1e-3),
            Crush::Active(_) => 1.0,
            Crush::Stow(left) => left / g.hullcrush_time_stow.max(1e-3),
        }
    }
}

#[derive(Component)]
pub struct Boat {
    pub info: BoatInfo,
    pub player: bool,
    pub pos: Vec3,
    pub vel: Vec2,
    pub vy: f32,
    pub yaw: f32,
    pub speed: f32,
    pub fuel: f32,
    pub super_time: f32,
    pub boosting: bool,
    pub tp: TrackPos,
    pub lap: u32,
    pub finished: Option<f32>,
    /// Ran out of arcade time (out of the race, like finishing but unplaced).
    pub timed_out: bool,
    pub control: Control,
    pub brain: Brain,
    /// Visual lean, for smoothing.
    pub roll: f32,
    pub pitch: f32,
    pub airborne: bool,
    /// Seconds left of a wipeout (no control).
    pub wipeout: f32,
    pub crush: Crush,
    /// Height of the surface ridden last frame (water or a terrain floor).
    pub surface: f32,
    /// 0..1 H2Overdrive catch-up help (AI boats behind the player).
    pub catchup: f32,
    /// Running counts for sound cues: boats blasted by this one's Hull Crusher, hard wall hits.
    pub smashes: u32,
    /// Boats this one has rammed while boosting (crate::race::boat_contacts).
    pub rams: u32,
    pub wall_hits: u32,
    /// Boost / gold boost pickups collected (sound cues: heard even with a full tank).
    pub pickups: u32,
    pub super_pickups: u32,
    /// Frames this boat was pushed out of a wall (collision check, RIPTIDE_PROBE).
    pub contacts: u32,
    /// Jumps since leaving the water, and seconds since the last one.
    pub jumps: u32,
    pub jump_t: f32,
    /// Smoothed rate the surface under the boat rises (ramp launches).
    pub climb_rate: f32,
}

#[derive(Default, Clone, Copy)]
pub struct Control {
    pub throttle: f32,
    pub steer: f32,
    pub boost: bool,
    /// Brake pressed this frame / held (boost + brake = jump).
    pub jump: bool,
    pub brake_held: bool,
}

#[derive(Clone, Copy)]
pub struct Brain {
    pub lane: f32,
    pub lane_target: f32,
    pub skill: f32,
    pub lane_timer: f32,
    /// Seconds spent nearly stopped (racing), and seconds left of backing off.
    pub stuck: f32,
    pub reverse: f32,
    /// Last nearest point on the navigation line (usize::MAX: not looked up yet).
    pub nav: usize,
    /// Wall contacts seen so far, and seconds left of steering clear after the last one.
    pub contacts: u32,
    pub scrape: f32,
}

#[derive(Component)]
struct Pickup {
    /// Row in the `pickups` sheet.
    row: usize,
    /// Boats that took it and seconds until it is back for them: every racer gets every boost
    /// (nobody can take one away from the others).
    taken: Vec<(Entity, f32)>,
    base: Vec3,
}

#[derive(Component)]
pub struct ChaseCam;

#[derive(Component)]
pub struct Sky;

/// Which of the level's skyboxes this dome is (Skybox event actions switch between them).
#[derive(Component)]
pub struct SkyIndex(pub usize);

/// A camera shake from a level event: amplitude (units) fading out by `until` (race time).
#[derive(Resource, Default)]
pub struct CameraShake {
    pub amp: f32,
    pub start: f32,
    pub until: f32,
}

#[derive(Component)]
enum HudText {
    Position,
    Timer,
    Speed,
    Center,
    Boat,
}

#[derive(Default)]
struct SuperNotice {
    last: f32,
    until: f32,
}

#[derive(Component)]
struct BoostBar;

/// Moves a prop along its controller's point chain at constant speed.
#[derive(Component)]
struct PathMover {
    points: Vec<Vec3>,
    cum: Vec<f32>,
    speed: f32,
    looped: bool,
    align: bool,
    d: f32,
}

impl PathMover {
    fn new(path: &riptide_assets::h2level::MotionPath, at: Vec3, _scale: f32) -> Self {
        let mut points: Vec<Vec3> = path.points.iter().map(|p| Vec3::from(*p)).collect();
        if path.looped {
            points.push(points[0]);
        }
        let mut cum = vec![0.0];
        for w in points.windows(2) {
            cum.push(cum.last().unwrap() + w[0].distance(w[1]));
        }
        // Start where the object was authored: the closest point on the chain.
        let mut best = (f32::MAX, 0.0);
        for (i, w) in points.windows(2).enumerate() {
            let seg = w[1] - w[0];
            let t = ((at - w[0]).dot(seg) / seg.length_squared().max(1e-3)).clamp(0.0, 1.0);
            let d2 = (w[0] + seg * t).distance_squared(at);
            if d2 < best.0 {
                best = (d2, cum[i] + seg.length() * t);
            }
        }
        Self { points, cum, speed: path.speed, looped: path.looped, align: path.align, d: best.1 }
    }

    fn sample(&self, d: f32) -> (Vec3, Vec3) {
        let total = *self.cum.last().unwrap();
        let d = if total > 0.0 { d.rem_euclid(total) } else { 0.0 };
        let i = self.cum.partition_point(|&c| c <= d).clamp(1, self.points.len() - 1) - 1;
        let span = (self.cum[i + 1] - self.cum[i]).max(1e-3);
        let dir = (self.points[i + 1] - self.points[i]).normalize_or(Vec3::NEG_Z);
        (self.points[i].lerp(self.points[i + 1], (d - self.cum[i]) / span), dir)
    }
}

/// A level prop turned and/or slid by its controllers (`Spin`, `Slide`) about where it was placed.
#[derive(Component)]
struct Motion {
    base: Transform,
    spin: Option<riptide_assets::h2level::Spin>,
    slide: Option<riptide_assets::h2level::Slide>,
}

fn move_props(time: Res<Time>, mut q: Query<(&mut PathMover, &mut Transform)>, mut motions: Query<(&Motion, &mut Transform), Without<PathMover>>) {
    let t = time.elapsed_secs();
    for (m, mut tf) in &mut motions {
        let mut out = m.base;
        if let Some(s) = m.slide {
            // 0 -> distance -> 0 once per round trip; eased (cosine) or at constant speed.
            let f = (s.phase + t * s.round_trips_per_second).rem_euclid(1.0);
            let k = if s.eased { 0.5 - 0.5 * (f * std::f32::consts::TAU).cos() } else { 1.0 - (2.0 * f - 1.0).abs() };
            out.translation += m.base.rotation * (Vec3::from(s.axis) * s.distance * k);
        }
        if let Some(s) = m.spin {
            let a = (s.phase + t * s.revs_per_second) * std::f32::consts::TAU;
            out.rotation = m.base.rotation * Quat::from_axis_angle(Vec3::from(s.axis), a);
        }
        *tf = out;
    }

    let dt = time.delta_secs().min(1.0 / 20.0);
    for (mut m, mut tf) in &mut q {
        m.d += m.speed * dt;
        if !m.looped && m.d > *m.cum.last().unwrap() {
            m.d = 0.0;
        }
        let (p, dir) = m.sample(m.d);
        tf.translation = p;
        if m.align {
            let flat = Vec3::new(dir.x, dir.y, dir.z);
            tf.look_to(flat, Vec3::Y);
        }
    }
}

#[derive(Resource)]
struct WaterMaterial {
    /// The water (crate::water: waves, ripples and foam run on the GPU).
    material: Handle<crate::water::WaterMat>,
    /// Animation frames (Hydro Thunder); empty = scroll the one texture instead.
    frames: Vec<Handle<Image>>,
}

fn water_flow(time: Res<Time>, water: Option<Res<WaterMaterial>>, mut materials: ResMut<Assets<crate::water::WaterMat>>) {
    let Some(w) = water else { return };
    if let Some(mat) = materials.get_mut(&w.material) {
        let t = time.elapsed_secs();
        // The shader moves the waves and ripples from this clock.
        mat.extension.params.time = t;
        let m = &mut mat.base;
        if w.frames.is_empty() {
            m.uv_transform = bevy::math::Affine2::from_translation(Vec2::new(t * 0.013, t * 0.031));
        } else {
            let f = (t * phy::HT_WATER_FPS) as usize % w.frames.len();
            if m.base_color_texture.as_ref() != Some(&w.frames[f]) {
                m.base_color_texture = Some(w.frames[f].clone());
            }
        }
    }
}

#[derive(Resource)]
pub struct RaceClock {
    pub t: f32,
    pub finish_order: Vec<Entity>,
}

fn spawn_race(
    mut commands: Commands,
    mut models: Models,
    sel: Res<Selection>,
    online: Res<crate::net::Online>,
    mut fog_color: Local<Option<Color>>,
) {
    let Some(choice) = models.content.tracks.get(sel.level).cloned() else { return };
    commands.remove_resource::<Collider>();
    let code = choice.id.to_string();
    // H2Overdrive levels load from triton.lux; a Hydro Thunder course becomes an H2Level holding
    // only its racing line (scaled into H2Overdrive units), plus the decoded course itself.
    let (level, ht_course) = match &choice.source {
        CourseSource::H2(lvl) => match load_level(&models.content.lux, lvl) {
            Ok(mut l) => {
                l.title = choice.name.clone();
                (l, None)
            }
            Err(e) => {
                error!("level {lvl}: {e:#}");
                return;
            }
        },
        // Hackworld: a ring for the AI and a sea of water; the base level lends its sky.
        CourseSource::Sandbox { base } => {
            let skyboxes = load_level(&models.content.lux, base).map(|l| l.skyboxes).unwrap_or_default();
            let (r, w, n) = (phy::HACKWORLD_RING_RADIUS, phy::HACKWORLD_RING_WIDTH, 72);
            let path = (0..n)
                .map(|i| {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    let (c, s) = (a.cos(), a.sin());
                    Edge { start: [c * (r - w * 0.5), 0.0, s * (r - w * 0.5)], end: [c * (r + w * 0.5), 0.0, s * (r + w * 0.5)], water: 0.0 }
                })
                .collect();
            let (h, tiles) = (phy::HACKWORLD_WATER_HALF, 12);
            let step = 2.0 * h / tiles as f32;
            let mut water = Vec::new();
            for j in 0..tiles {
                for i in 0..tiles {
                    let (x0, z0) = (-h + i as f32 * step, -h + j as f32 * step);
                    let (x1, z1) = (x0 + step, z0 + step);
                    water.push(Quad { corners: [[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]] });
                }
            }
            (H2Level { code: code.clone(), title: choice.name.clone(), path, water, skyboxes, ..Default::default() }, None)
        }
        CourseSource::Ht { file, entry, laps, .. } => {
            let Some(ht) = models.content.ht.clone() else { return };
            models.cache.forget_ht();
            match ht.load_track(file, entry) {
                Ok(t) => {
                    let s = phy::HT_WORLD_SCALE;
                    let sc = |p: [f32; 3]| [p[0] * s, p[1] * s, p[2] * s];
                    let path = t
                        .path
                        .iter()
                        .map(|e| riptide_assets::h2level::Edge { start: sc(e.start), end: sc(e.end), water: e.water * s })
                        .collect();
                    let mut level = H2Level { code: code.clone(), title: choice.name.clone(), path, ..Default::default() };
                    // The AI path excludes shortcuts. Every original river sector needs a surface.
                    let mut river = Vec::new();
                    for [a, b] in &t.river {
                        let drop = (a.water - b.water) * s > phy::WATERFALL_DROP;
                        let lift = |p: [f32; 3], y: f32| [p[0] * s, y * s, p[2] * s];
                        level.water.push(Quad { corners: [lift(a.start, a.water), lift(a.end, a.water), lift(b.end, if drop { a.water } else { b.water }), lift(b.start, if drop { a.water } else { b.water })] });
                        let bw = if drop { a.water } else { b.water };
                        river.push((sc(a.start), sc(a.end), a.water * s, sc(b.start), sc(b.end), bw * s));
                        if drop {
                            level.waterfalls.push(Quad { corners: [lift(b.start, a.water), lift(b.end, a.water), sc(b.end), sc(b.start)] });
                        }
                    }
                    crate::h2water::edges_from_river(&mut level, &river);
                    (level, Some((t, *laps)))
                }
                Err(e) => {
                    error!("track {file}/{entry}: {e:#}");
                    return;
                }
            }
        }
    };
    if level.path.len() < 3 {
        error!("level {code} has no racing line");
        return;
    }
    let mut track = Track::new(level.path.clone());
    if let Some(f) = level.finish {
        let here = track.locate_anywhere(Vec3::from(f));
        track.finish = Some((Vec2::new(f[0], f[2]), here.forward, here.progress));
        // The line between the two buoys farthest apart (the gate's posts), across the course.
        let bs: Vec<Vec2> = level.finish_buoys.iter().map(|b| Vec2::new(b[0], b[2])).collect();
        let mut best: Option<(Vec2, Vec2)> = None;
        for (i, a) in bs.iter().enumerate() {
            for b in &bs[i + 1..] {
                if best.is_none_or(|(p, q)| a.distance(*b) > p.distance(q)) {
                    best = Some((*a, *b));
                }
            }
        }
        track.finish_line = best.filter(|(a, b)| a.distance(*b) > phy::BOAT_RADIUS);
    } else if !track.edges.is_empty() {
        // No finish buoys (Hydro Thunder, some H2 levels): the course's last cross-section.
        let n = track.edges.len();
        let mid = |e: &riptide_assets::h2level::Edge| Vec2::new((e.start[0] + e.end[0]) * 0.5, (e.start[2] + e.end[2]) * 0.5);
        let (a, b) = (mid(&track.edges[n.saturating_sub(2)]), mid(&track.edges[n - 1]));
        track.finish = Some((b, (b - a).normalize_or(Vec2::NEG_Y), track.length()));
        // Hydro Thunder's decoded line runs right into the end wall (Practice ends in a
        // narrowing corner a boat can't reach): finish `ht_finish_back` short of it.
        if ht_course.is_some() && n >= 2 {
            let at = (track.length() - phy::HT_FINISH_BACK).max(0.0);
            let seg = track.dist.windows(2).position(|w| at < w[1]).unwrap_or(n - 2);
            let span = (track.dist[seg + 1] - track.dist[seg]).max(1e-3);
            let p = track.point(seg, ((at - track.dist[seg]) / span).clamp(0.0, 1.0), 0.5).xz();
            let dir = (mid(&track.edges[seg + 1]) - mid(&track.edges[seg])).normalize_or(Vec2::NEG_Y);
            track.finish = Some((p, dir, at));
        }
    }
    match (&choice.source, &ht_course) {
        // Laps come from the level's CLevelInfo (0 = point to point).
        (CourseSource::Sandbox { .. }, _) => {
            track.looped = true;
            track.laps = phy::HACKWORLD_LAPS as u32;
            track.open = true;
        }
        (CourseSource::H2(lvl), _) => {
            // A racing-line drop is a sloped chute when the level's own water is already partway down
            // halfway along it (Hong Kong's flumes: holding the upper level floated boats over the chute
            // walls into the collision mesh, where the AI kept respawning). Where the drawn water is
            // still up there it steps down at the end: a fall to fly off (Temple of Flume).
            let drawn = |p: Vec2| -> Option<f32> {
                let v = |q: [f32; 3]| Vec2::new(q[0], q[2]);
                level.water_sectors.iter().find_map(|ws| {
                    let (a, b) = (level.water_edges.get(ws.leading)?, level.water_edges.get(ws.trailing)?);
                    let quad = [v(a.start), v(a.end), v(b.end), v(b.start)];
                    let side = |i: usize| (quad[(i + 1) % 4] - quad[i]).perp_dot(p - quad[i]);
                    let s: Vec<f32> = (0..4).map(side).collect();
                    let inside = s.iter().all(|x| *x >= 0.0) || s.iter().all(|x| *x <= 0.0);
                    if !inside {
                        return None;
                    }
                    let (ma, mb) = ((quad[0] + quad[1]) * 0.5, (quad[2] + quad[3]) * 0.5);
                    let f = ((p - ma).dot(mb - ma) / (mb - ma).length_squared().max(1.0)).clamp(0.0, 1.0);
                    Some(a.water + (b.water - a.water) * f)
                })
            };
            let mid = |e: &Edge| Vec2::new((e.start[0] + e.end[0]) * 0.5, (e.start[2] + e.end[2]) * 0.5);
            track.chutes = track
                .edges
                .windows(2)
                .map(|w| {
                    let drop = w[0].water - w[1].water;
                    drop > phy::WATERFALL_DROP
                        && drawn((mid(&w[0]) + mid(&w[1])) * 0.5).is_some_and(|h| w[0].water - h > drop * phy::CHUTE_HALF_WAY_FRACTION)
                })
                .collect();
            let chutes = track.chutes.iter().filter(|c| **c).count();
            if chutes > 0 {
                info!("{code}: {chutes} sloped chutes");
            }
            if let Some(row) = H2_LEVELS.iter().find(|l| l.id == *lvl) {
                track.looped = row.num_laps > 0;
                track.laps = row.num_laps.max(1) as u32;
            }
        }
        (_, Some((t, laps))) => {
            track.looped = t.looped;
            track.gravity = Some(phy::HT_GRAVITY);
            track.laps = (*laps).max(1);
            let s = phy::HT_WORLD_SCALE;
            track.starts = t.starts.iter().map(|(p, yaw)| (Vec3::from(*p) * s, *yaw)).collect();
            track.lanes = t.lanes.clone();
            track.branches = t.river.iter().filter(|[a, b]| !t.path.windows(2).any(|w| w[0].start == a.start && w[1].start == b.start))
                .map(|[a, b]| [*a, *b].map(|e| Edge { start: e.start.map(|v| v * s), end: e.end.map(|v| v * s), water: e.water * s })).collect();
        }
        _ => {}
    }
    let turned = track.orient();
    if turned > 0 {
        info!("{code}: turned {turned} reversed cross-sections");
    }
    // No finish buoys (Hydro Thunder, Reverse Sydney/Tundra, Down Underdrive): the line is the
    // cross-section at the finish point, so those races also end the instant a nose touches it.
    if let (false, None, Some((p, dir, _))) = (track.looped, track.finish_line, track.finish) {
        let at = track.locate_anywhere(Vec3::new(p.x, 0.0, p.y));
        let l = track.point_at(at, 0.0).xz();
        let r = track.point_at(at, 1.0).xz();
        let half = (l.distance(r) * 0.5).max(phy::BOAT_RADIUS);
        let across = dir.perp().normalize_or(Vec2::X);
        track.finish_line = Some((p - across * half, p + across * half));
    }
    info!(
        "{code}: {} sectors, {} props, {} boosters, track {:.0} units",
        level.sector_meshes.len(),
        level.props.len(),
        level.boosters.len(),
        track.length()
    );
    let scope = DespawnOnExit(Screen::Race);

    let mut floater_sizes: std::collections::HashMap<usize, (f32, f32)> = Default::default();
    // Terrain.
    let world = commands.spawn((Transform::IDENTITY, Visibility::default(), scope.clone(), Name::new("terrain"))).id();
    // H2Overdrive's own world collision mesh (`coll4.wc_<level>`), Z mirrored like everything else.
    if let CourseSource::H2(lvl) = &choice.source {
        let tris = models.content.lux.get(&format!("coll4.wc_{lvl}")).and_then(|b| riptide_assets::h2coll::decode_collision(b).ok()).unwrap_or_default();
        if !tris.is_empty() {
            let part = riptide_assets::model::MeshPart {
                positions: tris.iter().flat_map(|t| t.iter().map(|v| [v[0], v[1], -v[2]])).collect(),
                indices: (0..tris.len() as u32 * 3).collect(),
                ..Default::default()
            };
            let mut model = riptide_assets::model::Model { name: format!("wc_{lvl}"), parts: vec![part], ..Default::default() };
            // Physics objects (floating logs, rafts, crates, moored boats) are solid: boats hit them
            // and ride over them, which is where many of the original's jumps come from. They
            // stay where they were placed (the original floats and pushes them).
            let mut solid = 0;
            for (i, prop) in level.props.iter().enumerate().filter(|(_, p)| p.solid) {
                let Some(m) = models.content.lux.get(&format!("mesh32.{}", prop.mesh)).and_then(|b| riptide_assets::h2mesh::decode_mesh(&prop.mesh, b).ok()) else { continue };
                // Floating ones (Physics Type 2) move: crate::floating, not the static collider.
                if prop.float_mass.is_some() {
                    let s = prop.scale.max(0.01);
                    let (mut lo, mut hi) = (Vec3::MAX, Vec3::MIN);
                    for part in &m.parts {
                        for p in &part.positions {
                            lo = lo.min(Vec3::from(*p));
                            hi = hi.max(Vec3::from(*p));
                        }
                    }
                    let half = ((hi - lo).xz() * 0.5 * s).max(Vec2::splat(1.0));
                    floater_sizes.insert(i, (half.x.max(half.y).min(phy::FLOATER_MAX_RADIUS), (hi.y * s).max(1.0)));
                    continue;
                }
                let place = Transform::from_translation(Vec3::from(prop.position)).with_rotation(Quat::from_array(prop.rotation).normalize()).with_scale(Vec3::splat(prop.scale.max(0.01)));
                for mut part in m.parts {
                    for p in &mut part.positions {
                        *p = place.transform_point(Vec3::from(*p)).to_array();
                    }
                    model.parts.push(part);
                }
                solid += 1;
            }
            if solid > 0 {
                info!("{lvl}: {solid} solid physics objects");
            }
            commands.insert_resource(Collider { trusted: true, ..Collider::build(std::slice::from_ref(&model), 1.0, phy::WALL_STEEPNESS, true) });
        }
    }
    for s in &level.sector_meshes {
        if let Some(p) = models.lux(s) {
            commands.entity(world).with_children(|c| {
                for piece in p.iter() {
                    { let mut e = c.spawn((Mesh3d(piece.mesh.clone()), NotShadowCaster)); piece.apply(&mut e); }
                }
            });
        }
    }
    // Spray takes the water's whitewash colour (crate::effects::SprayTint).
    if !level.water_edges.is_empty() {
        let n = level.water_edges.len() as f32;
        let c = level.water_edges.iter().map(|e| Vec3::new(e.whitewash_color[0], e.whitewash_color[1], e.whitewash_color[2])).sum::<Vec3>() / n;
        commands.insert_resource(crate::effects::SprayTint(c));
    }
    // Level sounds: positional loops and one-shot gates (`<code>_sounds`).
    let sounds = crate::levelsound::spawn(&mut commands, &level, &track, scope.clone());
    if sounds > 0 {
        info!("{code}: {sounds} level sounds");
    }
    // Level fires, smoke, torches, leaks and splashes (`<code>_Fire`): rocket flame defs at fixed spots.
    let mut fires = 0;
    let mut events = crate::events::LevelEvents::new(&level.tripwires, &track);
    let off = crate::events::LevelEvents::starts_off(&level.tripwires);
    events.add_tidals(&level.tidals);
    commands.insert_resource(crate::h2water::Tides::default());
    for f in &level.fires {
        let Some(fire) = crate::effects::LevelFire::new(&f.def, f.scale) else { continue };
        let mut fire = fire;
        if off.contains(&f.name) {
            fire.intensity = 0.0;
        }
        let e = commands.spawn((Transform::from_translation(Vec3::from(f.position)).with_rotation(Quat::from_array(f.rotation).normalize()), fire, scope.clone())).id();
        events.add_fire(&f.name, e);
        fires += 1;
    }
    // Geysers: a flame def driven by its cycle (crate::geyser).
    for (k, g) in level.geysers.iter().enumerate() {
        let Some(mut fire) = crate::effects::LevelFire::new(&g.def, 1.0) else { continue };
        fire.intensity = 0.0;
        let phase = if g.random_phase { (k as f32 * 2.399).rem_euclid(g.off + g.low + g.high) } else { g.phase };
        commands.spawn((
            Transform::from_translation(Vec3::from(g.position)).with_rotation(Quat::from_array(g.rotation).normalize()),
            fire,
            crate::geyser::Geyser { off: g.off, low: g.low, high: g.high, low_intensity: g.low_intensity, high_intensity: g.high_intensity, phase, radius: g.radius },
            scope.clone(),
        ));
        fires += 1;
    }
    if fires > 0 {
        info!("{code}: {fires} level fires / smoke / splashes");
    }
    // Props.
    let mut animated = 0;
    let mut moving = 0;
    for (i, prop) in level.props.iter().enumerate() {
        let Some(p) = models.lux(&prop.mesh) else { continue };
        let mover = prop.path.as_ref().map(|path| PathMover::new(path, Vec3::from(prop.position), prop.scale));
        let e = commands
            .spawn((
                Transform::from_translation(Vec3::from(prop.position))
                    .with_rotation(Quat::from_array(prop.rotation).normalize())
                    .with_scale(Vec3::splat(prop.scale.max(0.01))),
                Visibility::default(),
                scope.clone(),
            ))
            .id();
        // Knockable props on a bungee near the water: shoved aside when hit, springing back.
        if prop.bungee.is_some() && prop.path.is_none() && prop.spin.is_none() && prop.slide.is_none() && prop.anim.is_none() {
            let water = track.locate_anywhere(Vec3::from(prop.position)).water;
            if (prop.position[1] - water).abs() < phy::BUNGEE_MAX_ABOVE_WATER {
                commands.entity(e).insert(crate::floating::Floater {
                    vel: Vec2::ZERO,
                    spin: 0.0,
                    radius: phy::BUNGEE_RADIUS * prop.scale.max(0.1),
                    height: 1.0,
                    mass: phy::BUNGEE_MASS,
                    draft: prop.position[1] - water,
                    phase: i as f32 * 1.37,
                    anchor: Some(Vec2::new(prop.position[0], prop.position[2])),
                });
            }
        }
        if let (Some(mass), Some(&(radius, height))) = (prop.float_mass, floater_sizes.get(&i)) {
            let water = track.locate_anywhere(Vec3::from(prop.position)).water;
            commands.entity(e).insert(crate::floating::Floater {
                vel: Vec2::ZERO,
                spin: 0.0,
                radius,
                height,
                mass,
                draft: prop.position[1] - water,
                phase: i as f32 * 1.37,
                anchor: None,
            });
        }
        if let Some(m) = mover {
            commands.entity(e).insert(m);
        } else if prop.spin.is_some() || prop.slide.is_some() {
            let base = Transform::from_translation(Vec3::from(prop.position))
                .with_rotation(Quat::from_array(prop.rotation).normalize())
                .with_scale(Vec3::splat(prop.scale.max(0.01)));
            commands.entity(e).insert(Motion { base, spin: prop.spin, slide: prop.slide });
            moving += 1;
        }
        // Animated props (sawblades, spike blocks, cranes, wildlife) play their clip on their own
        // skeleton; the rest are drawn whole.
        match prop.anim.as_ref().and_then(|a| crate::boatrig::spawn_prop(&mut commands, &mut models, &prop.mesh, a, e)) {
            Some(rig) => {
                let mut rig = rig;
                // Init Flags 0x40: an event animation (Quake Canyon's dam, rock slides) waits for its
                // StartAnim and plays once.
                if prop.init_flags & 0x40 != 0 {
                    rig.held = true;
                    rig.once = true;
                }
                events.add_prop(&prop.name, e);
                commands.entity(e).insert(rig);
                animated += 1;
            }
            None => attach(&mut commands, e, &p),
        }
    }
    if !floater_sizes.is_empty() {
        info!("{code}: {} floating objects (logs, rafts, crates...)", floater_sizes.len());
    }
    commands.insert_resource(events);
    if animated + moving > 0 {
        info!("{code}: {animated} animated props, {moving} spinning or sliding");
    }
    if matches!(choice.source, CourseSource::Sandbox { .. }) {
        spawn_hackworld(&mut commands, &mut models, scope.clone());
    }
    // Boost pickups.
    for b in &level.boosters {
        let Some(row) = pickup_row(b.kind) else { continue };
        let fallback = PICKUPS[row].model.strip_prefix("lux:mesh32.").unwrap_or("");
        let Some(p) = models.lux(&b.placement.mesh).or_else(|| models.lux(fallback)) else { continue };
        let base = Vec3::from(b.placement.position);
        let e = commands
            .spawn((
                Transform::from_translation(base).with_scale(Vec3::splat(b.placement.scale.max(0.5))),
                Visibility::default(),
                Pickup { row, taken: Vec::new(), base },
                scope.clone(),
            ))
            .id();
        attach(&mut commands, e, &p);
    }
    // Hydro Thunder course: terrain, instances, and its hydro boosts.
    if let Some((t, _)) = &ht_course {
        let s = phy::HT_WORLD_SCALE;
        let world = commands
            .spawn((Transform::from_scale(Vec3::splat(s)), Visibility::default(), scope.clone(), Name::new("terrain")))
            .id();
        // Solid scenery is placed as instances (the Nile snake, Ship Graveyard's wrecks): add it
        // to the collider in place. Pickups and fauna/effects (`G?F*`: gulls, glows, mines) stay
        // non-solid.
        let mut solid = vec![t.terrain.clone()];
        let mut cache: std::collections::HashMap<String, Option<riptide_assets::model::Model>> = Default::default();
        for inst in &t.instances {
            let fx = inst.geometry.as_bytes().get(2) == Some(&b'F');
            if fx || PICKUPS.iter().any(|p| p.ht_geometry == inst.geometry) {
                continue;
            }
            let Some(m) = cache.entry(inst.geometry.clone()).or_insert_with(|| models.content.ht.as_ref()?.geometry(&inst.geometry).ok()).clone() else { continue };
            let rot = Quat::from_rotation_y(inst.yaw);
            let at = Vec3::from(inst.position);
            let mut placed = m;
            for part in &mut placed.parts {
                for p in &mut part.positions {
                    *p = (rot * (Vec3::from(*p) * inst.scale) + at).to_array();
                }
            }
            solid.push(placed);
        }
        commands.insert_resource(Collider { probe: phy::HT_WALL_PROBE_HEIGHT, low_cut: phy::HT_WALL_LOW_CUT, ..Collider::from_models(&solid, s, phy::HT_WALL_STEEPNESS) });
        let terrain = models.ht_model(t.terrain.clone());
        attach(&mut commands, world, &terrain);
        for inst in &t.instances {
            let at = Vec3::from(inst.position) * s;
            let pickup = PICKUPS.iter().position(|p| p.ht_geometry == inst.geometry && p.status.is_ok());
            // Animated objects (`G?????H1` with an `A?????H1` clip): node pieces moved by the clip.
            // Instances name the static `H0`; the animated `H1` geometry and its clip sit beside it.
            let h1 = inst.geometry.strip_suffix("H0").map(|b| format!("{b}H1")).unwrap_or_else(|| inst.geometry.clone());
            let clip = h1.ends_with("H1").then(|| format!("A{}", &h1[1..])).and_then(|c| models.ht_clip(&c));
            if let (Some(clip), Some(nodes)) = (clip.clone(), clip.as_ref().and_then(|_| models.ht_nodes(&h1))) {
                let e = commands
                    .spawn((
                        Transform::from_translation(at).with_rotation(Quat::from_rotation_y(inst.yaw)).with_scale(Vec3::splat(s * inst.scale)),
                        Visibility::default(),
                        crate::htmotion::HtAnimator { clip: clip.clone(), t: inst.position[0].abs() * 0.001 },
                        scope.clone(),
                    ))
                    .id();
                commands.entity(e).with_children(|c| {
                    for piece in nodes.iter() {
                        let mut n = c.spawn((Mesh3d(piece.mesh.clone()), Transform::IDENTITY, crate::htmotion::HtNode { track: crate::htmotion::track_for(&clip, piece.bone) }));
                        piece.apply(&mut n);
                    }
                });
                continue;
            }
            let Some(p) = models.ht(&inst.geometry) else { continue };
            let mut e = commands.spawn((
                Transform::from_translation(at).with_rotation(Quat::from_rotation_y(inst.yaw)).with_scale(Vec3::splat(s * inst.scale)),
                Visibility::default(),
                scope.clone(),
            ));
            if let Some(row) = pickup {
                e.insert(Pickup { row, taken: Vec::new(), base: at });
            }
            let e = e.id();
            attach(&mut commands, e, &p);
        }
    }
    // Water.
    // HT water animates through `T?TWAT1nn90`, nn = 00, 01, ... (the sheet names frame 00).
    let water_tex: Vec<Handle<Image>> = match &choice.source {
        CourseSource::Ht { water: Some(w), .. } if w.len() == 11 => {
            (0..100).map_while(|n| models.ht_texture(&format!("{}{n:02}{}", &w[..7], &w[9..]))).collect()
        }
        _ => Vec::new(),
    };
    spawn_water(&mut commands, &mut models, &level, &track, water_tex, !matches!(choice.source, CourseSource::Ht { .. }));
    if let CourseSource::Ht { sky: Some(first), .. } = &choice.source {
        spawn_ht_sky(&mut commands, &mut models, first, scope.clone());
    }
    // Seen from high up (big launches), H2Overdrive levels end where the camera normally can't
    // see: lay their most common ground texture out underneath, so the gaps read as distant land.
    let apron = if matches!(choice.source, CourseSource::H2(_)) { ground_apron(&mut models, &level) } else { None };
    // The level's other skyboxes, hidden until a Skybox event shows them (crate::events).
    for (i, sky) in level.skyboxes.iter().enumerate().skip(1) {
        if let Some(p) = models.lux_unlit(&sky.mesh) {
            let e = commands
                .spawn((
                    Transform::from_rotation(Quat::from_array(sky.rotation)).with_scale(Vec3::splat(sky.scale.max(1.0))),
                    Visibility::Hidden,
                    Sky,
                    SkyIndex(i),
                    scope.clone(),
                ))
                .id();
            commands.entity(e).with_children(|c| {
                for piece in p.iter() {
                    let mut e = c.spawn((Mesh3d(piece.mesh.clone()), NoFrustumCulling, NotShadowCaster));
                    piece.apply(&mut e);
                }
            });
        }
    }
    // Sky.
    if let Some(sky) = level.skyboxes.first() {
        if let Some(p) = models.lux_unlit(&sky.mesh) {
            let e = commands
                .spawn((
                    Transform::from_rotation(Quat::from_array(sky.rotation)).with_scale(Vec3::splat(sky.scale.max(1.0))),
                    Visibility::default(),
                    Sky,
                    SkyIndex(0),
                    scope.clone(),
                ))
                .id();
            commands.entity(e).with_children(|c| {
                for piece in p.iter() {
                    { let mut e = c.spawn((Mesh3d(piece.mesh.clone()), NoFrustumCulling, NotShadowCaster)); piece.apply(&mut e); }
                }
            });
            // The dome is a half sphere: from high up, past the terrain's edge, nothing is drawn
            // below its rim. Close it with a skirt in its own horizon colour, unless a ground apron
            // fills that (the skirt, hanging from the dome around the camera, hid the apron).
            if let Some((mesh, color)) = sky_skirt(&models, &sky.mesh) {
                // Gaps in the dome (cut-out art) show the clear colour: make it the horizon too.
                commands.insert_resource(ClearColor(color));
                if apron.is_none() {
                    let material = models.materials.add(StandardMaterial { base_color: color, unlit: true, fog_enabled: false, cull_mode: None, ..default() });
                    let mesh = models.meshes.add(mesh);
                    commands.entity(e).with_children(|c| {
                        c.spawn((Mesh3d(mesh), MeshMaterial3d(material), NoFrustumCulling, NotShadowCaster));
                    });
                }
            }
        }
    }

    if let Some((mesh, material)) = apron.clone() {
        {
            commands.spawn((Mesh3d(mesh), MeshMaterial3d(material), NotShadowCaster, scope.clone(), Name::new("ground apron")));
        }
    }

    // Lighting.
    // H2Overdrive levels carry their own sun (Terrain Dir Light colour, pitch, yaw) and ambient
    // colour; Hydro Thunder courses keep a neutral daylight.
    let lit = match &choice.source {
        CourseSource::H2(lvl) | CourseSource::Sandbox { base: lvl } => H2_LEVELS.iter().find(|l| l.id == *lvl),
        _ => None,
    };
    let rgb = |v: &[f32], s: f32| Color::linear_rgb(v.first().copied().unwrap_or(1.0) * s, v.get(1).copied().unwrap_or(1.0) * s, v.get(2).copied().unwrap_or(1.0) * s);
    let (sun_color, sun_rot, amb_color, amb) = match lit {
        Some(l) => (
            rgb(l.sun_color, l.sun_scale),
            Quat::from_euler(EulerRot::YXZ, -l.sun_yaw.to_radians(), -l.sun_pitch.to_radians(), 0.0),
            rgb(l.ambient_color, 1.0),
            phy::AMBIENT_BRIGHTNESS * 2.0 * l.ambient_scale,
        ),
        None => (Color::WHITE, Quat::from_euler(EulerRot::YXZ, 0.8, -0.9, 0.0), Color::srgb(0.85, 0.9, 1.0), phy::AMBIENT_BRIGHTNESS),
    };
    commands.spawn((
        DirectionalLight { illuminance: phy::SUN_ILLUMINANCE, color: sun_color, shadows_enabled: false, ..default() },
        Transform::from_rotation(sun_rot),
        scope.clone(),
    ));
    commands.insert_resource(GlobalAmbientLight { color: amb_color, brightness: amb, ..default() });
    let fog = *fog_color.get_or_insert(Color::srgb(0.62, 0.70, 0.78));

    // Boats: player in the back row's middle, rivals spread across the roster.
    let racers = phy::RACERS as usize;
    let roster = models.content.boats.clone();
    let player_boat = sel.boat.min(roster.len().saturating_sub(1));
    // Rivals: boats whose boatdef is flagged `AI Racer`, spread across the roster.
    let rivals: Vec<usize> = (0..roster.len())
        .filter(|&i| i != player_boat && roster[i].def.ai_racer && roster[i].row.tier != crate::sheets::BoatsTier::Test)
        .collect();
    let mut picks = vec![player_boat];
    let mut k = 0;
    while picks.len() < racers && !rivals.is_empty() {
        let idx = rivals[(player_boat + k * 5) % rivals.len()];
        k += 1;
        if !picks.contains(&idx) || k > rivals.len() * 2 {
            picks.push(idx);
        }
    }
    let mut player_slot = racers - 2;
    let mut slot_of = (0..racers).filter(|&s| s != player_slot);
    // (roster index, grid slot, the other player driving it online).
    let mut grid: Vec<(usize, usize, Option<u32>)> =
        picks.iter().enumerate().map(|(i, &bi)| (bi, if i == 0 { player_slot } else { slot_of.next().unwrap_or(i) }, None)).collect();
    // Online: the room's players in grid order, no AI. The player is always the first entry.
    if let Some(race) = &online.race {
        let boat_of = |p: &riptide_net::Player| roster.iter().position(|b| b.row.id == p.boat).unwrap_or(player_boat);
        let me = race.players.iter().position(|p| p.id == online.id).unwrap_or(0);
        player_slot = me;
        grid = vec![(player_boat, me, None)];
        grid.extend(race.players.iter().enumerate().filter(|(i, _)| *i != me).map(|(i, p)| (boat_of(p), i, Some(p.id))));
    }
    // Optional local capture pose: x,y,z,yaw-degrees. Never used by browser/player sessions.
    let capture_pose = sel.render_target.as_ref().and_then(|_| std::env::var("RIPTIDE_SHOT_AT").ok()).and_then(|s| {
        let p: Vec<f32> = s.split(',').filter_map(|n| n.trim().parse().ok()).collect();
        (p.len() == 4 && p.iter().all(|n| n.is_finite())).then(|| (Vec3::new(p[0],p[1],p[2]),p[3].to_radians()))
    });
    let mut seed = 0x9e37_79b9u32 ^ (sel.level as u32 * 7919);
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 10_000) as f32 / 10_000.0
    };
    for (i, &(bi, slot, remote)) in grid.iter().enumerate() {
        let info = roster[bi].clone();
        let player = i == 0;
        let (pos, yaw) = if player { capture_pose.unwrap_or_else(|| track.grid(slot)) } else { track.grid(slot) };
        let lane = 0.2 + 0.2 * (slot % 4) as f32;
        let tp = if player && capture_pose.is_some() { track.locate_anywhere(pos) } else { track.locate(pos, 0) };
        let pieces = models.boat(&info);
        let e = commands
            .spawn((
                Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
                Visibility::default(),
                Boat {
                    info: info.clone(),
                    player,
                    pos,
                    vel: Vec2::ZERO,
                    vy: 0.0,
                    yaw,
                    speed: 0.0,
                    fuel: phy::START_FUEL,
                    super_time: 0.0,
                    boosting: false,
                    tp,
                    lap: 0,
                    finished: None,
                    timed_out: false,
                    control: Control::default(),
                    brain: Brain { lane, lane_target: lane, skill: 0.90 + 0.08 * rand(), lane_timer: rand() * 3.0, stuck: 0.0, reverse: 0.0, nav: usize::MAX, contacts: 0, scrape: 0.0 },
                    roll: 0.0,
                    pitch: 0.0,
                    airborne: false,
                    wipeout: 0.0,
                    crush: Crush::Off,
                    surface: pos.y,
                    catchup: 0.0,
                    smashes: 0,
                    rams: 0,
                    wall_hits: 0,
                    pickups: 0,
                    super_pickups: 0,
                    contacts: 0,
                    jumps: 0,
                    jump_t: 0.0,
                    climb_rate: 0.0,
                },
                scope.clone(),
                Name::new(info.name.clone()),
            ))
            .id();
        match remote {
            Some(id) => commands.entity(e).insert(crate::net::Remote { id }),
            None => commands.entity(e).insert(crate::recovery::Recovery::default()),
        };
        // Hull depth below the origin, for the ride height (place_boats).
        let hull = info.row.model.strip_prefix("lux:mesh32.").and_then(|m| {
            let b = models.content.lux.get(&format!("mesh32.{m}"))?;
            let lo = riptide_assets::h2mesh::decode_mesh(m, b).ok()?.bounds()?.0;
            Some(-lo[1] * info.scale)
        });
        if let Some(h) = hull {
            commands.entity(e).insert(Hull(h));
        }
        // Model node carries the per-game scale. H2Overdrive boats are skeletons with moving
        // parts (crate::boatrig); others are drawn whole.
        let model = commands.spawn((Transform::from_scale(Vec3::splat(info.scale)), Visibility::default())).id();
        commands.entity(e).add_child(model);
        match crate::boatrig::spawn(&mut commands, &mut models, &info, model) {
            Some(rig) => {
                commands.entity(e).insert(rig);
            }
            None => {
                if let Some(p) = pieces {
                    // No rocket nozzle bones: boost flames come out of the hull's stern, half-way up.
                    let bounds = p.iter().filter_map(|pc| bevy::camera::primitives::MeshAabb::compute_aabb(models.meshes.get(&pc.mesh)?)).fold(None, |acc: Option<(Vec3, Vec3)>, a| {
                        let (lo, hi) = (Vec3::from(a.min()), Vec3::from(a.max()));
                        Some(acc.map_or((lo, hi), |(l, h)| (l.min(lo), h.max(hi))))
                    });
                    if let Some((lo, hi)) = bounds {
                        commands.entity(e).insert(Stern(Vec3::new((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5, hi.z) * info.scale));
                    }
                    attach(&mut commands, model, &p);
                }
            }
        }
    }

    // Camera.
    let (p0, yaw0) = capture_pose.unwrap_or_else(|| track.grid(player_slot));
    let fwd = Quat::from_rotation_y(yaw0) * Vec3::NEG_Z;
    // A sky to reflect: a generated cubemap in the course's colours with the sun in it.
    let sky_map = models.images.add(sky_cubemap(sun_rot * Vec3::Z, sun_color));
    let mut cam = commands.spawn((
        Camera3d::default(),
        IsDefaultUiCamera,
        Projection::Perspective(PerspectiveProjection {
            fov: phy::CAM_FOV.to_radians(),
            near: 2.0,
            far: 400_000.0,
            ..default()
        }),
        Transform::from_translation(p0 - fwd * 170.0 + Vec3::Y * 60.0).looking_at(p0, Vec3::Y),
        DistanceFog {
            color: fog,
            falloff: if ht_course.is_some() {
                FogFalloff::Linear { start: phy::HT_FOG_START, end: phy::HT_FOG_END }
            } else {
                FogFalloff::Linear { start: phy::FOG_START, end: phy::FOG_END }
            },
            ..default()
        },
        ChaseCam,
        scope.clone(),
    ));
    cam.insert(bevy::light::GeneratedEnvironmentMapLight { environment_map: sky_map, intensity: phy::ENV_INTENSITY, ..default() });
    // The water shader can read the scene's depth behind it (shore foam, shallows: crate::water)
    // when the camera has a depth prepass. Off: with it Hydro Thunder's cut-out terrain and sky
    // vanished and London went black (2026-10-04, lavapipe). RIPTIDE_PREPASS=1 to try it.
    if std::env::var_os("RIPTIDE_PREPASS").is_some() {
        cam.insert(bevy::core_pipeline::prepass::DepthPrepass);
    }
    if let Some(target) = sel.render_target.clone() {
        cam.insert(target);
    }

    spawn_hud(&mut commands, &level);
    // The pre-race checklist plays on the grid before the countdown (not in captures unless
    // RIPTIDE_INTRO is set, nor online).
    let intro = if (sel.render_target.is_none() || std::env::var_os("RIPTIDE_INTRO").is_some()) && !online.racing() { phy::RACE_INTRO } else { 0.0 };
    commands.insert_resource(RaceClock { t: -phy::COUNTDOWN - intro, finish_order: Vec::new() });
    commands.insert_resource(Intro::default());
    // Arcade timer: the level's `Starting Seconds`, topped up by its checkpoints (sheets).
    let timer_ok = TRACKS.iter().find(|t| t.id == choice.id).is_some_and(|t| t.timer.is_ok());
    let mut timer = ArcadeTimer { enabled: false, left: 0.0, gates: Vec::new(), banner: None };
    if let (true, CourseSource::H2(lvl)) = (timer_ok, &choice.source) {
        if let Some(row) = H2_LEVELS.iter().find(|l| l.id == *lvl) {
            // Online races have no time limit: the server ends them a while after the first finish.
            timer.enabled = row.starting_seconds > 0 && !online.racing();
            timer.left = row.starting_seconds as f32;
        }
        let len = track.length();
        for cp in CHECKPOINTS.iter().filter(|c| c.status.is_ok()) {
            let Some(tw) = cp.tripwire.map(|i| &H2_TRIPWIRES[i]) else { continue };
            if tw.level.map(|l| H2_LEVELS[l].id) != Some(lvl.as_str()) || tw.position.len() < 3 {
                continue;
            }
            let p = Vec3::new(tw.position[0], tw.position[1], -tw.position[2]);
            let dist = track.locate_anywhere(p).progress;
            // A lap-N gate sits by the line between laps N-1 and N: count the crossing nearest it.
            let at = match cp.lap.max(0) as f32 {
                0.0 => dist,
                n if dist < len * 0.5 => (n - 1.0) * len + dist,
                n => (n - 2.0) * len + dist,
            };
            let here = track.locate_anywhere(p);
            timer.gates.push(Gate { at, point: p.xz(), normal: here.forward, secs: tw.int_data as f32, taken: false });
            if std::env::var_os("RIPTIDE_DEBUG").is_some() {
                info!("gate {} at {:.0} of {:.0} (lap {}, +{}s)", tw.id, at, race_length(&track), cp.lap, tw.int_data);
            }
        }
    }
    commands.insert_resource(timer);
    commands.insert_resource(track);
}

fn spawn_water(commands: &mut Commands, models: &mut Models, level: &H2Level, track: &Track, frames: Vec<Handle<Image>>, add_ribbon: bool) {
    // On by default (desktop and browser); RIPTIDE_H2WATER=0 keeps Riptide's own water.
    let h2 = std::env::var("RIPTIDE_H2WATER").map_or(true, |v| v != "0")
        && !level.water_sectors.is_empty()
        && models.content.lux.get("shad4.FX_Water2").and_then(|blob| crate::h2water::install(&mut models.shaders, blob)).is_some_and(|sh| {
            // Hydro Thunder: generated edges, coloured from the course's own water art.
            let mut level = level.clone();
            if let Some(img) = frames.first().and_then(|f| models.images.get(f)) {
                crate::h2water::tint_from(&mut level, img);
            }
            let level = &level;
            let bump = models.lux_normal_map("wavesbump").unwrap_or_default();
            // Repeat sampler, set once: touching the image again (a restart) re-uploads it, and its
            // pixels live only in the render world, so the ripples came back blank (a flat mirror).
            if models.images.get(&bump).is_some_and(|i| !matches!(i.sampler, bevy::image::ImageSampler::Descriptor(_))) {
                if let Some(img) = models.images.get_mut(&bump) {
                    img.sampler = bevy::image::ImageSampler::Descriptor(crate::content::repeat_sampler());
                }
            }
            let n = crate::h2water::spawn(commands, level, &sh, &mut models.meshes, &mut models.h2water, &mut models.images, bump.clone(), phy::WATER_CELL.max(1.0));
            let lux = models.content.lux.clone();
            if let Some(blob) = lux.get("shad4.FX_Waterfall").filter(|_| !std::env::var("RIPTIDE_WATERFALLS").is_ok_and(|v| v == "0")) {
                let art = [format!("pt_{}_waterfall1", level.code), "pt_wa_waterfall1".into()].iter().find_map(|n| models.lux_texture(n)).unwrap_or_else(|| bump.clone());
                let falls = crate::h2water::spawn_waterfalls(commands, level, &mut models.shaders, blob, &mut models.meshes, &mut models.waterfalls, &mut models.images, art);
                info!("h2water: {falls} waterfalls with shad4.FX_Waterfall");
            }
            info!("h2water: {n} sectors with shad4.FX_Water2");
            n > 0
        });
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // Each quad is split into a grid fine enough to carry the waves the boats bob on: cells no
    // bigger than water_cell (a fixed count left Hydro Thunder's huge river quads with cells
    // longer than a wave, so the drawn surface missed the waves and boats sank into it).
    let mut quad = |c: [[f32; 3]; 4]| {
        let side = |a: [f32; 3], b: [f32; 3]| Vec2::new(a[0] - b[0], a[2] - b[2]).length();
        let longest = side(c[0], c[1]).max(side(c[1], c[2])).max(side(c[2], c[3])).max(side(c[3], c[0]));
        let sub = ((longest / phy::WATER_CELL.max(1.0)).ceil() as u32).clamp((phy::WATER_SUBDIV as u32).max(1), 96);
        let base = positions.len() as u32;
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
        for j in 0..=sub {
            for i in 0..=sub {
                let (u, v) = (i as f32 / sub as f32, j as f32 / sub as f32);
                let p = lerp(lerp(c[0], c[1], u), lerp(c[3], c[2], u), v);
                positions.push([p[0], p[1] + 0.5, p[2]]);
                uvs.push([p[0] / 350.0, p[2] / 350.0]);
            }
        }
        let w = sub + 1;
        for j in 0..sub {
            for i in 0..sub {
                let k = base + j * w + i;
                indices.extend([k, k + 1, k + w + 1, k, k + w + 1, k + w]);
            }
        }
    };
    for q in level.water.iter().filter(|_| !h2) {
        quad(q.corners);
    }
    // The racing line's own ribbon fills any gap between water sectors.
    if !h2 {
        commands.remove_resource::<crate::h2water::H2WaterOn>();
        commands.remove_resource::<crate::h2water::H2Waves>();
    }
    let ribbon = if track.open || !add_ribbon || h2 { 0 } else { track.edges.len() - 1 };
    for i in 0..ribbon {
        let (a, b) = (&track.edges[i], &track.edges[i + 1]);
        let lift = |p: [f32; 3], h: f32| [p[0], h - 1.5, p[2]];
        quad([lift(a.start, a.water), lift(a.end, a.water), lift(b.end, b.water), lift(b.start, b.water)]);
    }
    let n = positions.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    let _ = mesh.generate_tangents();
    // Kept in the main world too: `water_flow` moves the vertices every frame.
    let mesh = models.meshes.add(mesh);
    // Hydro Thunder water art is already coloured; H2Overdrive's needs the blue tint.
    let own_color = !frames.is_empty();
    // The level's own water art and normal map, else the shared ones.
    let code = &level.code;
    let texture = frames
        .first()
        .cloned()
        .or_else(|| models.lux_texture(&format!("wt_{code}_water")))
        .or_else(|| models.lux_texture("wt_qc_water"));
    let normal_map = [format!("wt_{code}_water_N"), format!("wt_{code}_water_NM"), "pt_tst_water_normal".into()]
        .iter()
        .find_map(|n| models.lux_normal_map(n));
    let material = StandardMaterial {
        base_color: if own_color {
            Color::srgba(1.0, 1.0, 1.0, 0.97)
        } else if texture.is_some() {
            Color::srgba(0.42, 0.62, 0.66, 0.9)
        } else { Color::srgba(0.03, 0.20, 0.26, 0.86) },
        base_color_texture: texture,
        emissive: LinearRgba::rgb(0.004, 0.025, 0.035),
        alpha_mode: AlphaMode::Blend,
        // HT water is painted art: keep it matte so sky reflections don't wash it out.
        // (The normal map drives the shader's ripple layers instead: crate::water.)
        perceptual_roughness: if own_color { 0.35 } else { 0.22 },
        reflectance: if own_color { 0.2 } else { 0.35 },
        double_sided: true,
        cull_mode: None,
        ..default()
    };
    let water = models.water.add(crate::water::WaterMat {
        base: material.clone(),
        extension: crate::water::WaterExt { params: Default::default(), ripples: normal_map },
    });
    // Waterfalls are walls of water: the plain material, no flat-water waves.
    let material = models.materials.add(material);
    commands.insert_resource(WaterMaterial { material: water.clone(), frames });
    if !level.waterfalls.is_empty() {
        let mut positions = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();
        for q in &level.waterfalls {
            let offset = positions.len() as u32;
            positions.extend(q.corners);
            let width = Vec3::from(q.corners[0]).distance(Vec3::from(q.corners[1])) / 350.0;
            let height = Vec3::from(q.corners[0]).distance(Vec3::from(q.corners[3])) / 350.0;
            uvs.extend([[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]]);
            indices.extend([offset, offset + 1, offset + 2, offset, offset + 2, offset + 3]);
        }
        let mut falls = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        falls.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        falls.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        falls.insert_indices(Indices::U32(indices));
        falls.compute_normals();
        commands.spawn((Mesh3d(models.meshes.add(falls)), MeshMaterial3d(material.clone()), Name::new("waterfalls"), DespawnOnExit(Screen::Race)));
    }
    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(water),
        NotShadowCaster,
        DespawnOnExit(Screen::Race),
        Name::new("water"),
    ));
}

fn spawn_hud(commands: &mut Commands, level: &H2Level) {
    let scope = DespawnOnExit(Screen::Race);
    let big = TextFont { font_size: 34.0, ..default() };
    let shadow = TextShadow::default();
    commands
        .spawn((
            Node { width: percent(100), height: percent(100), position_type: PositionType::Absolute, ..default() },
            scope,
        ))
        .with_children(|root| {
            root.spawn((
                // Place, times and gauges are the game's own HUD (crate::hud); this keeps the title.
                Node { position_type: PositionType::Absolute, left: px(24), bottom: px(18), flex_direction: FlexDirection::Column, ..default() },
            ))
            .with_children(|c| {
                c.spawn((
                    Text::new(if level.title.is_empty() { level.code.to_uppercase() } else { level.title.clone() }),
                    TextFont { font_size: 18.0, ..default() },
                    TextColor(Color::srgb(0.8, 0.85, 0.9)),
                    shadow,
                ));
            });
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    right: px(28),
                    bottom: px(24),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::FlexEnd,
                    row_gap: px(6),
                    ..default()
                },
            ))
            .with_children(|c| {
                c.spawn((Text::new(""), TextFont { font_size: 18.0, ..default() }, shadow, HudText::Boat));
                c.spawn((
                    Node { width: px(260), height: px(16), border: UiRect::all(px(2)), display: Display::None, ..default() },
                    BorderColor::all(Color::srgb(0.9, 0.9, 0.95)),
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.4)),
                ))
                .with_children(|bar| {
                    bar.spawn((
                        Node { width: percent(50), height: percent(100), ..default() },
                        BackgroundColor(Color::srgb(0.2, 0.7, 1.0)),
                        BoostBar,
                    ));
                });
            });
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    width: percent(100),
                    top: percent(30),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
            ))
            .with_children(|c| {
                c.spawn((
                    Text::new(""),
                    TextFont { font_size: 72.0, ..default() },
                    TextLayout::new_with_justify(Justify::Center),
                    TextColor(Color::srgb(1.0, 0.85, 0.2)),
                    shadow,
                    HudText::Center,
                ));
            });
        });
}

fn player_input(input: Input, cheats: Res<Cheats>, autopilot: Option<Res<Autopilot>>, mut boats: Query<&mut Boat>) {
    if autopilot.is_some() {
        return;
    }
    let mut c = Control::default();
    if !cheats.menu_open {
        c.throttle = input.analog(ctl::THROTTLE).max(if input.pressed(ctl::THROTTLE) { 1.0 } else { 0.0 });
        c.throttle -= 0.6 * input.analog(ctl::BRAKE).max(if input.pressed(ctl::BRAKE) { 1.0 } else { 0.0 });
        if input.pressed(ctl::STEER_LEFT) {
            c.steer -= 1.0;
        }
        if input.pressed(ctl::STEER_RIGHT) {
            c.steer += 1.0;
        }
        for pad in &input.pads {
            let stick = pad.left_stick();
            if stick.x.abs() > 0.12 {
                c.steer = stick.x;
            }
        }
        c.boost = input.pressed(ctl::BOOST);
        // Boost + brake is the jump (H2Overdrive / Hydro Thunder): no braking while boosting.
        c.brake_held = input.pressed(ctl::BRAKE);
        c.jump = c.boost && input.just_pressed(ctl::BRAKE);
        if c.boost && c.throttle < 0.0 {
            c.throttle = 1.0;
        }
    }
    for mut b in &mut boats {
        if b.player {
            b.control = c;
        }
    }
}

/// Total race distance of the track (all laps).
fn race_length(track: &Track) -> f32 {
    if track.looped {
        track.race_distance(track.laps, 0.0)
    } else {
        track.length()
    }
}

fn ai_drive(
    time: Res<Time>,
    track: Res<Track>,
    clock: Res<RaceClock>,
    collider: Option<Res<Collider>>,
    tuning: Res<Tuning>,
    nav: Option<Res<crate::nav::NavLine>>,
    autopilot: Option<Res<Autopilot>>,
    mut boats: Query<&mut Boat, Without<crate::net::Remote>>,
) {
    let g = &tuning.0;
    let dt = time.delta_secs().min(1.0 / 20.0);
    let player_progress = boats.iter().find(|b| b.player).map(|b| track.race_distance(b.lap, b.tp.progress)).unwrap_or(0.0);
    for mut b in &mut boats {
        if b.player && autopilot.is_none() {
            continue;
        }
        let tp = b.tp;
        let mut brain = b.brain;
        brain.lane_timer -= dt;
        if b.contacts > brain.contacts {
            brain.scrape = phy::AI_SCRAPE_TIME;
        }
        brain.contacts = b.contacts;
        brain.scrape = (brain.scrape - dt).max(0.0);
        if brain.lane_timer <= 0.0 {
            let h = (tp.seg as f32 * 12.9898 + brain.skill * 78.233).sin() * 43758.545;
            brain.lane_target = 0.25 + 0.5 * h.fract().abs();
            // Test hook: RIPTIDE_TEST_LANE=0.05 drives the autopiloted player along one bank.
            if let Some(l) = b.player.then(|| std::env::var("RIPTIDE_TEST_LANE").ok()?.parse::<f32>().ok()).flatten() {
                brain.lane_target = l;
            }
            brain.lane_timer = 2.5 + 3.0 * (h * 0.37).fract().abs();
        }
        brain.lane += (brain.lane_target - brain.lane) * (dt * 0.6).min(1.0);
        // Aim a speed-dependent distance down the line.
        let look = (phy::AI_LOOK + b.speed.abs() * phy::AI_LOOK_PER_SPEED).min(phy::AI_LOOK_MAX);
        let aim = match &nav {
            Some(n) => {
                brain.nav = n.nearest(b.pos, brain.nav);
                n.aim(brain.nav, look, brain.lane)
            }
            // The racing line's cross-sections can reach past the banks on bends: while scraping a
            // wall, if one stands between the boat and its aim point, aim down the middle, then closer.
            None if brain.scrape <= 0.0 => aim_point(&track, tp, look, brain.lane),
            None => {
                let clear = |p: Vec3| {
                    collider.as_ref().is_none_or(|col| {
                        let h = Vec3::Y * col.probe.min(phy::WALL_PROBE_HEIGHT * 2.0);
                        col.hit(b.pos + h, Vec3::new(p.x, b.pos.y, p.z) + h).is_none()
                    })
                };
                [(look, brain.lane), (look, 0.5), (look * 0.5, 0.5)]
                    .into_iter()
                    .map(|(ahead, lane)| aim_point(&track, tp, ahead, lane))
                    .find(|p| clear(*p))
                    .unwrap_or_else(|| aim_point(&track, tp, look, brain.lane))
            }
        };
        let to = Vec2::new(aim.x - b.pos.x, aim.z - b.pos.z);
        let heading = Vec2::new(-b.yaw.sin(), -b.yaw.cos());
        let cross = heading.perp_dot(to.normalize_or_zero());
        let dot = heading.dot(to.normalize_or_zero());
        // `cross` < 0 means the aim point is to the boat's left; steer is +1 for right.
        let mut steer = (cross * 2.2).clamp(-1.0, 1.0);
        if dot < 0.0 {
            steer = if cross > 0.0 { 1.0 } else { -1.0 };
        }
        // Obstacle ahead (rocks, pillars, wrecks): probe the way it is heading at hull height and
        // steer toward whichever side probe is clearer.
        if let Some(col) = &collider {
            let eye = b.pos + Vec3::Y * col.probe.min(phy::WALL_PROBE_HEIGHT * 2.0);
            let reach = (b.speed.abs() * phy::AI_PROBE_TIME).max(phy::AI_PROBE_MIN);
            let ray = |yaw: f32| Vec3::new(-yaw.sin(), 0.0, -yaw.cos()) * reach;
            if col.hit(eye, eye + ray(b.yaw)).is_some() {
                let left = col.hit(eye, eye + ray(b.yaw + phy::AI_PROBE_ANGLE)).unwrap_or(1.0);
                let right = col.hit(eye, eye + ray(b.yaw - phy::AI_PROBE_ANGLE)).unwrap_or(1.0);
                // Positive yaw turns left (steer -1).
                steer = if left >= right { -1.0 } else { 1.0 };
            }
        }
        // H2Overdrive catch-up: help AI boats that are behind the player, fading out near the finish.
        let mine = track.race_distance(b.lap, tp.progress);
        let behind = player_progress - mine;
        let span = (g.catchup_max_behind_distance - g.catchup_min_behind_distance).max(1.0);
        let near = ((race_length(&track) - mine - g.catchup_dist_from_finish_off)
            / (g.catchup_dist_from_finish_ramp_down - g.catchup_dist_from_finish_off).max(1.0))
        .clamp(0.0, 1.0);
        b.catchup = if b.player { 0.0 } else { ((behind - g.catchup_min_behind_distance) / span).clamp(0.0, 1.0) * near };
        b.fuel = (b.fuel + g.catchup_unit_boost_help * b.catchup * dt).min(g.boost_fuel_max_regular);
        // No boosting into a turn: the course direction ai_boost_lookahead seconds ahead (at this
        // speed) must stay within ai_boost_straight of the current one (with boost a real +26%,
        // boats boosted wide through bends and kept respawning: Ship Graveyard, Revenge of the Nile).
        let straight_ahead = {
            let at = (tp.progress + b.speed.max(0.0) * phy::AI_BOOST_LOOKAHEAD).min(track.length() - 1.0);
            let s = track.dist.windows(2).position(|w| at < w[1]).unwrap_or(track.last_seg());
            let dir = (track.point(s, 1.0, 0.5) - track.point(s, 0.0, 0.5)).xz().normalize_or_zero();
            dir.dot(tp.forward) >= phy::AI_BOOST_STRAIGHT
        };
        let corner = 1.0 - (steer.abs() * 0.25);
        let skill = if b.player { 1.0 } else { brain.skill };
        b.control = Control {
            throttle: (skill * corner).clamp(0.3, 1.1),
            steer,
            boost: b.fuel > 0.35 * g.boost_fuel_max_regular && steer.abs() < 0.3 && dot > 0.95 && straight_ahead,
            ..default()
        };
        // Test hook: RIPTIDE_TEST_BOOST=secs holds boost (no steering) for three seconds from then.
        if let Some(t) = b.player.then(|| std::env::var("RIPTIDE_TEST_BOOST").ok()?.parse::<f32>().ok()).flatten() {
            if clock.t > t && clock.t < t + 3.0 {
                b.control = Control { throttle: 1.0, steer: b.control.steer, boost: true, ..default() };
                b.brain = brain;
                continue;
            }
        }
        // Test hook: RIPTIDE_TEST_JUMP=secs boosts and presses jump at that time (twice, 0.3 s apart).
        if let Some(t) = b.player.then(|| std::env::var("RIPTIDE_TEST_JUMP").ok()?.parse::<f32>().ok()).flatten() {
            if clock.t > t - 0.5 && clock.t < t + 1.0 {
                let press = (clock.t - t).abs() < dt * 0.6 || (clock.t - t - 0.3).abs() < dt * 0.6;
                b.control = Control { throttle: 1.0, steer: 0.0, boost: true, jump: press, brake_held: true };
                b.brain = brain;
                continue;
            }
        }
        // Test hook: RIPTIDE_TEST_BRAKE=secs holds the autopiloted player's brake after that time.
        if b.player && std::env::var("RIPTIDE_TEST_BRAKE").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|t| clock.t > t) {
            b.control = Control { throttle: -1.0, steer: 0.0, boost: false, ..default() };
            b.brain = brain;
            continue;
        }
        // Stuck on an obstacle: back off for a moment, steering the other way.
        let racing = clock.t > 1.0 && b.finished.is_none() && !b.timed_out && b.wipeout <= 0.0;
        if brain.reverse > 0.0 {
            brain.reverse -= dt;
            b.control = Control { throttle: -1.0, steer: -steer, boost: false, ..default() };
        } else if racing && !b.airborne && b.speed.abs() < phy::AI_STUCK_SPEED {
            brain.stuck += dt;
            if brain.stuck > phy::AI_STUCK_TIME {
                brain.stuck = 0.0;
                brain.reverse = phy::AI_REVERSE_TIME;
                brain.lane_target = 1.0 - brain.lane_target;
            }
        } else {
            brain.stuck = 0.0;
        }
        b.brain = brain;
    }
}

fn boat_physics(
    time: Res<Time>,
    track: Res<Track>,
    clock: Res<RaceClock>,
    cheats: Res<Cheats>,
    tuning: Res<Tuning>,
    collider: Option<Res<Collider>>,
    h2waves: Option<Res<crate::h2water::H2Waves>>,
    tides: Res<crate::h2water::Tides>,
    mut boats: Query<&mut Boat, Without<crate::net::Remote>>,
) {
    let g = &tuning.0;
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }
    let racing = clock.t >= 0.0;
    for mut b in &mut boats {
        let def = b.info.def;
        // The boat def's own Gravity (-200 on every boat; measured 198-205 units/s² on the original
        // Wild America, evidence EVD_GRAVITY), Wipeout Gravity while wiping out.
        let own = track.gravity.unwrap_or(-def.gravity);
        let fall = if b.wipeout > 0.0 { own * def.wipeout_gravity / def.gravity.min(-1.0) } else { own };
        let mut c = b.control;
        if !racing || b.finished.is_some() || b.timed_out {
            c = Control { throttle: if b.finished.is_some() { 0.3 } else { 0.0 }, steer: c.steer * 0.3, boost: false, ..default() };
        }
        if b.wipeout > 0.0 {
            b.wipeout = (b.wipeout - dt).max(0.0);
            c = Control::default();
        }
        // Player cheats.
        let hold_crush = b.player && cheats.active(CheatsEffect::HullcrushHold);
        if b.player {
            if cheats.active(CheatsEffect::FuelFull) {
                b.fuel = g.boost_fuel_max_regular;
            }
            if cheats.active(CheatsEffect::SuperFull) {
                b.super_time = g.boost_fuel_max_super;
            }
        }
        // Hull Crusher: deploy -> active -> stow. With the cheat, Boost starts it and holding
        // Boost keeps it active.
        b.crush = match b.crush {
            Crush::Off if hold_crush && c.boost && racing => Crush::Deploy(g.hullcrush_time_deploy),
            Crush::Off => Crush::Off,
            Crush::Deploy(t) if t - dt <= 0.0 => Crush::Active(g.hullcrush_time_active),
            Crush::Deploy(t) => Crush::Deploy(t - dt),
            Crush::Active(_) if hold_crush && c.boost => Crush::Active(g.hullcrush_time_active),
            Crush::Active(t) if t - dt <= 0.0 => Crush::Stow(g.hullcrush_time_stow),
            Crush::Active(t) => Crush::Active(t - dt),
            Crush::Stow(t) if t - dt <= 0.0 => Crush::Off,
            Crush::Stow(t) => Crush::Stow(t - dt),
        };
        // Boost: fuel is seconds of boost; gold is seconds of super boost.
        b.super_time = (b.super_time - dt).max(0.0);
        b.boosting = (c.boost && b.fuel > 0.0) || b.super_time > 0.0;
        if c.boost && b.super_time <= 0.0 && b.fuel > 0.0 {
            b.fuel = (b.fuel - phy::FUEL_BURN * dt).max(0.0);
        }
        // Jumps from the game's globals: Target Single / Double Jump Height (single player), the
        // double jump within `2-Jump Activation Max Time`, and a tap (released before `High Jump
        // Button Hold Time`) trimmed by `Low Jump Upward Vel Atten`.
        b.jump_t += dt;
        if c.jump && b.boosting && racing && b.wipeout <= 0.0 {
            let lift = |h: f32| (2.0 * fall * h).sqrt();
            if !b.airborne {
                b.vy = lift(g.target_single_jump_height_sp);
                b.airborne = true;
                b.jumps = 1;
                b.jump_t = 0.0;
            } else if b.jumps == 1 && b.jump_t <= g._2_jump_activation_max_time {
                b.vy = b.vy.max(lift(g.target_double_jump_height_sp));
                b.jumps = 2;
                b.jump_t = 0.0;
            }
        }
        if b.jumps > 0 && b.airborne && !c.brake_held && b.jump_t < g.high_jump_button_hold_time && b.vy > 0.0 {
            b.vy *= g.low_jump_upward_vel_atten;
        }
        if !b.airborne {
            b.jumps = 0;
        }
        // Speed and thrust from the boat's own CBoatDef.
        let (mut top, mut thrust) = if b.super_time > 0.0 {
            (def.max_speed_super, def.thrust_super)
        } else if b.boosting {
            (def.max_speed_boost, def.thrust_boost)
        } else {
            (def.max_speed_l1, def.thrust_l1)
        };
        // Boat-def speeds are not world units/s (see physics.speed_scale).
        let cheat = if b.player { cheats.speed() } else { 1.0 };
        top *= (1.0 + (g.catchup_max_speed_factor - 1.0) * b.catchup) * b.info.tune[0] * cheat * phy::SPEED_SCALE;
        thrust *= (1.0 + g.catchup_unit_thrust_help * b.catchup) * b.info.tune[1] * phy::SPEED_SCALE;
        // Thrust and drag.
        if !b.airborne {
            if c.throttle > 0.0 {
                let room = (top - b.speed).max(0.0) / top;
                b.speed += c.throttle * thrust * phy::ACCEL_PER_THRUST * (phy::START_ROOM_BIAS + phy::THRUST_ROOM_WEIGHT * room) * dt;
            } else if c.throttle < 0.0 {
                b.speed += c.throttle * def.thrust_l1 * phy::SPEED_SCALE * phy::ACCEL_PER_THRUST * phy::BRAKE_MULT * dt;
            }
            b.speed -= b.speed * phy::WATER_DRAG * dt;
            if b.speed > top {
                b.speed -= (b.speed - top) * phy::OVERSPEED_DRAG * dt;
            }
            if b.wipeout > 0.0 {
                b.speed -= b.speed * phy::OVERSPEED_DRAG * dt;
            }
            b.speed = b.speed.clamp(-phy::REVERSE_MAX, top * 1.1);
        }
        // Steering: sharper at mid speed, lazy when crawling or airborne.
        let v = b.speed.abs();
        let grip = if b.airborne { phy::AIR_GRIP } else { 1.0 };
        let turn = def.turn_rate
            * b.info.tune[2]
            * (v / phy::TURN_FULL_SPEED).min(1.0)
            * (1.0 - (v / phy::TURN_FALLOFF_SPEED).min(phy::TURN_FALLOFF_MAX))
            * grip;
        b.yaw -= c.steer * turn * dt * b.speed.signum();
        let heading = Vec2::new(-b.yaw.sin(), -b.yaw.cos());
        // Water lets the hull slide: velocity chases the heading.
        let want = heading * b.speed;
        let slide = if b.airborne { phy::SLIDE_AIR } else { phy::SLIDE_WATER * b.info.tune[3] };
        let vel = b.vel;
        b.vel = vel.lerp(want, (slide * dt).min(1.0));
        let mut step = b.vel * dt;
        let r = phy::BOAT_RADIUS * b.info.scale.min(1.3);
        let mut penalised = false;
        // Parry courses (tracks.collision): moves longer than the hull radius are swept first,
        // collide and slide against up to three walls, so nothing skips through. Classic courses
        // and slower moves rely on the sub-step push-out below.
        if let Some(col) = collider.as_ref().filter(|c| c.parry.is_some() && c.trusted && step.length() > r) {
            let mut from = b.pos;
            let mut done = Vec2::ZERO;
            for _ in 0..3 {
                let Some((t, n)) = col.sweep_hull(from, step, r, Some(b.tp.forward)) else {
                    done += step;
                    step = Vec2::ZERO;
                    break;
                };
                let before = step * (t - 0.02).max(0.0);
                done += before;
                from += Vec3::new(before.x, 0.0, before.y);
                let rest = step - before;
                step = rest - n * rest.dot(n).min(0.0);
                let into = b.vel.dot(n);
                if into < 0.0 {
                    if !penalised && -into > g.unit_scrape_to_impact_collision_threshhold * b.vel.length() {
                        b.speed *= g.hit_wall_speed_penalty_mult;
                        penalised = true;
                        b.wall_hits += 1;
                    }
                    b.vel -= n * into * phy::WALL_BOUNCE;
                }
            }
            step += done;
        }
        // Sub-steps of at most half a hull radius so fast boats cannot skip through a wall.
        let subs = ((step.length() / (r * 0.5)).ceil() as usize).clamp(1, 8);
        for _ in 0..subs {
            b.pos.x += step.x / subs as f32;
            b.pos.z += step.y / subs as f32;
            // Course walls (rocks, hulls, pillars, cliffs): push out and lose the into-wall velocity;
            // a real impact costs `Hit Wall Speed Penalty Mult` like the banks.
            let Some(col) = &collider else { continue };
            // Cut at hull height and lower down, so low walls and slopes catch too.
            let fwd = b.tp.forward;
            // Parry courses: the hull cylinder's exact contacts; classic: walls cut at two heights.
            let touch = if col.parry.is_some() {
                col.hull(b.pos, r, Some(fwd)).map(|(p, n, _)| (p, n))
            } else {
                col.cuts().into_iter().find_map(|h| col.walls_id(b.pos, r, b.pos.y + h, Some(fwd)).map(|(p, n, _)| (p, n)))
            };
            let hit = touch.filter(|(push, _)| {
                // Hydro Thunder collides with its visual terrain: there the corridor owns the
                // banks and pushes that would leave it are ignored. H2Overdrive's collision
                // mesh is authoritative.
                col.trusted || {
                    let to = b.pos + Vec3::new(push.x, 0.0, push.y);
                    let u = track.locate(to, b.tp.seg).u;
                    (phy::WALL_MARGIN..=1.0 - phy::WALL_MARGIN).contains(&u)
                }
            });
            if let Some((push, n)) = hit {
                b.pos.x += push.x;
                b.pos.z += push.y;
                b.contacts += 1;
                if b.player && std::env::var_os("RIPTIDE_DEBUG").is_some() {
                    let before = b.pos - Vec3::new(push.x, 0.0, push.y);
                    let h = col.cuts().into_iter().find_map(|h| col.walls_id(before, r, before.y + h, Some(fwd)));
                    let tri = h.map(|(_, _, id)| (col.tris[id as usize], col.barrier[id as usize], fwd));
                    info!("wall contact at {:?} push {:.1} normal {:?} tri {:?}", b.pos, push.length(), n, tri);
                }
                let into = b.vel.dot(n);
                if into < 0.0 {
                    if !penalised && -into > g.unit_scrape_to_impact_collision_threshhold * b.vel.length() {
                        b.speed *= g.hit_wall_speed_penalty_mult;
                        penalised = true;
                        b.wall_hits += 1;
                    }
                    let v = b.vel - n * into * phy::WALL_BOUNCE;
                    b.vel = v;
                }
                if heading.dot(n) < -0.7 {
                    b.speed *= 1.0 - phy::WALL_SCRAPE_DRAG * dt / subs as f32;
                }
            }
        }

        // Where are we on the track? Crossing the end of a circuit starts the next lap.
        let mut tp = track.locate(b.pos, b.tp.seg);
        if track.looped && tp.seg >= track.last_seg() && tp.s >= 1.0 {
            b.lap += 1;
            tp = track.locate(b.pos, 0);
        }
        // Off the racing line but over the level's own water (a secret path, a shortcut, a side pool)
        // on a course whose collision mesh is authoritative (H2Overdrive): no corridor bank there, the
        // collision mesh is the only wall. The bank stays as the edge of the world where there is no
        // water.
        let off_line = tp.u < phy::WALL_MARGIN || tp.u > 1.0 - phy::WALL_MARGIN;
        let free_water = off_line
            && collider.as_ref().is_some_and(|c| c.trusted)
            && h2waves.as_ref().and_then(|w| w.surface(b.pos.xz(), b.pos.y)).is_some();
        // Banks: a real impact costs `Hit Wall Speed Penalty Mult`, a glancing scrape just drags.
        if !track.open && off_line && !free_water {
            let u = tp.u.clamp(phy::WALL_MARGIN, 1.0 - phy::WALL_MARGIN);
            let on = track.point_at(tp, u);
            b.pos.x = on.x;
            b.pos.z = on.z;
            let across = Vec2::new(-tp.forward.y, tp.forward.x);
            let into = b.vel.dot(across);
            let wall_side = if tp.u < 0.5 { -1.0 } else { 1.0 };
            if into * wall_side > 0.0 {
                if into.abs() > g.unit_scrape_to_impact_collision_threshhold * b.vel.length() {
                    b.speed *= g.hit_wall_speed_penalty_mult;
                }
                let bounce = b.vel - across * into * phy::WALL_BOUNCE;
                b.vel = bounce;
            }
            b.speed *= 1.0 - phy::WALL_SCRAPE_DRAG * dt;
        }
        // Behind the start line: keep boats on the grid apron.
        if track.open {
            // The edge of the world: stop at the end of the water.
            let h = phy::HACKWORLD_WATER_HALF - 500.0;
            if b.pos.x.abs() > h || b.pos.z.abs() > h {
                b.pos.x = b.pos.x.clamp(-h, h);
                b.pos.z = b.pos.z.clamp(-h, h);
                b.vel = -b.vel * 0.3;
                b.speed *= 0.3;
            }
        } else if tp.seg == 0 && tp.s < -3.0 {
            b.vel = tp.forward * b.vel.length();
        }
        // Water surface and gravity.
        // In and beside a sloped chute, the level of the H2Overdrive water sector actually under the hull: a racing-line
        // cross-section has one height, but a chute drops between side pools that stay up (Hong
        // Kong's flumes); riding the cross-section there sank boats in the pools into the weir.
        let mut tp = tp;
        let near_chute = (tp.seg.saturating_sub(1)..=tp.seg + 1).any(|s| track.chutes.get(s).copied().unwrap_or(false));
        if let Some(h) = h2waves.as_ref().filter(|_| near_chute || free_water).and_then(|w| w.surface(b.pos.xz(), b.pos.y.min(tp.water + phy::H2WATER_SURFACE_MAX))) {
            if (h - tp.water).abs() < phy::H2WATER_SURFACE_MAX {
                tp.water = h;
            }
        }
        // A rolling tidal wave (crate::h2water::Tides, started by level events) lifts the water.
        if !tides.0.is_empty() {
            tp.water += tides.height(b.pos.xz(), clock.t);
        }
        // The surface a hull rides: the water, or a terrain floor (ramp, mound) within step-up reach.
        let reach = b.pos.y + phy::FLOOR_STEP_UP;
        let inside = (phy::FLOOR_CORRIDOR_MARGIN..=1.0 - phy::FLOOR_CORRIDOR_MARGIN).contains(&tp.u);
        let floor = collider
            .as_ref()
            .filter(|_| inside || track.open)
            .and_then(|c| c.floor(b.pos.x, b.pos.z, reach.min(tp.water + phy::FLOOR_MAX_ABOVE_WATER)));
        let water = floor.map_or(tp.water, |f| f.max(tp.water));
        b.vy -= fall * dt;
        b.pos.y += b.vy * dt;
        if b.pos.y <= water {
            let was_air = b.airborne;
            // Ride the surface; a falling water line lets the hull fly.
            b.pos.y = water;
            if b.vy < 0.0 {
                b.vy = if was_air && b.vy < -120.0 { -b.vy * 0.15 } else { 0.0 };
            }
            if was_air {
                b.speed *= 0.97;
            }
            b.airborne = false;
        } else if b.pos.y > water + 6.0 {
            b.airborne = true;
        }
        // Test hook: RIPTIDE_TEST_LAUNCH=secs throws the player high once (checking the view from
        // the air). Never online.
        if b.player && !b.airborne && !cheats.locked {
            if let Some(t) = std::env::var("RIPTIDE_TEST_LAUNCH").ok().and_then(|s| s.parse::<f32>().ok()) {
                if clock.t > t && clock.t < t + 0.2 {
                    b.vy = 900.0;
                    b.airborne = true;
                }
            }
        }
        // Crest launches: the surface fell away faster than gravity.
        if !b.airborne {
            let prev = b.surface;
            let climb = water - prev;
            // Only a slope launches: a ledge climbed in one frame (rocks, collision-mesh steps,
            // prop edges) is steeper than physics.ramp_max_slope over the distance travelled.
            let run = b.vel.length() * dt;
            // Riding up a ramp carries the climb rate into the air at its lip, never faster than
            // the game's own launch ceiling (TritonGame Player / AI Vel Y Max). The rate is smoothed
            // over ramp_climb_smooth: frame to frame the surface steps between triangles, and those
            // spikes launched boats at the ceiling off gentle ramps (Wild America's first ramp,
            // slope 0.25: 350 here, 181 in the original at the same speed).
            let rate = if climb > 0.0 && climb <= run * phy::RAMP_MAX_SLOPE { climb / dt } else { 0.0 };
            b.climb_rate += (rate - b.climb_rate) * (dt / phy::RAMP_CLIMB_SMOOTH.max(dt)).min(1.0);
            if rate > 0.0 {
                let cap = if b.player { g.player_vel_y_max } else { g.ai_vel_y_max };
                b.vy = b.vy.max(b.climb_rate.min(cap));
            }
        }
        b.surface = water;
        b.tp = tp;
        // Point to point: the finish buoys' plane when the level has them (crossed, not reached
        // by centre-line distance), else the end of the racing line.
        // The finish line (buoys): the nose crossing it this frame, the time interpolated to the
        // instant it touched, so the race ends exactly on the line.
        let mut exact: Option<f32> = None;
        if let (false, Some((a, c)), Some((_, fwd, at))) = (track.looped, track.finish_line, track.finish) {
            let heading = Vec2::new(-b.yaw.sin(), -b.yaw.cos());
            let nose = b.pos.xz() + heading * phy::BOAT_RADIUS * b.info.scale;
            let prev = nose - b.vel * dt;
            let dir = c - a;
            let n = { let p = dir.perp().normalize_or_zero(); if p.dot(fwd) < 0.0 { -p } else { p } };
            let (d0, d1) = ((prev - a).dot(n), (nose - a).dot(n));
            if tp.progress >= at - phy::GATE_WINDOW && d0 < 0.0 && d1 >= 0.0 {
                let f = d0 / (d0 - d1);
                // The buoys mark part of the river; the line spans the whole course there.
                let hit = prev.lerp(nose, f);
                let u = track.locate(Vec3::new(hit.x, b.pos.y, hit.y), tp.seg).u;
                if (-phy::FINISH_LINE_REACH..=1.0 + phy::FINISH_LINE_REACH).contains(&u) {
                    exact = Some(clock.t - dt * (1.0 - f));
                }
            }
        }
        // Circuits count a lap when the centre passes the start/finish cross-section; the final one
        // is timed back to the instant the nose touched it.
        if track.looped && b.finished.is_none() && b.lap >= track.laps && exact.is_none() {
            let e = &track.edges[0];
            let (a, c) = (Vec2::new(e.start[0], e.start[2]), Vec2::new(e.end[0], e.end[2]));
            let n = { let p = (c - a).perp().normalize_or_zero(); if p.dot(track.locate(b.pos, 0).forward) < 0.0 { -p } else { p } };
            let past = (b.pos.xz() - a).dot(n) + phy::BOAT_RADIUS * b.info.scale;
            let speed = b.vel.dot(n).max(1.0);
            exact = Some(clock.t - (past / speed).clamp(0.0, dt * 4.0));
        }
        let done = if track.looped {
            b.lap >= track.laps
        } else if track.finish_line.is_some() {
            exact.is_some()
        } else if let Some((p, n, at)) = track.finish {
            (tp.progress >= at - phy::GATE_WINDOW && (b.pos.xz() - p).dot(n) >= 0.0) || (tp.seg >= track.last_seg() && tp.s >= 0.98)
        } else {
            tp.seg >= track.last_seg() && tp.s >= 0.98
        };
        if racing && b.finished.is_none() && done {
            b.finished = Some(exact.unwrap_or(clock.t).max(0.0));
            if b.player && std::env::var_os("RIPTIDE_PROBE").is_some() {
                info!("FINISH at {:.2}s exact {} pos {:.0} {:.0} line {:?}", b.finished.unwrap_or(0.0), exact.is_some(), b.pos.x, b.pos.z, track.finish_line);
            }
        }
    }
}

/// Boat-on-boat hits: push apart, then H2Overdrive's impact rules. A hit harder than
/// `* Impact Speed Max` wipes the boat out, one above `* Impact Speed Min` costs the player
/// `Player Speed Loss`. A boat with its Hull Crusher out blasts whatever it touches.
fn boat_contacts(cheats: Res<Cheats>, tuning: Res<Tuning>, mut boats: Query<&mut Boat>) {
    let g = &tuning.0;
    let no_wipeout = cheats.active(CheatsEffect::NoWipeout);
    let blast_vy = (2.0 * phy::GRAVITY * g.hullcrush_blast_height).sqrt();
    let mut list: Vec<_> = boats.iter_mut().collect();
    let n = list.len();
    let radius = |b: &Boat| {
        let crush = if b.crush == Crush::Off { 1.0 } else { phy::HULLCRUSH_SCALE };
        phy::BOAT_RADIUS * b.info.scale.min(1.3) * crush
    };
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = list.split_at_mut(j);
            let (p, q) = (&mut a[i], &mut b[0]);
            let d = Vec2::new(q.pos.x - p.pos.x, q.pos.z - p.pos.z);
            let r = radius(p) + radius(q);
            let len = d.length();
            if len <= 0.01 || len >= r {
                continue;
            }
            let normal = d / len;
            let push = normal * (r - len) * 0.5;
            p.pos.x -= push.x;
            p.pos.z -= push.y;
            q.pos.x += push.x;
            q.pos.z += push.y;
            let rel = (q.vel - p.vel).dot(normal);
            if rel >= 0.0 {
                continue;
            }
            // Hull Crusher blasts the other boat away and wipes it out.
            let (pc, qc) = (p.crush.smashing(), q.crush.smashing());
            if pc != qc {
                if pc { p.smashes += 1 } else { q.smashes += 1 }
                let (victim, dir) = if pc { (&mut **q, normal) } else { (&mut **p, -normal) };
                victim.vel = dir * g.hullcrush_blast_speed;
                victim.speed *= 0.3;
                victim.vy = blast_vy;
                victim.airborne = true;
                if !(victim.player && no_wipeout) {
                    victim.wipeout = phy::WIPEOUT_TIME;
                }
                continue;
            }
            let imp = normal * rel * 0.5;
            p.vel += imp;
            q.vel -= imp;
            // A boosting boat ramming another: its closing speed counts `Impact Speed Mult` times (the
            // original's multiplier, kept off ordinary bumps where it wiped out starting packs); past the
            // victim's Impact Speed Max the victim wipes out and is thrown up and away.
            let (pb, qb) = (p.boosting || p.super_time > 0.0, q.boosting || q.super_time > 0.0);
            // Only where the player is in it (ramming or rammed): AI-on-AI rams wiped out so many
            // boats that whole fields missed the arcade time limit (24 courses: 155 -> 143 finishers).
            if pb != qb && (p.player || q.player) {
                let (rammer, victim, dir) = if pb { (&mut **p, &mut **q, normal) } else { (&mut **q, &mut **p, -normal) };
                let max = if victim.player { g.player_impact_speed_max } else { g.ai_impact_speed_max };
                if -rel / phy::SPEED_SCALE * g.impact_speed_mult >= max && !(victim.player && no_wipeout) {
                    rammer.rams += 1;
                    victim.vel += dir * -rel * phy::BOOST_RAM_PUSH;
                    victim.vy = victim.vy.max(phy::BOOST_RAM_HOP);
                    victim.airborne = true;
                    victim.wipeout = phy::WIPEOUT_TIME;
                    if std::env::var_os("RIPTIDE_PROBE").is_some() {
                        info!("BOOST RAM closing {:.0} victim player {}", -rel, victim.player);
                    }
                    continue;
                }
            }
            // Closing speed as-is: scaling it by `Impact Speed Mult` wiped out whole starting packs.
            let impact = -rel / phy::SPEED_SCALE;
            for boat in [&mut **p, &mut **q] {
                let (min, max) = if boat.player {
                    (g.player_impact_speed_min, g.player_impact_speed_max)
                } else {
                    (g.ai_impact_speed_min, g.ai_impact_speed_max)
                };
                if impact >= max && !(boat.player && no_wipeout) {
                    boat.wipeout = phy::WIPEOUT_TIME;
                } else if impact >= min && boat.player {
                    boat.speed *= 1.0 - g.player_speed_loss;
                }
            }
        }
    }
}

fn pickups(
    time: Res<Time>,
    tuning: Res<Tuning>,
    mut items: Query<(&mut Pickup, &mut Transform, &mut Visibility)>,
    mut boats: Query<(Entity, &mut Boat), Without<crate::net::Remote>>,
) {
    let g = &tuning.0;
    let dt = time.delta_secs();
    let t = time.elapsed_secs();
    let me = boats.iter().find(|(_, b)| b.player).map(|(e, _)| e);
    for (mut pk, mut tf, mut vis) in &mut items {
        for (_, left) in &mut pk.taken {
            *left -= dt;
        }
        pk.taken.retain(|(_, left)| *left > 0.0);
        // Hidden only for the racer who took it.
        let mine = me.is_some_and(|m| pk.taken.iter().any(|(e, _)| *e == m));
        *vis = if mine { Visibility::Hidden } else { Visibility::Inherited };
        tf.rotation = Quat::from_rotation_y(t * 2.0);
        tf.translation = pk.base + Vec3::Y * (4.0 * (t * 3.0).sin());
        let row = &PICKUPS[pk.row];
        let fuel = row.fuel_global.and_then(|i| g.get(H2_GLOBALS[i].id)).unwrap_or(0.0);
        for (e, mut b) in &mut boats {
            if b.pos.distance(pk.base) < phy::PICKUP_RADIUS && !pk.taken.iter().any(|(t, _)| *t == e) {
                if row.fills_super {
                    b.super_time = (b.super_time + fuel).min(g.boost_fuel_max_super);
                    b.super_pickups += 1;
                } else {
                    b.fuel = (b.fuel + fuel).min(g.boost_fuel_max_regular);
                    b.pickups += 1;
                }
                if b.player && std::env::var_os("RIPTIDE_PROBE").is_some() {
                    info!("PICKUP {} (boost {}, gold {}) fuel {:.1}", row.id, b.pickups, b.super_pickups, b.fuel);
                }
                pk.taken.push((e, phy::PICKUP_RESPAWN));
            }
        }
    }
}

/// How far a boat's hull reaches below its model origin (world units).
#[derive(Component)]
pub struct Hull(pub f32);

/// Where a boat without a rig's boost flames start: its hull's stern (boat-local, scaled).
#[derive(Component)]
pub struct Stern(pub Vec3);

pub(crate) fn place_boats(
    time: Res<Time>,
    tuning: Res<Tuning>,
    h2waves: Option<Res<crate::h2water::H2Waves>>,
    mut boats: Query<(&mut Boat, &mut Transform, Option<&Hull>)>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    let t = time.elapsed_secs();
    for (mut b, mut tf, hull) in &mut boats {
        let norm = (b.speed / phy::SPEED_NORM).clamp(0.0, 1.0);
        // Riding terrain (a ramp, a mound): the hull sits on it, not in it. No water bob, no water
        // draft, and it pitches with the slope (level on a ramp, its nose dug into the surface).
        let on_floor = !b.airborne && b.surface > b.tp.water + 1.0;
        let target_roll = -b.control.steer * norm * 0.22;
        let target_pitch = if b.airborne {
            (b.vy / 900.0).clamp(-0.35, 0.3)
        } else if on_floor {
            b.climb_rate.atan2(b.vel.length().max(1.0)).clamp(-0.6, 0.6)
        } else {
            0.04 + 0.05 * (b.speed / phy::SPEED_NORM).clamp(0.0, 1.3)
        };
        b.roll += (target_roll - b.roll) * (dt * 5.0).min(1.0);
        b.pitch += (target_pitch - b.pitch) * (dt * 4.0).min(1.0);
        // Under H2Overdrive's water shader: sit on the surface it draws (crate::h2water::H2Waves).
        let h2 = h2waves.as_ref().filter(|_| !b.airborne && !on_floor).and_then(|w| w.height(b.pos.xz(), time.elapsed_secs_wrapped()));
        let bob = if let Some(h) = h2 { (h - b.pos.y).clamp(-phy::H2WATER_BOB_MAX, phy::H2WATER_BOB_MAX) } else if b.airborne || on_floor { 0.0 } else {
            // Match the rendered surface. Keeping the rig on the mean water
            // plane submerges its low exhaust nozzles whenever a crest passes.
            let k = std::f32::consts::TAU / phy::WAVE_LENGTH.max(1.0);
            let phase = t * phy::WAVE_SPEED;
            let (x, z) = (b.pos.x * k, b.pos.z * k);
            phy::WAVE_AMPLITUDE * ((x + phase).sin()
                + 0.6 * (z * 0.8 - phase * 1.3).sin()
                + 0.3 * ((x + z) * 0.6 + phase * 0.7).sin())
        };
        // Wiped-out boats spin while they recover.
        let spin = b.wipeout / phy::WIPEOUT_TIME * std::f32::consts::TAU * 2.0;
        // Ride height (boat def Buoyancy Depth Max at rest -> Min once planing at OnPlane
        // Speed): the hull's lowest point sits that deep, so fast boats ride on the water.
        let lift = hull.map_or(0.0, |h| {
            let d = b.info.def;
            let plane = (b.speed.max(0.0) / phy::SPEED_SCALE / d.onplane_speed.max(1.0)).clamp(0.0, 1.0);
            let draft = if on_floor { 0.0 } else { d.buoyancy_depth_max + (d.buoyancy_depth_min - d.buoyancy_depth_max) * plane };
            (h.0 - draft).max(0.0)
        });
        tf.translation = b.pos + Vec3::Y * (bob + lift);
        tf.rotation = Quat::from_euler(EulerRot::YXZ, b.yaw + spin, b.pitch, b.roll);
        tf.scale = Vec3::splat(1.0 + (phy::HULLCRUSH_SCALE - 1.0) * b.crush.grown(&tuning));
    }
}

fn chase_camera(
    time: Res<Time>,
    shake: Res<CameraShake>,
    race_clock: Res<RaceClock>,
    view: Res<CamView>,
    track: Res<Track>,
    cheats: Res<Cheats>,
    occluder: Option<Res<Collider>>,
    boats: Query<&Boat>,
    mut cam: Query<&mut Transform, (With<ChaseCam>, Without<Boat>)>,
    mut look: Local<Option<Vec3>>,
) {
    let Some(b) = boats.iter().find(|b| b.player) else { return };
    let Ok(mut tf) = cam.single_mut() else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let size = b.info.scale.max(1.0);
    let zoom = cheats.zoom() * view.scale();
    let heading = Vec3::new(-b.yaw.sin(), 0.0, -b.yaw.cos());
    let speed_pull = (b.speed / phy::SPEED_NORM).clamp(0.0, 1.6);
    let want = b.pos - heading * (phy::CAM_BACK + phy::CAM_BACK_SPEED * speed_pull) * size.sqrt() * zoom
        + Vec3::Y * (phy::CAM_UP + phy::CAM_UP_SPEED * speed_pull) * zoom;
    let k = (dt * phy::CAM_FOLLOW_RATE).min(1.0);
    tf.translation = tf.translation.lerp(want, k);
    // Stay inside the river corridor so cliffs never swallow the view (open water has none).
    // Not on H2Overdrive courses: their collision mesh keeps the camera off the terrain below, and
    // in wide water (Wild America's lake) the clamp left the camera far behind a boat off the line.
    let trusted = occluder.as_ref().is_some_and(|c| c.trusted);
    let at = track.locate(tf.translation, b.tp.seg);
    if !track.open && !trusted && (at.u < 0.02 || at.u > 0.98) {
        let on = track.point_at(at, at.u.clamp(0.02, 0.98));
        tf.translation.x = on.x;
        tf.translation.z = on.z;
    }
    // Never dip under the water surface.
    tf.translation.y = tf.translation.y.max(b.tp.water + 18.0);
    // Hydro Thunder courses overhang the river (decks, bridges): keep the camera in front of
    // whatever lies between it and the boat.
    if let Some(occ) = occluder {
        let eye = b.pos + Vec3::Y * 18.0;
        if let Some(t) = occ.hit(eye, tf.translation) {
            tf.translation = eye + (tf.translation - eye) * (t - 0.05).max(0.15);
        }
        // Keep a little room from walls so a surface never fills the view.
        if let Some((push, _)) = occ.walls(tf.translation, phy::CAM_RADIUS, tf.translation.y) {
            tf.translation.x += push.x;
            tf.translation.z += push.y;
        }
    }
    // Level event camera shake (the dam collapse, rock slides): a jitter fading out.
    if race_clock.t < shake.until && shake.amp > 0.0 {
        let left = (shake.until - race_clock.t) / (shake.until - shake.start).max(1e-3);
        let t = race_clock.t * 37.0;
        tf.translation += Vec3::new(t.sin(), (t * 1.31).cos(), (t * 0.77).sin()) * shake.amp * left;
    }
    let target = b.pos + heading * phy::CAM_LOOK_AHEAD + Vec3::Y * 18.0;
    let l = look.get_or_insert(target);
    *l = l.lerp(target, (dt * 8.0).min(1.0));
    tf.look_at(*l, Vec3::Y);
    if std::env::var_os("RIPTIDE_DEBUG").is_some() && (time.elapsed_secs() * 2.0) as u32 != ((time.elapsed_secs() - dt) * 2.0) as u32 {
        info!("player {:?} speed {:.0} vy {:.0} air {} seg {} u {:.2} water {:.0} wipeout {:.1} | cam {:?}", b.pos, b.speed, b.vy, b.airborne, b.tp.seg, b.tp.u, b.tp.water, b.wipeout, tf.translation);
    }
}

fn hud(
    clock: Res<RaceClock>,
    online: Res<crate::net::Online>,
    timer: Option<Res<ArcadeTimer>>,
    track: Res<Track>,
    tuning: Res<Tuning>,
    boats: Query<(Entity, &Boat)>,
    mut texts: Query<(&HudText, &mut Text)>,
    mut bar: Query<(&mut Node, &mut BackgroundColor), With<BoostBar>>,
    mut super_notice: Local<SuperNotice>,
) {
    let g = &tuning.0;
    let Some((pe, p)) = boats.iter().find(|(_, b)| b.player) else { return };
    if clock.t < 0.0 {
        // Cheats can fill gold boost during the prerace countdown. Start the notice when
        // racing begins, so it is not spent before the player sees the GO card.
        super_notice.last = 0.0;
        super_notice.until = 0.0;
    } else {
        // Only when gold boost switches on: cheats and pickups top it up while it runs.
        if p.super_time > 0.0 && super_notice.last <= 0.0 {
            super_notice.until = clock.t + 1.5;
        }
        super_notice.last = p.super_time;
    }
    // Finished boats rank by finish order, the rest by progress.
    let mut order: Vec<(f32, Entity)> = boats
        .iter()
        .map(|(e, b)| {
            let score = match clock.finish_order.iter().position(|x| *x == e) {
                Some(i) => 1e9 - i as f32,
                None => track.race_distance(b.lap, b.tp.progress),
            };
            (score, e)
        })
        .collect();
    order.sort_by(|a, b| b.0.total_cmp(&a.0));
    let place = order.iter().position(|(_, e)| *e == pe).unwrap_or(0) + 1;
    let fmt = |t: f32| {
        let t = t.max(0.0);
        format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0)
    };
    for (kind, mut text) in &mut texts {
        let s = match kind {
            HudText::Position => format!("POS {place}/{}", order.len()),
            HudText::Timer => {
                let t = p.finished.unwrap_or(clock.t);
                let left = timer.as_ref().filter(|t| t.enabled).map(|t| format!("   TIME {:.0}", t.left.ceil())).unwrap_or_default();
                if track.looped {
                    format!("{}   LAP {}/{}{left}", fmt(t), (p.lap + 1).min(track.laps), track.laps)
                } else {
                    format!("{}   {:.0}%{left}", fmt(t), (p.tp.progress / track.length() * 100.0).clamp(0.0, 100.0))
                }
            }
            HudText::Speed => format!("{:.0} MPH", p.vel.length() * g.digital_mph_scale_factor),
            HudText::Boat => format!("{} ({})", p.info.name, p.info.game),
            HudText::Center => {
                if online.waiting() {
                    "Waiting for every racer to load the course...".to_string()
                } else if clock.t < -phy::COUNTDOWN {
                    "GET READY".to_string()
                } else if clock.t < 0.0 {
                    let n = (-clock.t).ceil() as i32;
                    if n <= 3 { n.to_string() } else { String::new() }
                } else if clock.t < 1.2 {
                    // The original GO card is drawn by hud::banners.
                    String::new()
                } else if let Some(t) = p.finished {
                    let suffix = match place {
                        1 => "st",
                        2 => "nd",
                        3 => "rd",
                        _ => "th",
                    };
                    if online.racing() {
                        format!("FINISHED {place}{suffix}\n{}\n\n{}", fmt(t), online.standings())
                    } else {
                        format!("FINISHED {place}{suffix}\n{}\nR: race again   Esc: menu", fmt(t))
                    }
                } else if p.timed_out {
                    "TIME UP!\nR: race again   Esc: menu".into()
                } else if timer.as_ref().is_some_and(|t| t.banner.is_some()) {
                    // The original TIME EXTENDED card is drawn by hud::banners; no text over it.
                    String::new()
                } else if p.wipeout > 0.0 {
                    "WIPEOUT!".into()
                } else if matches!(p.crush, Crush::Deploy(_)) {
                    "HULL CRUSHER!".into()
                } else if p.super_time > 0.0 && clock.t < super_notice.until {
                    "MEGA BOOST!".into()
                } else {
                    String::new()
                }
            }
        };
        if text.0 != s {
            text.0 = s;
        }
    }
    if let Ok((mut node, mut bg)) = bar.single_mut() {
        let frac = if p.super_time > 0.0 {
            p.super_time / g.boost_fuel_max_super.max(1e-3)
        } else {
            p.fuel / g.boost_fuel_max_regular.max(1e-3)
        };
        node.width = percent(frac.clamp(0.0, 1.0) * 100.0);
        bg.0 = if p.super_time > 0.0 {
            Color::srgb(1.0, 0.8, 0.1)
        } else if p.boosting {
            Color::srgb(0.4, 0.9, 1.0)
        } else {
            Color::srgb(0.2, 0.6, 1.0)
        };
    }
}

fn race_keys(
    input: Input,
    mut view: ResMut<CamView>,
    cheats: Res<Cheats>,
    mut online: ResMut<crate::net::Online>,
    mut next: ResMut<NextState<Screen>>,
    sel: Res<Selection>,
    boats: Query<&Boat>,
    mut clock: ResMut<RaceClock>,
    mut intro: ResMut<Intro>,
    mut sfx: crate::sound::Sfx,
) {
    // Test hook: RIPTIDE_TEST_RESTART=secs restarts the race once, that long into it.
    if let Some(at) = std::env::var("RIPTIDE_TEST_RESTART").ok().and_then(|s| s.parse::<f32>().ok()) {
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if clock.t > at && !online.racing() && !DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
            info!("test restart");
            next.set(Screen::Restart);
        }
    }
    if sel.render_target.is_some() || cheats.menu_open {
        return;
    }
    // Accelerate, boost or start skips the pre-race checklist to the countdown.
    if clock.t < -phy::COUNTDOWN && [ctl::THROTTLE, ctl::BOOST, ctl::MENU_START].iter().any(|k| input.just_pressed(*k)) {
        clock.t = -phy::COUNTDOWN;
        if let Some(e) = intro.0.take() {
            sfx.stop(e);
        }
    }
    if input.just_pressed(ctl::LEAVE_RACE) {
        // Online: quitting before the finish leaves the room; afterwards it's back to the lobby.
        let done = boats.iter().any(|b| b.player && (b.finished.is_some() || b.timed_out));
        if online.racing() && !done {
            online.send(riptide_net::ClientMsg::Leave);
            online.room = None;
        }
        next.set(Screen::Menu);
    }
    if input.just_pressed(ctl::RESTART) && !online.racing() {
        next.set(Screen::Restart);
    }
    if input.just_pressed(ctl::CAMERA) {
        view.0 = (view.0 + 1) % 3;
    }
}

fn aim_point(track: &Track, tp: TrackPos, ahead: f32, lane: f32) -> Vec3 {
    let mut target = tp.progress + ahead;
    let mut seg = tp.seg;
    if track.looped && target > track.length() {
        target -= track.length();
        seg = 0;
    }
    while seg < track.last_seg() && track.dist[seg + 1] < target {
        seg += 1;
    }
    let span = (track.dist[seg + 1] - track.dist[seg]).max(1.0);
    let s = ((target - track.dist[seg]) / span).clamp(0.0, 1.0);
    // `lane` is a fraction of the course's AI band there (the whole width when it has none).
    let [lo, hi] = track.lane_band(seg, s);
    track.point(seg, s, lo + (hi - lo) * lane)
}

fn race_clock(
    time: Res<Time>,
    mut clock: ResMut<RaceClock>,
    boats: Query<(Entity, &Boat)>,
    online: Res<crate::net::Online>,
    autopilot: Option<Res<Autopilot>>,
    mut exit: MessageWriter<AppExit>,
) {
    // Online: the countdown waits until every racer has loaded the course.
    if online.waiting() {
        clock.t = -phy::COUNTDOWN;
        return;
    }
    clock.t += time.delta_secs().min(1.0 / 20.0);
    let mut done: Vec<(f32, Entity)> =
        boats.iter().filter_map(|(e, b)| b.finished.map(|t| (t, e))).filter(|(_, e)| !clock.finish_order.contains(e)).collect();
    done.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (t, e) in done {
        // By finish time: online, another racer's earlier finish can arrive after ours.
        let at = clock.finish_order.iter().position(|x| boats.get(*x).ok().and_then(|(_, b)| b.finished).is_some_and(|f| f > t)).unwrap_or(clock.finish_order.len());
        clock.finish_order.insert(at, e);
        if let Ok((_, b)) = boats.get(e) {
            if b.player {
                // One line for test scripts; capture runs (autopilot) stop here when asked to.
                info!("RESULT finished {} of {} in {:.2}s", at + 1, boats.iter().count(), b.finished.unwrap_or(0.0));
                if autopilot.is_some() && !online.racing() && std::env::var_os("RIPTIDE_EXIT_ON_FINISH").is_some() {
                    exit.write(AppExit::Success);
                }
            }
        }
    }
}

fn sky_follow(cam: Query<&Transform, With<ChaseCam>>, mut sky: Query<&mut Transform, (With<Sky>, Without<ChaseCam>)>) {
    let Ok(c) = cam.single() else { return };
    for mut s in &mut sky {
        s.translation = c.translation;
    }
}

/// Course triangles in a 2D grid: camera line-of-sight tests, and boat-vs-wall collision
/// against the steep ones.
#[derive(Resource)]
pub struct Collider {
    cell: f32,
    grid: std::collections::HashMap<(i32, i32), Vec<u32>>,
    pub(crate) tris: Vec<[Vec3; 3]>,
    /// Height above the waterline the walls are cut at.
    probe: f32,
    /// A second, lower cut (fraction of `probe`; 0 = none), so low walls and slopes catch too.
    low_cut: f32,
    /// The game's own collision mesh (H2Overdrive): never second-guessed.
    trusted: bool,
    /// Per triangle: a giant scripted barrier (see physics.barrier_height).
    barrier: Vec<bool>,
    /// Steep triangles only (walls), same grid layout.
    walls: std::collections::HashMap<(i32, i32), Vec<u32>>,
    /// Upward-facing triangles (floors: ramps, mounds, banks), same grid layout.
    floors: std::collections::HashMap<(i32, i32), Vec<u32>>,
    /// Per triangle: a wall / a floor (as classified for the grids above).
    steep_tri: Vec<bool>,
    pub(crate) floor_tri: Vec<bool>,
    /// Parry collision (crate::collide), switched on per course by `tracks.collision`.
    parry: Option<ParrySets>,
}

/// The course triangles in parry meshes: walls, floors, and sight blockers.
pub struct ParrySets {
    walls: crate::collide::Set,
    floors: crate::collide::Set,
    sight: crate::collide::Set,
}

impl Collider {
    const CELL: f32 = 600.0;

    fn new(model: &riptide_assets::model::Model, scale: f32, steepness: f32) -> Self {
        Self::from_models(std::slice::from_ref(model), scale, steepness)
    }

    fn from_models(models: &[riptide_assets::model::Model], scale: f32, steepness: f32) -> Self {
        Self::build(models, scale, steepness, false)
    }

    fn build(models: &[riptide_assets::model::Model], scale: f32, steepness: f32, upward_only: bool) -> Self {
        // With reliable winding (H2), steep upward faces are ramps; otherwise only gentle ones.
        let floor_min = if upward_only { phy::H2_FLOOR_MIN_NORMAL } else { phy::FLOOR_MIN_NORMAL };
        let mut tris = Vec::new();
        let mut grid: std::collections::HashMap<(i32, i32), Vec<u32>> = Default::default();
        let mut walls: std::collections::HashMap<(i32, i32), Vec<u32>> = Default::default();
        let mut floors: std::collections::HashMap<(i32, i32), Vec<u32>> = Default::default();
        let (mut steep_tri, mut floor_tri) = (Vec::new(), Vec::new());
        for part in models.iter().flat_map(|m| &m.parts) {
            for t in part.indices.chunks_exact(3) {
                let v = [0, 1, 2].map(|k| Vec3::from(part.positions[t[k] as usize]) * scale);
                let id = tris.len() as u32;
                let (lo, hi) = (v[0].min(v[1]).min(v[2]), v[0].max(v[1]).max(v[2]));
                let n = (v[1] - v[0]).cross(v[2] - v[0]).normalize_or_zero();
                // Walls: near-vertical faces only. The H2 mesh flips some triangles' winding, so the
                // normal's sign means nothing (a flat riverbed patch can face "down").
                let steep = n != Vec3::ZERO && n.y.abs() < steepness;
                steep_tri.push(steep);
                floor_tri.push(n.y.abs() >= floor_min);
                for cx in (lo.x / Self::CELL).floor() as i32..=(hi.x / Self::CELL).floor() as i32 {
                    for cz in (lo.z / Self::CELL).floor() as i32..=(hi.z / Self::CELL).floor() as i32 {
                        grid.entry((cx, cz)).or_default().push(id);
                        if steep {
                            walls.entry((cx, cz)).or_default().push(id);
                        }
                        // Upward-facing only where winding is reliable (the H2Overdrive collision mesh):
                        // a tunnel ceiling is not a floor. HT's visual mesh mixes windings.
                        if n.y.abs() >= floor_min {
                            floors.entry((cx, cz)).or_default().push(id);
                        }
                    }
                }
                tris.push(v);
            }
        }
        let up = tris.iter().filter(|t| (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero().y >= phy::FLOOR_MIN_NORMAL).count();
        let down = tris.iter().filter(|t| (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero().y <= -phy::FLOOR_MIN_NORMAL).count();
        let barrier: Vec<bool> = tris
            .iter()
            .map(|t| t.iter().map(|v| v.y).fold(f32::MIN, f32::max) - t.iter().map(|v| v.y).fold(f32::MAX, f32::min) > phy::BARRIER_HEIGHT)
            .collect();
        info!(
            "collider: {} triangles, {} wall cells, {up} facing up, {down} facing down, {} barrier",
            tris.len(),
            walls.len(),
            barrier.iter().filter(|b| **b).count()
        );
        Self { cell: Self::CELL, grid, tris, walls, floors, probe: phy::WALL_PROBE_HEIGHT, low_cut: phy::WALL_LOW_CUT, trusted: false, barrier, steep_tri, floor_tri, parry: None }
    }

    /// First hit along `a -> b` as a fraction of the segment.
    pub(crate) fn hit(&self, a: Vec3, b: Vec3) -> Option<f32> {
        if let Some(p) = &self.parry {
            return p.sight.along(a, b);
        }
        let d = b - a;
        let steps = ((d.x.abs().max(d.z.abs()) / (self.cell * 0.5)).ceil() as usize).max(1);
        let mut seen = std::collections::HashSet::new();
        let mut best: Option<f32> = None;
        for i in 0..=steps {
            let p = a + d * (i as f32 / steps as f32);
            let key = ((p.x / self.cell).floor() as i32, (p.z / self.cell).floor() as i32);
            for &id in self.grid.get(&key).map(Vec::as_slice).unwrap_or(&[]) {
                if !seen.insert(id) {
                    continue;
                }
                if self.barrier[id as usize] {
                    continue; // invisible gates block neither sight lines nor the AI's look-ahead
                }
                if let Some(t) = segment_triangle(a, d, &self.tris[id as usize]) {
                    best = Some(best.map_or(t, |x: f32| x.min(t)));
                }
            }
        }
        best
    }
}

/// Möller–Trumbore, both faces: fraction along `a + t*d` (0..1) where it crosses `tri`.
fn segment_triangle(a: Vec3, d: Vec3, tri: &[Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (tri[1] - tri[0], tri[2] - tri[0]);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-6 {
        return None;
    }
    let inv = 1.0 / det;
    let s = a - tri[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (0.0..=1.0).contains(&t).then_some(t)
}

/// Hydro Thunder sky: the panorama tiles `T?TSKY__A1n` (n = 1, 2, ...) wrapped around a
/// camera-following cylinder, six slots repeating the tiles, plus a cap in the panorama's top
/// colour. The tiles decode upside down (sky at the bottom), so V runs bottom-up.
fn spawn_ht_sky(commands: &mut Commands, models: &mut Models, first: &str, scope: DespawnOnExit<Screen>) {
    let Some(stem) = first.strip_suffix('1') else { return };
    let tiles: Vec<Handle<Image>> = (1..10).map_while(|n| models.ht_texture(&format!("{stem}{n}"))).collect();
    if tiles.is_empty() {
        return;
    }
    const SLOTS: usize = 6;
    const STEPS: usize = 8;
    let r = phy::SKY_RADIUS;
    let (bottom, top) = (-0.15 * r, 0.5 * r);
    let root = commands.spawn((Transform::IDENTITY, Visibility::default(), Sky, scope)).id();
    for slot in 0..SLOTS {
        let mut pos = Vec::new();
        let mut uv = Vec::new();
        let mut idx = Vec::new();
        for k in 0..=STEPS {
            let f = k as f32 / STEPS as f32;
            let a = (slot as f32 + f) / SLOTS as f32 * std::f32::consts::TAU;
            let (x, z) = (a.cos() * r, a.sin() * r);
            pos.extend([[x, bottom, z], [x, top, z]]);
            uv.extend([[f, 0.0], [f, 1.0]]);
            if k < STEPS {
                let b = (k * 2) as u32;
                idx.extend([b, b + 1, b + 2, b + 1, b + 3, b + 2]);
            }
        }
        let n = pos.len();
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_indices(Indices::U32(idx));
        let material = StandardMaterial {
            base_color_texture: Some(tiles[slot % tiles.len()].clone()),
            unlit: true,
            fog_enabled: false,
            double_sided: true,
            cull_mode: None,
            ..default()
        };
        let (mesh, material) = (models.meshes.add(mesh), models.materials.add(material));
        commands.entity(root).with_children(|c| {
            c.spawn((Mesh3d(mesh), MeshMaterial3d(material), NoFrustumCulling, NotShadowCaster));
        });
    }
    // Cap: the average colour of the panorama's top edge (the last decoded row).
    let cap = models
        .images
        .get(&tiles[0])
        .and_then(|img| {
            let (w, h) = (img.width() as usize, img.height() as usize);
            let row = img.data.as_ref()?.get((h - 1) * w * 4..h * w * 4)?;
            let mut sum = [0u32; 3];
            for px in row.chunks_exact(4) {
                for c in 0..3 {
                    sum[c] += px[c] as u32;
                }
            }
            Some(Color::srgb_u8((sum[0] / w as u32) as u8, (sum[1] / w as u32) as u8, (sum[2] / w as u32) as u8))
        })
        .unwrap_or(Color::srgb(0.1, 0.2, 0.7));
    let disk = Mesh::from(Circle::new(r)).rotated_by(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
    let material = StandardMaterial { base_color: cap, unlit: true, fog_enabled: false, double_sided: true, cull_mode: None, ..default() };
    let (mesh, material) = (models.meshes.add(disk), models.materials.add(material));
    commands.entity(root).with_children(|c| {
        c.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::from_xyz(0.0, top, 0.0), NoFrustumCulling, NotShadowCaster));
    });
}

/// A checkpoint: race distance where it is crossed, seconds it adds.
struct Gate {
    /// Race distance of the gate (orders lap gates; the plane test does the triggering).
    at: f32,
    /// Gate plane: the tripwire position and the course direction there (XZ).
    point: Vec2,
    normal: Vec2,
    secs: f32,
    taken: bool,
}

/// The H2Overdrive arcade clock for the player (H2 levels with checkpoint data).
#[derive(Resource)]
pub struct ArcadeTimer {
    pub(crate) enabled: bool,
    pub(crate) left: f32,
    gates: Vec<Gate>,
    /// Centre-screen message and seconds it stays up.
    pub(crate) banner: Option<(String, f32)>,
}

fn arcade_timer(
    time: Res<Time>,
    clock: Res<RaceClock>,
    track: Res<Track>,
    cheats: Res<Cheats>,
    autopilot: Option<Res<Autopilot>>,
    mut timer: ResMut<ArcadeTimer>,
    mut boats: Query<&mut Boat>,
    mut exit: MessageWriter<AppExit>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    if let Some((_, t)) = &mut timer.banner {
        *t -= dt;
        if *t <= 0.0 {
            timer.banner = None;
        }
    }
    if !timer.enabled || clock.t < 0.0 {
        return;
    }
    let Some(mut p) = boats.iter_mut().find(|b| b.player) else { return };
    if p.finished.is_some() || p.timed_out {
        return;
    }
    let d = track.race_distance(p.lap, p.tp.progress);
    // A gate fires the moment the boat crosses its plane (once it is in that gate's stretch of
    // the race), not when centre-line distance catches up: on the outside of a bend that lagged.
    let pos = p.pos.xz();
    let gained: f32 = timer
        .gates
        .iter_mut()
        .filter(|g| !g.taken && d >= g.at - phy::GATE_WINDOW && (pos - g.point).dot(g.normal) >= 0.0).map(|g| {
        g.taken = true;
        g.secs
    }).sum();
    if gained > 0.0 && std::env::var_os("RIPTIDE_DEBUG").is_some() {
        info!("checkpoint +{gained} at {d:.0}, t={:.1}", clock.t);
    }
    if gained > 0.0 {
        timer.left += gained;
        timer.banner = Some((format!("CHECKPOINT!  +{gained:.0}"), 2.0));
    }
    if !cheats.active(CheatsEffect::NoTimeLimit) {
        timer.left -= dt;
    }
    if timer.left <= 0.0 {
        timer.left = 0.0;
        p.timed_out = true;
        let pct = (d / race_length(&track) * 100.0).clamp(0.0, 100.0);
        info!("RESULT time up at {pct:.0}% after {:.2}s", clock.t);
        if autopilot.is_some() && std::env::var_os("RIPTIDE_EXIT_ON_FINISH").is_some() {
            exit.write(AppExit::Success);
        }
    }
}

/// Chase camera view, cycled with the camera key (0 normal, 1 near, 2 far); kept between races.
#[derive(Resource, Default)]
pub struct CamView(pub u8);

impl CamView {
    fn scale(&self) -> f32 {
        match self.0 {
            1 => phy::CAM_VIEW_NEAR,
            2 => phy::CAM_VIEW_FAR,
            _ => 1.0,
        }
    }
}

/// The player's engine loops (from its boatdef's `Engine Def`) and the countdown voice.
/// The pre-race checklist playing (cut short when the player skips it).
#[derive(Resource, Default)]
pub struct Intro(Option<Entity>);

fn start_race_audio(
    mut sfx: crate::sound::Sfx,
    boats: Query<(Entity, &Boat)>,
    sel: Res<Selection>,
    content: Res<crate::content::Content>,
) {
    // The track's music (tracks sheet).
    let music = content.tracks.get(sel.level).and_then(|c| TRACKS.iter().find(|t| t.id == c.id)).and_then(|t| t.music);
    if let Some(m) = music {
        sfx.music(m, phy::MUSIC_VOLUME, DespawnOnExit(Screen::Race));
    }
    if let Some((e, b)) = boats.iter().find(|(_, b)| b.player) {
        if let Some(engine) = b.info.def.engine_def {
            crate::sound::start_engine(&mut sfx, e, engine, DespawnOnExit(Screen::Race));
        }
    }
}

fn engine_audio(boats: Query<&Boat>, mut layers: Query<(&crate::sound::EngineLayer, &crate::sound::EngineOf, &mut bevy::audio::AudioSink)>) {
    crate::sound::drive_engines(|e| boats.get(e).ok().map(|b| b.speed.abs() / b.info.def.max_speed_l1.max(1.0)), &mut layers);
}

/// What the player's boat did last frame, to turn state changes into sound events.
#[derive(Default)]
struct HeardState {
    /// Race clock at the last wall-impact sound.
    wall_sound: f32,
    /// Race clock last frame.
    t: f32,
    /// The 3-2-1 countdown voice has played (when the clock reaches the countdown).
    counted: bool,
    boosting: bool,
    super_on: bool,
    smashes: u32,
    wall_hits: u32,
    fuel: f32,
    pickups: u32,
    super_pickups: u32,
    wipeout: bool,
    crushing: bool,
    stowing: bool,
    riff: Option<Entity>,
    lap: u32,
    airborne: bool,
    finished: bool,
    timed_out: bool,
    gates: usize,
    low_time: bool,
    started: bool,
    /// The Go / No Go systems checklist has played (just after GO).
    checklist: bool,
    rams: u32,
}

fn race_audio(
    mut sfx: crate::sound::Sfx,
    mut intro: ResMut<Intro>,
    track: Res<Track>,
    clock: Res<RaceClock>,
    timer: Option<Res<ArcadeTimer>>,
    tuning: Res<Tuning>,
    boats: Query<&Boat>,
    mut last: Local<HeardState>,
) {
    use crate::sheets::sound_events_ids as ev;
    let Some(p) = boats.iter().find(|b| b.player) else { return };
    let g = &tuning.0;
    // A new race (or a restart): the clock went back. Start over, or the last race's finish,
    // laps and countdown would silence this one's.
    if !last.started || clock.t < last.t - 0.5 {
        if let Some(old) = last.riff.take() {
            sfx.stop(old);
        }
        if let Some(old) = intro.0.take() {
            sfx.stop(old);
        }
        *last = HeardState { fuel: p.fuel, started: true, ..default() };
    }
    last.t = clock.t;
    if !last.counted && clock.t >= -phy::COUNTDOWN - 1e-3 {
        last.counted = true;
        sfx.event(ev::COUNTDOWN);
    }
    // The Go / No Go systems checklist (picked by the boat's engine, boat def Hydro Engine) plays
    // once the race is under way, not over the countdown (user, 2026-10-05).
    if !last.checklist && clock.t >= phy::CHECKLIST_AFTER_GO {
        last.checklist = true;
        let ev = if p.info.def.hydro_engine { ev::RACE_CHECKLIST_HYDRO } else { ev::RACE_CHECKLIST_GAS };
        intro.0 = sfx.event_once(ev, DespawnOnExit(Screen::Race));
    }
    if p.rams > last.rams {
        sfx.event(ev::HULL_IMPACT);
    }
    last.rams = p.rams;
    // Pickups by count, not by the tank rising: a full tank (or the infinite boost cheat) still
    // hears them.
    if p.pickups > last.pickups {
        sfx.event(ev::BOOST_PICKUP);
    }
    if p.super_pickups > last.super_pickups {
        sfx.event(ev::SUPER_PICKUP);
    }
    if p.fuel < g.boost_fuel_max_regular * 0.15 && last.fuel >= g.boost_fuel_max_regular * 0.15 && clock.t > 0.0 {
        sfx.event(ev::BOOST_LOW);
    }
    // The boat's own boosters opening and closing, and the gold rocket.
    let super_on = p.super_time > 0.0;
    let boost_on = p.boosting && !super_on;
    if boost_on != last.boosting {
        let def = if boost_on { p.info.row.boost_deploy } else { p.info.row.boost_stow };
        if let Some(d) = def {
            sfx.def(d, 1.0);
        }
    }
    if super_on != last.super_on {
        let row = p.info.row;
        let def = if super_on { row.sboost_deploy.or(row.boost_deploy) } else { row.sboost_stow.or(row.boost_stow) };
        if let Some(d) = def {
            sfx.def(d, 1.0);
        }
        sfx.event(if super_on { ev::SUPER_START } else { ev::SUPER_END });
    }
    if p.smashes > last.smashes {
        sfx.event(ev::HULLCRUSH_ATTACK);
    }
    // Scraping along a wall counts many hits: one impact sound per wall_hit_gap, turned down.
    if p.wall_hits > last.wall_hits && clock.t - last.wall_sound >= phy::WALL_HIT_GAP {
        last.wall_sound = clock.t;
        sfx.event_gain(ev::WALL_HIT, phy::WALL_HIT_VOLUME);
    }
    last.boosting = boost_on;
    last.super_on = super_on;
    last.smashes = p.smashes;
    last.wall_hits = p.wall_hits;
    if p.wipeout > 0.0 && !last.wipeout {
        sfx.event(ev::WIPEOUT);
    }
    let crushing = p.crush != Crush::Off;
    if crushing && !last.crushing {
        sfx.event(ev::HULLCRUSH_START);
        sfx.event(ev::HULLCRUSH_VOICE);
        if let Some(old) = last.riff.take() {
            sfx.stop(old);
        }
        last.riff = sfx.event_loop(ev::HULLCRUSH_RIFF, DespawnOnExit(Screen::Race));
    }
    // Retracting: the riff ends with the metal clank.
    let stowing = matches!(p.crush, Crush::Stow(_));
    if stowing && !last.stowing {
        sfx.event(ev::HULLCRUSH_STOW);
    }
    if !crushing || stowing {
        if let Some(e) = last.riff.take() {
            sfx.stop(e);
        }
    }
    last.stowing = stowing;
    if track.looped && p.lap > last.lap && p.lap + 1 == track.laps {
        sfx.event(ev::FINAL_LAP);
    }
    if last.airborne && !p.airborne && p.vy.abs() > 60.0 {
        sfx.event(ev::SPLASH_LARGE);
    }
    if p.finished.is_some() && !last.finished {
        sfx.event(ev::FINISH);
        let first = clock.finish_order.first().is_some_and(|e| boats.get(*e).is_ok_and(|b| b.player));
        sfx.event(if first { ev::FINISH_FIRST_MUSIC } else { ev::FINISH_MUSIC });
    }
    if p.timed_out && !last.timed_out {
        sfx.event(ev::TIME_UP);
    }
    if let Some(t) = timer.as_ref().filter(|t| t.enabled) {
        let taken = t.gates.iter().filter(|g| g.taken).count();
        if taken > last.gates {
            sfx.event(ev::CHECKPOINT);
        }
        let low = t.left < 10.0 && clock.t > 0.0;
        if low && !last.low_time {
            sfx.event(ev::CHECKPOINT_LOW);
        }
        last.gates = taken;
        last.low_time = low;
    }
    last.fuel = p.fuel;
    last.pickups = p.pickups;
    last.super_pickups = p.super_pickups;
    last.wipeout = p.wipeout > 0.0;
    last.crushing = crushing;
    last.lap = p.lap;
    last.airborne = p.airborne;
    last.finished = p.finished.is_some();
    last.timed_out = p.timed_out;
}

impl Collider {
    /// No riverbed at `p` (on the waterline): no surface between `nav_land_depth` below it and
    /// `nav_land_cap` above it. Behind a Hydro Thunder bank there is no surface at all. The
    /// waterline is interpolated between cross-sections, so the window is generous (rivers climb
    /// in steps). For the navigation line (nav.rs).
    /// The heights above the waterline walls are cut at: `probe`, and the lower cut if any.
    pub(crate) fn cuts(&self) -> Vec<f32> {
        if self.low_cut > 0.0 { vec![self.probe, self.probe * self.low_cut] } else { vec![self.probe] }
    }

    pub(crate) fn land(&self, p: Vec3) -> bool {
        self.floor(p.x, p.z, p.y + phy::NAV_LAND_CAP).is_none_or(|y| y < p.y - phy::NAV_LAND_DEPTH)
    }

    /// The wall triangle [`Self::blocked`] finds at `p` (classic collision), for diagnostics.
    pub(crate) fn blocker(&self, p: Vec3, r: f32, course: Vec2) -> Option<(f32, u32, [Vec3; 3])> {
        self.cuts()
            .into_iter()
            .find_map(|h| self.walls_id(p, r, p.y + h, Some(course)).map(|(_, _, id)| (h, id, self.tris[id as usize])))
    }

    /// Would a boat of radius `r` at `p` (on the water) touch a wall, by the same test the boat
    /// physics uses (parry hull, or walls cut at two heights)? For the navigation line (nav.rs).
    pub(crate) fn blocked(&self, p: Vec3, r: f32, course: Vec2) -> bool {
        if self.parry.is_some() {
            return self.hull(p, r, Some(course)).is_some();
        }
        self.cuts().into_iter().any(|h| self.walls_id(p, r, p.y + h, Some(course)).is_some())
    }

    /// Push a circle of radius `r` at `p` out of the walls cut at height `y`: the summed push
    /// (XZ) and the normal of the deepest contact, or `None` when clear.
    fn walls(&self, p: Vec3, r: f32, y: f32) -> Option<(Vec2, Vec2)> {
        if let Some(pq) = &self.parry {
            return pq.walls.push_out(Vec3::new(p.x, y, p.z), r * 0.5, r, |_| false).map(|(push, n, _)| (push, n));
        }
        self.walls_id(p, r, y, None).map(|(push, n, _)| (push, n))
    }

    /// [`Self::walls`] plus the triangle index of the deepest contact.
    /// With `course` (the track direction), barriers facing along the course are ignored.
    fn walls_id(&self, p: Vec3, r: f32, y: f32, course: Option<Vec2>) -> Option<(Vec2, Vec2, u32)> {
        let q = Vec2::new(p.x, p.z);
        let (c0, c1) = (((q - r) / self.cell).floor(), ((q + r) / self.cell).floor());
        let mut seen = std::collections::HashSet::new();
        let (mut push, mut best) = (Vec2::ZERO, (0.0f32, Vec2::ZERO, 0u32));
        for cx in c0.x as i32..=c1.x as i32 {
            for cz in c0.y as i32..=c1.y as i32 {
                for &id in self.walls.get(&(cx, cz)).map(Vec::as_slice).unwrap_or(&[]) {
                    if !seen.insert(id) {
                        continue;
                    }
                    let Some((a, b)) = slice(&self.tris[id as usize], y) else { continue };
                    if let (true, Some(f)) = (self.barrier[id as usize], course) {
                        let along = (b - a).normalize_or_zero();
                        if along.dot(f).abs() < 0.5 {
                            continue; // crosses the course: a scripted gate
                        }
                    }
                    // Closest point of the cut segment to the boat.
                    let ab = b - a;
                    let t = ((q - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
                    let d = q - (a + ab * t);
                    let dist = d.length();
                    if dist < r && dist > 1e-4 {
                        let depth = r - dist;
                        let n = d / dist;
                        push += n * depth;
                        if depth > best.0 {
                            best = (depth, n, id);
                        }
                    }
                }
            }
        }
        (best.0 > 0.0).then_some((push, best.1, best.2))
    }
}

/// Where a triangle crosses the horizontal plane at `y`, as a 2D (XZ) segment.
fn slice(t: &[Vec3; 3], y: f32) -> Option<(Vec2, Vec2)> {
    let mut pts = [Vec2::ZERO; 2];
    let mut n = 0;
    for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
        if (a.y - y) * (b.y - y) < 0.0 && n < 2 {
            let f = (y - a.y) / (b.y - a.y);
            let p = a.lerp(b, f);
            pts[n] = Vec2::new(p.x, p.z);
            n += 1;
        }
    }
    (n == 2).then_some((pts[0], pts[1]))
}

/// A 64px sky cubemap: ground below the horizon, horizon-to-zenith gradient above, and a sun
/// glow toward `sun` (unit vector toward the sun). Colours from the `physics` sheet.
fn sky_cubemap(sun: Vec3, sun_color: Color) -> Image {
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
    const N: usize = 64;
    let c = |v: &[f32]| Vec3::new(v.first().copied().unwrap_or(0.5), v.get(1).copied().unwrap_or(0.5), v.get(2).copied().unwrap_or(0.5));
    let (zenith, horizon, ground) = (c(phy::ENV_ZENITH), c(phy::ENV_HORIZON), c(phy::ENV_GROUND));
    let sun_rgb = Vec3::from_array(sun_color.to_linear().to_f32_array_no_alpha());
    let mut data = Vec::with_capacity(N * N * 6 * 16);
    for face in 0..6 {
        for y in 0..N {
            for x in 0..N {
                let (u, v) = ((x as f32 + 0.5) / N as f32 * 2.0 - 1.0, (y as f32 + 0.5) / N as f32 * 2.0 - 1.0);
                let d = match face {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                }
                .normalize();
                let mut col = if d.y >= 0.0 { horizon.lerp(zenith, d.y.sqrt()) } else { horizon.lerp(ground, (-d.y * 4.0).min(1.0)) };
                let glow = d.dot(sun).max(0.0);
                col += sun_rgb * (glow.powf(64.0) * 6.0 + glow.powf(8.0) * 0.3);
                for ch in [col.x, col.y, col.z, 1.0] {
                    data.extend_from_slice(&ch.to_le_bytes());
                }
            }
        }
    }
    let mut image = Image::new(
        Extent3d { width: N as u32, height: N as u32, depth_or_array_layers: 6 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    image
}

impl Collider {
    /// Highest floor under (x, z) that is no higher than `max_y`.
    pub(crate) fn floor(&self, x: f32, z: f32, max_y: f32) -> Option<f32> {
        if let Some(p) = &self.parry {
            return p.floors.down(Vec3::new(x, max_y, z), 1.0e6).map(|d| max_y - d);
        }
        let key = ((x / self.cell).floor() as i32, (z / self.cell).floor() as i32);
        let q = Vec2::new(x, z);
        let mut best: Option<f32> = None;
        for &id in self.floors.get(&key).map(Vec::as_slice).unwrap_or(&[]) {
            let t = &self.tris[id as usize];
            let (a, b, c) = (t[0].xz(), t[1].xz(), t[2].xz());
            let d = (b - a).perp_dot(c - a);
            if d.abs() < 1e-6 {
                continue;
            }
            let u = (b - q).perp_dot(c - q) / d;
            let v = (c - q).perp_dot(a - q) / d;
            let w = 1.0 - u - v;
            if u < 0.0 || v < 0.0 || w < 0.0 {
                continue;
            }
            let y = t[0].y * u + t[1].y * v + t[2].y * w;
            if y <= max_y && best.is_none_or(|b| y > b) {
                best = Some(y);
            }
        }
        best
    }
}

/// Hackworld's objects (the `hackworld` sheet): ramps and props are drawn and solid, boosts are
/// pickups. Ramps ride as floors through the collider's steep-floor rule.
fn spawn_hackworld(commands: &mut Commands, models: &mut Models, scope: DespawnOnExit<Screen>) {
    let mut solid = Vec::new();
    // Ramps are surfaces you ride, never walls: their side and lip faces caught the wall probe
    // mid-climb. They go first so their triangles are the lowest ids.
    let mut ramps = Vec::new();
    for o in HACKWORLD {
        let at = Vec3::new(o.x, o.y, o.z);
        let rot = Quat::from_rotation_y(o.yaw.to_radians());
        if o.kind == HackworldKind::Booster {
            let Some(row) = o.pickup else { continue };
            let Some(p) = PICKUPS[row].model.strip_prefix("lux:mesh32.").and_then(|m| models.lux(m)) else { continue };
            let e = commands
                .spawn((Transform::from_translation(at), Visibility::default(), Pickup { row, taken: Vec::new(), base: at }, scope.clone()))
                .id();
            attach(commands, e, &p);
            continue;
        }
        let Some(name) = o.model.strip_prefix("lux:mesh32.") else { continue };
        if let Some(p) = models.lux(name) {
            let e = commands
                .spawn((Transform::from_translation(at).with_rotation(rot).with_scale(Vec3::splat(o.scale)), Visibility::default(), scope.clone()))
                .id();
            attach(commands, e, &p);
        }
        if !o.solid {
            continue;
        }
        let mesh = models.content.lux.get(&format!("mesh32.{name}")).and_then(|b| riptide_assets::h2mesh::decode_mesh(name, b).ok());
        if let Some(mut m) = mesh {
            for part in &mut m.parts {
                for p in &mut part.positions {
                    *p = (rot * (Vec3::from(*p) * o.scale) + at).to_array();
                }
            }
            if o.kind == HackworldKind::Ramp { ramps.push(m) } else { solid.push(m) }
        }
    }
    // Tall props are scenery, not scripted gates: keep every face solid.
    let ramp_tris: usize = ramps.iter().flat_map(|m| &m.parts).map(|p| p.indices.len() / 3).sum();
    ramps.append(&mut solid);
    let mut col = Collider { trusted: true, ..Collider::build(&ramps, 1.0, phy::WALL_STEEPNESS, true) };
    col.barrier.fill(false);
    for cell in col.walls.values_mut() {
        cell.retain(|&id| id as usize >= ramp_tris);
    }
    let n = ramp_tris.min(col.steep_tri.len());
    col.steep_tri[..n].fill(false);
    commands.insert_resource(col);
}

/// A wall and floor closing a sky dome below its rim, coloured like the dome at its horizon:
/// the average of the dome texture at the rim vertices (the lowest 3% of the dome's height).
fn sky_skirt(models: &Models, mesh: &str) -> Option<(Mesh, Color)> {
    let lux = &models.content.lux;
    let model = riptide_assets::h2mesh::decode_mesh(mesh, lux.get(&format!("mesh32.{mesh}"))?).ok()?;
    let (lo, hi) = model.bounds()?;
    let rim_y = lo[1] + (hi[1] - lo[1]) * 0.03;
    let radius = (hi[0] - lo[0]).max(hi[2] - lo[2]) * 0.5;
    let (mut sum, mut n) = (Vec3::ZERO, 0.0f32);
    for part in &model.parts {
        let Some(img) = part.texture.as_ref().and_then(|t| lux.get(&format!("txtr1.{t}"))).and_then(|b| riptide_assets::image::decode_txtr(b).ok()) else {
            continue;
        };
        for (p, uv) in part.positions.iter().zip(&part.uvs) {
            if p[1] > rim_y {
                continue;
            }
            let x = ((uv[0].rem_euclid(1.0)) * (img.width - 1) as f32) as u32;
            let y = ((uv[1].rem_euclid(1.0)) * (img.height - 1) as f32) as u32;
            let px = &img.rgba[((y * img.width + x) * 4) as usize..][..3];
            sum += Vec3::new(px[0] as f32, px[1] as f32, px[2] as f32) / 255.0;
            n += 1.0;
        }
    }
    if n == 0.0 {
        return None;
    }
    let c = sum / n;
    // Wall from the rim down to one radius below, then a floor; 48 sides, viewed from inside.
    let sides = 48;
    let (top, bottom) = (lo[1] + 1.0, lo[1] - radius);
    let mut positions = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for i in 0..sides {
        let a = i as f32 / sides as f32 * std::f32::consts::TAU;
        let (x, z) = (a.cos() * radius, a.sin() * radius);
        positions.push([x, top, z]);
        positions.push([x, bottom, z]);
    }
    let centre = positions.len() as u32;
    positions.push([0.0, bottom, 0.0]);
    for i in 0..sides as u32 {
        let j = (i + 1) % sides as u32;
        let (t0, b0, t1, b1) = (i * 2, i * 2 + 1, j * 2, j * 2 + 1);
        indices.extend([t0, b0, b1, t0, b1, t1]);
        indices.extend([b0, centre, b1]);
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    let mut m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    m.insert_indices(Indices::U32(indices));
    Some((m, Color::srgb(c.x, c.y, c.z)))
}

impl Collider {
    /// Switch this course to parry collision (crate::collide). Giant barriers inside the racing
    /// corridor are level-script pieces (Hong Kong's dam and the like): not walls there.
    pub fn enable_parry(&mut self, track: Option<&Track>) {
        if let Some(track) = track.filter(|t| !t.open) {
            for i in 0..self.tris.len() {
                if !self.barrier[i] || !self.steep_tri[i] {
                    continue;
                }
                let c = (self.tris[i][0] + self.tris[i][1] + self.tris[i][2]) / 3.0;
                let at = track.locate_anywhere(c);
                if track.contains(at.seg, c.xz()) && (phy::BARRIER_EDGE..=1.0 - phy::BARRIER_EDGE).contains(&at.u) {
                    self.steep_tri[i] = false;
                }
            }
        }
        let (steep, floor, barrier) = (&self.steep_tri, &self.floor_tri, &self.barrier);
        self.parry = Some(ParrySets {
            walls: crate::collide::Set::new(&self.tris, |i| steep[i]),
            floors: crate::collide::Set::new(&self.tris, |i| floor[i]),
            sight: crate::collide::Set::new(&self.tris, |i| !barrier[i]),
        });
        info!("collision: parry ({} walls)", self.parry.as_ref().map_or(0, |p| p.walls.len()));
    }

    /// A scripted gate across the course (a giant barrier whose face looks along `course`).
    fn gate(&self, id: u32, course: Option<Vec2>) -> bool {
        let (Some(f), true) = (course, self.barrier[id as usize]) else { return false };
        let t = &self.tris[id as usize];
        let n = (t[1] - t[0]).cross(t[2] - t[0]).xz().normalize_or_zero();
        n.dot(f).abs() > 0.866
    }

    /// The boat's collision cylinder at `p`: physics.hull_clearance up to the probe height.
    fn hull_shape(&self, p: Vec3) -> (Vec3, f32) {
        let (lo, hi) = (phy::HULL_CLEARANCE, self.probe.max(phy::HULL_CLEARANCE + 1.0));
        (p + Vec3::Y * (lo + hi) * 0.5, (hi - lo) * 0.5)
    }

    /// Parry: push the hull at `p` out of the walls (push, normal, triangle id).
    fn hull(&self, p: Vec3, r: f32, course: Option<Vec2>) -> Option<(Vec2, Vec2, u32)> {
        let (c, h) = self.hull_shape(p);
        self.parry.as_ref()?.walls.push_out(c, h, r, |id| self.gate(id, course))
    }

    /// Parry: cast the hull at `p` along `step` (XZ): (fraction before a wall, its normal).
    fn sweep_hull(&self, p: Vec3, step: Vec2, r: f32, course: Option<Vec2>) -> Option<(f32, Vec2)> {
        let (c, h) = self.hull_shape(p);
        self.parry.as_ref()?.walls.sweep(c, Vec3::new(step.x, 0.0, step.y), h, r, |id| self.gate(id, course)).map(|(t, n, _)| (t, n))
    }
}

/// Collision per course (`tracks.collision`): classic, or parry (crate::collide). The test
/// switch RIPTIDE_COLLISION=classic|parry forces one everywhere.
fn choose_collision(
    mut commands: Commands,
    collider: Option<ResMut<Collider>>,
    track: Option<Res<Track>>,
    sel: Res<Selection>,
    content: Res<crate::content::Content>,
) {
    commands.remove_resource::<crate::nav::NavLine>();
    let Some(mut col) = collider else { return };
    let row = content.tracks.get(sel.level).and_then(|c| TRACKS.iter().find(|t| t.id == c.id));
    let parry = match std::env::var("RIPTIDE_COLLISION").ok().as_deref() {
        Some("parry") => true,
        Some("classic") => false,
        _ => row.is_some_and(|t| t.collision == TracksCollision::Parry),
    };
    if parry {
        col.enable_parry(track.as_deref());
    } else {
        info!("collision: classic");
    }
    // The AI's way round what is in the water, where `tracks.ai_line` measured it better than
    // the bare racing line (RIPTIDE_NAV=1 / 0 forces it on / off).
    let nav = match std::env::var("RIPTIDE_NAV").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => row.is_some_and(|t| t.ai_line == TracksAiLine::Nav),
    };
    if let Some(t) = track.as_deref().filter(|_| nav) {
        if let Some(nav) = crate::nav::NavLine::build(t, &col) {
            commands.insert_resource(nav);
        }
    }
}

impl Collider {
    /// Does the move `a -> b` pass through a wall (a steep, non-barrier triangle)?
    fn crosses_wall(&self, a: Vec3, b: Vec3) -> bool {
        if let Some(p) = &self.parry {
            return p.walls.along(a, b).is_some();
        }
        let d = b - a;
        let key = |p: Vec3| ((p.x / self.cell).floor() as i32, (p.z / self.cell).floor() as i32);
        let mut cells = vec![key(a)];
        if key(b) != key(a) {
            cells.push(key(b));
        }
        cells.iter().any(|c| {
            self.walls.get(c).is_some_and(|ids| ids.iter().any(|&id| !self.barrier[id as usize] && segment_triangle(a, d, &self.tris[id as usize]).is_some()))
        })
    }
}

/// Collision check (RIPTIDE_PROBE=1): the player's run, logged as one `PROBE` line every
/// 20 frames: progress, wall contacts, seconds stalled while driving, launches higher than
/// physics.probe_launch, the highest air, and moves that crossed a wall.
#[derive(Default)]
struct ProbeState {
    prev: Option<Vec3>,
    stall: f32,
    /// Current stall streak (s) and whether it was reported.
    streak: f32,
    reported: bool,
    /// Furthest race distance, when it last grew, and whether that spot was reported.
    far: f32,
    far_t: f32,
    far_reported: bool,
    launches: u32,
    peak: f32,
    max_air: f32,
    crossings: u32,
    best: f32,
    frame: u32,
}

fn probe(time: Res<Time>, clock: Res<RaceClock>, track: Res<Track>, collider: Option<Res<Collider>>, boats: Query<&Boat>, mut st: Local<ProbeState>) {
    if std::env::var_os("RIPTIDE_PROBE").is_none() {
        return;
    }
    let Some(b) = boats.iter().find(|b| b.player) else { return };
    let dt = time.delta_secs().max(1e-3);
    if let Some(prev) = st.prev {
        let moved = (b.pos - prev).xz().length();
        if clock.t > 2.0 && b.finished.is_none() && !b.airborne && b.control.throttle > 0.5 && moved / dt < 30.0 {
            st.stall += dt;
            st.streak += dt;
            if st.streak > 2.0 && !st.reported {
                st.reported = true;
                let touch = collider.as_ref().and_then(|c| c.walls(b.pos, phy::BOAT_RADIUS * b.info.scale.min(1.3), b.pos.y + 10.0));
                info!(
                    "STALL at {:.0} {:.0} {:.0} seg {} s {:.2} u {:.2} water {:.0} floor {:.0} wall {:?} speed {:.0} heading {:.2} forward {:?}",
                    b.pos.x, b.pos.y, b.pos.z, b.tp.seg, b.tp.s, b.tp.u, b.tp.water, b.pos.y - b.tp.water,
                    touch.map(|(_, n)| n), b.speed, b.yaw, b.tp.forward
                );
            }
        } else if moved / dt > 60.0 {
            st.streak = 0.0;
            st.reported = false;
        }
        if let Some(col) = &collider {
            if moved > 1.0 && col.crosses_wall(prev + Vec3::Y * 12.0, b.pos + Vec3::Y * 12.0) {
                st.crossings += 1;
            }
        }
    }
    if b.airborne {
        st.peak = st.peak.max(b.pos.y - b.tp.water);
    } else if st.peak > 0.0 {
        if st.peak > phy::PROBE_LAUNCH {
            st.launches += 1;
        }
        st.max_air = st.max_air.max(st.peak);
        st.peak = 0.0;
    }
    st.best = st.best.max(track.race_distance(b.lap, b.tp.progress) / race_length(&track).max(1.0) * 100.0);
    let d = track.race_distance(b.lap, b.tp.progress);
    if d > st.far + 200.0 {
        st.far = d;
        st.far_t = clock.t;
        st.far_reported = false;
    } else if clock.t - st.far_t > 8.0 && !st.far_reported && b.finished.is_none() && clock.t > 2.0 {
        st.far_reported = true;
        let touch = collider.as_ref().and_then(|c| c.walls(b.pos, phy::BOAT_RADIUS * b.info.scale.min(1.3), b.pos.y + 10.0));
        info!(
            "STUCK at {:.0} {:.0} {:.0} seg {} s {:.2} u {:.2} water {:.0} above {:.0} wall {:?} speed {:.0} air {} forward {:?}",
            b.pos.x, b.pos.y, b.pos.z, b.tp.seg, b.tp.s, b.tp.u, b.tp.water, b.pos.y - b.tp.water,
            touch.map(|(_, n)| n), b.speed, b.airborne, b.tp.forward
        );
        // What the boat is doing about it: controls, the AI's plan, and what blocks it at the
        // two wall-test heights the physics uses and along the AI's obstacle ray.
        let heading = Vec2::new(-b.yaw.sin(), -b.yaw.cos());
        let (r, c) = (phy::BOAT_RADIUS * b.info.scale.min(1.3), b.control);
        let (walls, ray) = collider.as_ref().map_or((Vec::new(), None), |col| {
            let at = |h: f32| col.walls_id(b.pos, r, b.pos.y + h, Some(b.tp.forward)).map(|(_, n, id)| (h, n, id, col.tris[id as usize]));
            let eye = b.pos + Vec3::Y * col.probe.min(phy::WALL_PROBE_HEIGHT * 2.0);
            let reach = (b.speed.abs() * phy::AI_PROBE_TIME).max(phy::AI_PROBE_MIN);
            (col.cuts().into_iter().map(at).collect::<Vec<_>>(), col.hit(eye, eye + Vec3::new(heading.x, 0.0, heading.y) * reach).map(|f| f * reach))
        });
        info!(
            "STUCK detail heading {:?} throttle {:.2} steer {:.2} boost {} lane {:.2}->{:.2} reverse {:.1} stuck {:.1} contacts {} ray {:?} walls {:?}",
            heading, c.throttle, c.steer, c.boost, b.brain.lane, b.brain.lane_target, b.brain.reverse, b.brain.stuck, b.contacts, ray, walls
        );
    }
    st.prev = Some(b.pos);
    st.frame += 1;
    if st.frame % 20 == 0 {
        info!(
            "PROBE done {:.0}% finished {} contacts {} stall {:.1}s launches {} max_air {:.0} crossings {} at {:.0} {:.1} {:.0} water {:.1} ground {:?}",
            st.best.min(100.0),
            b.finished.is_some(),
            b.contacts,
            st.stall,
            st.launches,
            st.max_air,
            st.crossings,
            b.pos.x,
            b.pos.y,
            b.pos.z,
            b.tp.water,
            collider.as_ref().and_then(|c| c.floor(b.pos.x, b.pos.z, b.pos.y + 300.0))
        );
    }
}

/// A wide plane just under an H2Overdrive level, tiled with the texture that covers most of
/// its upward-facing terrain.
fn ground_apron(models: &mut Models, level: &H2Level) -> Option<(Handle<Mesh>, Handle<StandardMaterial>)> {
    let mut area: std::collections::HashMap<String, f32> = Default::default();
    let (mut lo, mut hi, mut low_y) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN), f32::MAX);
    for name in &level.sector_meshes {
        let Some(blob) = models.content.lux.get(&format!("mesh32.{name}")) else { continue };
        let Ok(model) = riptide_assets::h2mesh::decode_mesh(name, blob) else { continue };
        for part in &model.parts {
            for p in &part.positions {
                lo = lo.min(Vec2::new(p[0], p[2]));
                hi = hi.max(Vec2::new(p[0], p[2]));
                low_y = low_y.min(p[1]);
            }
            let Some(tex) = &part.texture else { continue };
            for t in part.indices.chunks_exact(3) {
                let v = |i: u32| Vec3::from(part.positions[i as usize]);
                let n = (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0]));
                if n.length() > 0.0 && n.y.abs() > 0.7 * n.length() {
                    *area.entry(tex.clone()).or_default() += n.length() * 0.5;
                }
            }
        }
    }
    let tex = area.into_iter().max_by(|a, b| a.1.total_cmp(&b.1))?.0;
    let texture = models.lux_texture(&tex)?;
    let (centre, half, tile) = ((lo + hi) * 0.5, phy::GROUND_APRON_SIZE, phy::GROUND_APRON_TILE.max(1.0));
    let y = low_y - phy::GROUND_APRON_DROP;
    let corners = [[-half, -half], [half, -half], [half, half], [-half, half]].map(|[x, z]| [centre.x + x, y, centre.y + z]);
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, corners.iter().map(|c| [c[0] / tile, c[2] / tile]).collect::<Vec<_>>());
    mesh.insert_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]));
    // No fog: fogged, the far apron turned back into the fog colour (sunset orange on Temple of Flume).
    let material = StandardMaterial { base_color_texture: Some(texture), perceptual_roughness: 1.0, cull_mode: None, fog_enabled: false, ..default() };
    Some((models.meshes.add(mesh), models.materials.add(material)))
}

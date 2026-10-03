//! The race: level spawn, boat physics, AI, pickups, chase camera and HUD.

use crate::cheats::{Cheats, Tuning};
use crate::content::{attach, BoatInfo, CourseSource, Models};
use crate::controls::Input;
use crate::sheets::{controls_ids as ctl, physics as phy, CheatsEffect, CHECKPOINTS, HackworldKind, HACKWORLD, H2_GLOBALS, H2_LEVELS, H2_TRIPWIRES, PICKUPS, TRACKS};
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
        app.add_systems(OnEnter(Screen::Race), (spawn_race, start_race_audio).chain())
            .add_systems(
                Update,
                (
                    player_input,
                    ai_drive,
                    boat_physics,
                    boat_contacts,
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
                )
                    .chain()
                    .run_if(in_state(Screen::Race)),
            );
        app.add_systems(Update, (race_audio, engine_audio).run_if(in_state(Screen::Race)));
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
    pub wall_hits: u32,
    /// Jumps since leaving the water, and seconds since the last one.
    pub jumps: u32,
    pub jump_t: f32,
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
}

#[derive(Component)]
struct Pickup {
    /// Row in the `pickups` sheet.
    row: usize,
    cooldown: f32,
    base: Vec3,
}

#[derive(Component)]
pub struct ChaseCam;

#[derive(Component)]
struct Sky;

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

fn move_props(time: Res<Time>, mut q: Query<(&mut PathMover, &mut Transform)>) {
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
    material: Handle<StandardMaterial>,
    mesh: Handle<Mesh>,
    /// Rest positions of the water vertices.
    base: Vec<[f32; 3]>,
    /// Animation frames (Hydro Thunder); empty = scroll the one texture instead.
    frames: Vec<Handle<Image>>,
}

fn water_flow(
    time: Res<Time>,
    water: Option<Res<WaterMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(w) = water else { return };
    // Waves: three travelling sines; normals from their slopes.
    if let Some(mesh) = meshes.get_mut(&w.mesh) {
        let t = time.elapsed_secs() * phy::WAVE_SPEED;
        let k = std::f32::consts::TAU / phy::WAVE_LENGTH.max(1.0);
        let a = phy::WAVE_AMPLITUDE;
        let mut pos = Vec::with_capacity(w.base.len());
        let mut nor = Vec::with_capacity(w.base.len());
        for p in &w.base {
            let (x, z) = (p[0] * k, p[2] * k);
            let (s1, s2, s3) = ((x + t).sin(), (z * 0.8 - t * 1.3).sin(), ((x + z) * 0.6 + t * 0.7).sin());
            let (c1, c2, c3) = ((x + t).cos(), (z * 0.8 - t * 1.3).cos(), ((x + z) * 0.6 + t * 0.7).cos());
            let y = a * (s1 + 0.6 * s2 + 0.3 * s3);
            let dx = a * k * (c1 + 0.3 * 0.6 * c3);
            let dz = a * k * (0.6 * 0.8 * c2 + 0.3 * 0.6 * c3);
            pos.push([p[0], p[1] + y, p[2]]);
            nor.push(Vec3::new(-dx, 1.0, -dz).normalize().to_array());
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, nor);
    }
    if let Some(m) = materials.get_mut(&w.material) {
        let t = time.elapsed_secs();
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

fn spawn_race(mut commands: Commands, mut models: Models, sel: Res<Selection>, mut fog_color: Local<Option<Color>>) {
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
                    for [a, b] in &t.river {
                        let drop = (a.water - b.water) * s > phy::WATERFALL_DROP;
                        let lift = |p: [f32; 3], y: f32| [p[0] * s, y * s, p[2] * s];
                        level.water.push(Quad { corners: [sc(a.start), sc(a.end), lift(b.end, if drop { a.water } else { b.water }), lift(b.start, if drop { a.water } else { b.water })] });
                        if drop {
                            level.waterfalls.push(Quad { corners: [lift(b.start, a.water), lift(b.end, a.water), sc(b.end), sc(b.start)] });
                        }
                    }
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
    } else if !track.edges.is_empty() {
        // No finish buoys (Hydro Thunder, some H2 levels): the course's last cross-section.
        let n = track.edges.len();
        let mid = |e: &riptide_assets::h2level::Edge| Vec2::new((e.start[0] + e.end[0]) * 0.5, (e.start[2] + e.end[2]) * 0.5);
        let (a, b) = (mid(&track.edges[n.saturating_sub(2)]), mid(&track.edges[n - 1]));
        track.finish = Some((b, (b - a).normalize_or(Vec2::NEG_Y), track.length()));
    }
    match (&choice.source, &ht_course) {
        // Laps come from the level's CLevelInfo (0 = point to point).
        (CourseSource::Sandbox { .. }, _) => {
            track.looped = true;
            track.laps = phy::HACKWORLD_LAPS as u32;
            track.open = true;
        }
        (CourseSource::H2(lvl), _) => {
            if let Some(row) = H2_LEVELS.iter().find(|l| l.id == *lvl) {
                track.looped = row.num_laps > 0;
                track.laps = row.num_laps.max(1) as u32;
            }
        }
        (_, Some((t, laps))) => {
            track.looped = t.looped;
            track.laps = (*laps).max(1);
            let s = phy::HT_WORLD_SCALE;
            track.starts = t.starts.iter().map(|(p, yaw)| (Vec3::from(*p) * s, *yaw)).collect();
            track.lanes = t.lanes.clone();
            track.branches = t.river.iter().filter(|[a, b]| !t.path.windows(2).any(|w| w[0].start == a.start && w[1].start == b.start))
                .map(|[a, b]| [*a, *b].map(|e| Edge { start: e.start.map(|v| v * s), end: e.end.map(|v| v * s), water: e.water * s })).collect();
        }
        _ => {}
    }
    info!(
        "{code}: {} sectors, {} props, {} boosters, track {:.0} units",
        level.sector_meshes.len(),
        level.props.len(),
        level.boosters.len(),
        track.length()
    );
    let scope = DespawnOnExit(Screen::Race);

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
            let model = riptide_assets::model::Model { name: format!("wc_{lvl}"), parts: vec![part], ..Default::default() };
            commands.insert_resource(Collider { trusted: true, ..Collider::build(std::slice::from_ref(&model), 1.0, phy::WALL_STEEPNESS, true) });
        }
    }
    for s in &level.sector_meshes {
        if let Some(p) = models.lux(s) {
            commands.entity(world).with_children(|c| {
                for piece in p.iter() {
                    c.spawn((Mesh3d(piece.mesh.clone()), MeshMaterial3d(piece.material.clone()), NotShadowCaster));
                }
            });
        }
    }
    // Props.
    for prop in &level.props {
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
        if let Some(m) = mover {
            commands.entity(e).insert(m);
        }
        attach(&mut commands, e, &p);
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
                Pickup { row, cooldown: 0.0, base },
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
        commands.insert_resource(Collider { probe: phy::HT_WALL_PROBE_HEIGHT, ..Collider::from_models(&solid, s, phy::HT_WALL_STEEPNESS) });
        let terrain = models.ht_model(t.terrain.clone());
        attach(&mut commands, world, &terrain);
        for inst in &t.instances {
            let at = Vec3::from(inst.position) * s;
            let pickup = PICKUPS.iter().position(|p| p.ht_geometry == inst.geometry && p.status.is_ok());
            let Some(p) = models.ht(&inst.geometry) else { continue };
            let mut e = commands.spawn((
                Transform::from_translation(at).with_rotation(Quat::from_rotation_y(inst.yaw)).with_scale(Vec3::splat(s * inst.scale)),
                Visibility::default(),
                scope.clone(),
            ));
            if let Some(row) = pickup {
                e.insert(Pickup { row, cooldown: 0.0, base: at });
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
    // Sky.
    if let Some(sky) = level.skyboxes.first() {
        if let Some(p) = models.lux_unlit(&sky.mesh) {
            let e = commands
                .spawn((
                    Transform::from_rotation(Quat::from_array(sky.rotation)).with_scale(Vec3::splat(sky.scale.max(1.0))),
                    Visibility::default(),
                    Sky,
                    scope.clone(),
                ))
                .id();
            commands.entity(e).with_children(|c| {
                for piece in p.iter() {
                    c.spawn((Mesh3d(piece.mesh.clone()), MeshMaterial3d(piece.material.clone()), NoFrustumCulling, NotShadowCaster));
                }
            });
            // The dome is a half sphere: from high up, past the terrain's edge, nothing is drawn
            // below its rim. Close it with a skirt in its own horizon colour.
            if let Some((mesh, color)) = sky_skirt(&models, &sky.mesh) {
                // Gaps in the dome (cut-out art) show the clear colour: make it the horizon too.
                commands.insert_resource(ClearColor(color));
                let material = models.materials.add(StandardMaterial { base_color: color, unlit: true, fog_enabled: false, cull_mode: None, ..default() });
                let mesh = models.meshes.add(mesh);
                commands.entity(e).with_children(|c| {
                    c.spawn((Mesh3d(mesh), MeshMaterial3d(material), NoFrustumCulling, NotShadowCaster));
                });
            }
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
    let player_slot = racers - 2;
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
    let mut slot_of = (0..racers).filter(|&s| s != player_slot);
    for (i, &bi) in picks.iter().enumerate() {
        let info = roster[bi].clone();
        let player = i == 0;
        let slot = if player { player_slot } else { slot_of.next().unwrap_or(i) };
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
                    brain: Brain { lane, lane_target: lane, skill: 0.90 + 0.08 * rand(), lane_timer: rand() * 3.0, stuck: 0.0, reverse: 0.0 },
                    roll: 0.0,
                    pitch: 0.0,
                    airborne: false,
                    wipeout: 0.0,
                    crush: Crush::Off,
                    surface: pos.y,
                    catchup: 0.0,
                    smashes: 0,
                    wall_hits: 0,
                    jumps: 0,
                    jump_t: 0.0,
                },
                scope.clone(),
                Name::new(info.name.clone()),
            ))
            .id();
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
    if let Some(target) = sel.render_target.clone() {
        cam.insert(target);
    }

    spawn_hud(&mut commands, &level);
    commands.insert_resource(RaceClock { t: -phy::COUNTDOWN, finish_order: Vec::new() });
    // Arcade timer: the level's `Starting Seconds`, topped up by its checkpoints (sheets).
    let timer_ok = TRACKS.iter().find(|t| t.id == choice.id).is_some_and(|t| t.timer.is_ok());
    let mut timer = ArcadeTimer { enabled: false, left: 0.0, gates: Vec::new(), banner: None };
    if let (true, CourseSource::H2(lvl)) = (timer_ok, &choice.source) {
        if let Some(row) = H2_LEVELS.iter().find(|l| l.id == *lvl) {
            timer.enabled = row.starting_seconds > 0;
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
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // Each quad is split into a grid so the surface can carry waves.
    let sub = (phy::WATER_SUBDIV as u32).max(1);
    let mut quad = |c: [[f32; 3]; 4]| {
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
    for q in &level.water {
        quad(q.corners);
    }
    // The racing line's own ribbon fills any gap between water sectors.
    let ribbon = if track.open || !add_ribbon { 0 } else { track.edges.len() - 1 };
    for i in 0..ribbon {
        let (a, b) = (&track.edges[i], &track.edges[i + 1]);
        let lift = |p: [f32; 3], h: f32| [p[0], h - 1.5, p[2]];
        quad([lift(a.start, a.water), lift(a.end, a.water), lift(b.end, b.water), lift(b.start, b.water)]);
    }
    let n = positions.len();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let base = positions.clone();
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
        normal_map_texture: normal_map,
        perceptual_roughness: if own_color { 0.35 } else { 0.22 },
        reflectance: if own_color { 0.2 } else { 0.35 },
        double_sided: true,
        cull_mode: None,
        ..default()
    };
    let material = models.materials.add(material);
    commands.insert_resource(WaterMaterial { material: material.clone(), frames, mesh: mesh.clone(), base });
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
        MeshMaterial3d(material),
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
    autopilot: Option<Res<Autopilot>>,
    mut boats: Query<&mut Boat>,
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
        let aim = aim_point(&track, tp, look, brain.lane);
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
        let corner = 1.0 - (steer.abs() * 0.25);
        let skill = if b.player { 1.0 } else { brain.skill };
        b.control = Control {
            throttle: (skill * corner).clamp(0.3, 1.1),
            steer,
            boost: b.fuel > 0.35 * g.boost_fuel_max_regular && steer.abs() < 0.3 && dot > 0.95,
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
    mut boats: Query<&mut Boat>,
) {
    let g = &tuning.0;
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }
    let racing = clock.t >= 0.0;
    for mut b in &mut boats {
        let def = b.info.def;
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
            let lift = |h: f32| (2.0 * phy::GRAVITY * h).sqrt();
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
                b.speed += c.throttle * thrust * phy::ACCEL_PER_THRUST * (phy::START_ROOM_BIAS + room) * dt;
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
        // Move in sub-steps of at most half a hull radius so fast boats cannot skip through a wall.
        let step = b.vel * dt;
        let r = phy::BOAT_RADIUS * b.info.scale.min(1.3);
        let subs = ((step.length() / (r * 0.5)).ceil() as usize).clamp(1, 8);
        let mut penalised = false;
        for _ in 0..subs {
            b.pos.x += step.x / subs as f32;
            b.pos.z += step.y / subs as f32;
            // Course walls (rocks, hulls, pillars, cliffs): push out and lose the into-wall velocity;
            // a real impact costs `Hit Wall Speed Penalty Mult` like the banks.
            let Some(col) = &collider else { continue };
            // Cut at hull height and lower down, so low walls and slopes catch too.
            let fwd = b.tp.forward;
            let hit = [col.probe, col.probe * 0.35]
                .iter()
                .find_map(|&h| col.walls_id(b.pos, r, b.pos.y + h, Some(fwd)).map(|(p, n, _)| (p, n)))
                .filter(|(push, _)| {
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
                if b.player && std::env::var_os("RIPTIDE_DEBUG").is_some() {
                    let before = b.pos - Vec3::new(push.x, 0.0, push.y);
                    let h = [col.probe, col.probe * 0.35].iter().find_map(|&h| col.walls_id(before, r, before.y + h, Some(fwd)));
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
        // Banks: a real impact costs `Hit Wall Speed Penalty Mult`, a glancing scrape just drags.
        if !track.open && (tp.u < phy::WALL_MARGIN || tp.u > 1.0 - phy::WALL_MARGIN) {
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
        // The surface a hull rides: the water, or a terrain floor (ramp, mound) within step-up reach.
        let reach = b.pos.y + phy::FLOOR_STEP_UP;
        let inside = (phy::FLOOR_CORRIDOR_MARGIN..=1.0 - phy::FLOOR_CORRIDOR_MARGIN).contains(&tp.u);
        let floor = collider
            .as_ref()
            .filter(|_| inside || track.open)
            .and_then(|c| c.floor(b.pos.x, b.pos.z, reach.min(tp.water + phy::FLOOR_MAX_ABOVE_WATER)));
        let water = floor.map_or(tp.water, |f| f.max(tp.water));
        b.vy -= phy::GRAVITY * dt;
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
            if !b.airborne && b.vy > 0.0 && b.surface > tp.water + 1.0 {
                // Off a ramp lip (still climbing, leaving a raised floor): TritonGame `Player / AI
                // Vel Y Min..Max`, by speed (evidence EVD_RAMP_LAUNCH).
                let (lo, hi) = if b.player { (g.player_vel_y_min, g.player_vel_y_max) } else { (g.ai_vel_y_min, g.ai_vel_y_max) };
                let unit = (b.speed / top.max(1.0)).clamp(0.0, 1.0);
                b.vy = b.vy.max(lo + (hi - lo) * unit);
            }
            b.airborne = true;
        }
        // Test hook: RIPTIDE_TEST_LAUNCH=secs throws the player high once (checking the view from
        // the air).
        if b.player && !b.airborne {
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
            if climb > 0.0 && climb <= run * phy::RAMP_MAX_SLOPE {
                // Riding up a ramp carries the climb rate into the air at its lip, never faster
                // than the game's own launch ceiling (TritonGame Player / AI Vel Y Max).
                let cap = if b.player { g.player_vel_y_max } else { g.ai_vel_y_max };
                b.vy = b.vy.max((climb / dt).min(cap));
            }
        }
        b.surface = water;
        b.tp = tp;
        // Point to point: the finish buoys' plane when the level has them (crossed, not reached
        // by centre-line distance), else the end of the racing line.
        let done = if track.looped {
            b.lap >= track.laps
        } else if let Some((p, n, at)) = track.finish {
            (tp.progress >= at - phy::GATE_WINDOW && (b.pos.xz() - p).dot(n) >= 0.0) || (tp.seg >= track.last_seg() && tp.s >= 0.98)
        } else {
            tp.seg >= track.last_seg() && tp.s >= 0.98
        };
        if racing && b.finished.is_none() && done {
            b.finished = Some(clock.t);
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
    mut boats: Query<&mut Boat>,
) {
    let g = &tuning.0;
    let dt = time.delta_secs();
    let t = time.elapsed_secs();
    for (mut pk, mut tf, mut vis) in &mut items {
        pk.cooldown = (pk.cooldown - dt).max(0.0);
        *vis = if pk.cooldown > 0.0 { Visibility::Hidden } else { Visibility::Inherited };
        tf.rotation = Quat::from_rotation_y(t * 2.0);
        tf.translation = pk.base + Vec3::Y * (4.0 * (t * 3.0).sin());
        if pk.cooldown > 0.0 {
            continue;
        }
        let row = &PICKUPS[pk.row];
        let fuel = row.fuel_global.and_then(|i| g.get(H2_GLOBALS[i].id)).unwrap_or(0.0);
        for mut b in &mut boats {
            if b.pos.distance(pk.base) < phy::PICKUP_RADIUS {
                if row.fills_super {
                    b.super_time = (b.super_time + fuel).min(g.boost_fuel_max_super);
                } else {
                    b.fuel = (b.fuel + fuel).min(g.boost_fuel_max_regular);
                }
                pk.cooldown = phy::PICKUP_RESPAWN;
                break;
            }
        }
    }
}

/// How far a boat's hull reaches below its model origin (world units).
#[derive(Component)]
pub struct Hull(pub f32);

pub(crate) fn place_boats(time: Res<Time>, tuning: Res<Tuning>, mut boats: Query<(&mut Boat, &mut Transform, Option<&Hull>)>) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    let t = time.elapsed_secs();
    for (mut b, mut tf, hull) in &mut boats {
        let norm = (b.speed / phy::SPEED_NORM).clamp(0.0, 1.0);
        let target_roll = -b.control.steer * norm * 0.22;
        let target_pitch = if b.airborne {
            (b.vy / 900.0).clamp(-0.35, 0.3)
        } else {
            0.04 + 0.05 * (b.speed / phy::SPEED_NORM).clamp(0.0, 1.3)
        };
        b.roll += (target_roll - b.roll) * (dt * 5.0).min(1.0);
        b.pitch += (target_pitch - b.pitch) * (dt * 4.0).min(1.0);
        let bob = if b.airborne { 0.0 } else {
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
            let draft = d.buoyancy_depth_max + (d.buoyancy_depth_min - d.buoyancy_depth_max) * plane;
            (h.0 - draft).max(0.0)
        });
        tf.translation = b.pos + Vec3::Y * (bob + lift);
        tf.rotation = Quat::from_euler(EulerRot::YXZ, b.yaw + spin, b.pitch, b.roll);
        tf.scale = Vec3::splat(1.0 + (phy::HULLCRUSH_SCALE - 1.0) * b.crush.grown(&tuning));
    }
}

fn chase_camera(
    time: Res<Time>,
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
    let zoom = cheats.zoom();
    let heading = Vec3::new(-b.yaw.sin(), 0.0, -b.yaw.cos());
    let speed_pull = (b.speed / phy::SPEED_NORM).clamp(0.0, 1.6);
    let want = b.pos - heading * (phy::CAM_BACK + phy::CAM_BACK_SPEED * speed_pull) * size.sqrt() * zoom
        + Vec3::Y * (phy::CAM_UP + phy::CAM_UP_SPEED * speed_pull) * zoom;
    let k = (dt * phy::CAM_FOLLOW_RATE).min(1.0);
    tf.translation = tf.translation.lerp(want, k);
    // Stay inside the river corridor so cliffs never swallow the view (open water has none).
    let at = track.locate(tf.translation, b.tp.seg);
    if !track.open && (at.u < 0.02 || at.u > 0.98) {
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
                if clock.t < 0.0 {
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
                    format!("FINISHED {place}{suffix}\n{}\nR: race again   Esc: menu", fmt(t))
                } else if p.timed_out {
                    "TIME UP!\nR: race again   Esc: menu".into()
                } else if let Some((msg, _)) = timer.as_ref().and_then(|t| t.banner.clone()) {
                    msg
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

fn race_keys(input: Input, cheats: Res<Cheats>, mut next: ResMut<NextState<Screen>>, sel: Res<Selection>) {
    if sel.render_target.is_some() || cheats.menu_open {
        return;
    }
    if input.just_pressed(ctl::LEAVE_RACE) {
        next.set(Screen::Menu);
    }
    if input.just_pressed(ctl::RESTART) {
        next.set(Screen::Restart);
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
    autopilot: Option<Res<Autopilot>>,
    mut exit: MessageWriter<AppExit>,
) {
    clock.t += time.delta_secs().min(1.0 / 20.0);
    let mut done: Vec<(f32, Entity)> =
        boats.iter().filter_map(|(e, b)| b.finished.map(|t| (t, e))).filter(|(_, e)| !clock.finish_order.contains(e)).collect();
    done.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, e) in done {
        clock.finish_order.push(e);
        if let Ok((_, b)) = boats.get(e) {
            if b.player {
                // One line for test scripts; capture runs (autopilot) stop here when asked to.
                info!("RESULT finished {} of {} in {:.2}s", clock.finish_order.len(), boats.iter().count(), b.finished.unwrap_or(0.0));
                if autopilot.is_some() && std::env::var_os("RIPTIDE_EXIT_ON_FINISH").is_some() {
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
    tris: Vec<[Vec3; 3]>,
    /// Height above the waterline the walls are cut at.
    probe: f32,
    /// The game's own collision mesh (H2Overdrive): never second-guessed.
    trusted: bool,
    /// Per triangle: a giant scripted barrier (see physics.barrier_height).
    barrier: Vec<bool>,
    /// Steep triangles only (walls), same grid layout.
    walls: std::collections::HashMap<(i32, i32), Vec<u32>>,
    /// Upward-facing triangles (floors: ramps, mounds, banks), same grid layout.
    floors: std::collections::HashMap<(i32, i32), Vec<u32>>,
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
        for part in models.iter().flat_map(|m| &m.parts) {
            for t in part.indices.chunks_exact(3) {
                let v = [0, 1, 2].map(|k| Vec3::from(part.positions[t[k] as usize]) * scale);
                let id = tris.len() as u32;
                let (lo, hi) = (v[0].min(v[1]).min(v[2]), v[0].max(v[1]).max(v[2]));
                let n = (v[1] - v[0]).cross(v[2] - v[0]).normalize_or_zero();
                // Walls: near-vertical faces only. The H2 mesh flips some triangles' winding, so the
                // normal's sign means nothing (a flat riverbed patch can face "down").
                let steep = n != Vec3::ZERO && n.y.abs() < steepness;
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
        Self { cell: Self::CELL, grid, tris, walls, floors, probe: phy::WALL_PROBE_HEIGHT, trusted: false, barrier }
    }

    /// First hit along `a -> b` as a fraction of the segment.
    fn hit(&self, a: Vec3, b: Vec3) -> Option<f32> {
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

/// The player's engine loops (from its boatdef's `Engine Def`) and the countdown voice.
fn start_race_audio(mut sfx: crate::sound::Sfx, boats: Query<(Entity, &Boat)>, sel: Res<Selection>, content: Res<crate::content::Content>) {
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
    sfx.event(crate::sheets::sound_events_ids::COUNTDOWN);
}

fn engine_audio(boats: Query<&Boat>, mut layers: Query<(&crate::sound::EngineLayer, &crate::sound::EngineOf, &mut bevy::audio::AudioSink)>) {
    crate::sound::drive_engines(|e| boats.get(e).ok().map(|b| b.speed.abs() / b.info.def.max_speed_l1.max(1.0)), &mut layers);
}

/// What the player's boat did last frame, to turn state changes into sound events.
#[derive(Default)]
struct HeardState {
    boosting: bool,
    super_on: bool,
    smashes: u32,
    wall_hits: u32,
    fuel: f32,
    super_time: f32,
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
}

fn race_audio(
    mut sfx: crate::sound::Sfx,
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
    if !last.started {
        *last = HeardState { fuel: p.fuel, started: true, ..default() };
    }
    if p.fuel > last.fuel + 0.5 {
        sfx.event(ev::BOOST_PICKUP);
    }
    if p.super_time > last.super_time + 0.5 {
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
    if p.wall_hits > last.wall_hits {
        sfx.event(ev::WALL_HIT);
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
    last.super_time = p.super_time;
    last.wipeout = p.wipeout > 0.0;
    last.crushing = crushing;
    last.lap = p.lap;
    last.airborne = p.airborne;
    last.finished = p.finished.is_some();
    last.timed_out = p.timed_out;
}

impl Collider {
    /// Push a circle of radius `r` at `p` out of the walls cut at height `y`: the summed push
    /// (XZ) and the normal of the deepest contact, or `None` when clear.
    fn walls(&self, p: Vec3, r: f32, y: f32) -> Option<(Vec2, Vec2)> {
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
    fn floor(&self, x: f32, z: f32, max_y: f32) -> Option<f32> {
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
                .spawn((Transform::from_translation(at), Visibility::default(), Pickup { row, cooldown: 0.0, base: at }, scope.clone()))
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

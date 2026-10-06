//! Water effects from H2Overdrive's own art: foam wakes (`pt_com_waterwhite_flip01..60`),
//! rooster-tail spray (`ET_COM_WaterSpray01..04`) and landing splashes. Rates and sizes come from
//! the `physics` sheet; the rooster tail sits at each boat def's `Rooster Offset`.

use crate::content::Models;
use crate::cheats::Tuning;
use crate::race::{Boat, ChaseCam};
use crate::sheets::{h2_bolts_ids, H2RocketLayersRow, H2_BOLTS, H2_MOTIFS, H2_ROCKET_FLAMES, H2_ROCKET_LAYERS};
use crate::sheets::physics as phy;
use crate::Screen;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use std::collections::HashMap;

pub struct EffectsPlugin;

impl Plugin for EffectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlameArt>()
            .add_systems(OnEnter(Screen::Race), (load_art, load_bolt_art))
            .add_systems(Update, (emit, animate, hull_bolts).chain().run_if(in_state(Screen::Race)))
            .add_systems(Update, (emit_flames, emit_level_fires, animate_flames).chain()
                .after(emit)
                .after(crate::race::place_boats)
                .after(crate::boatrig::animate)
                .run_if(in_state(Screen::Race)));
    }
}

/// Alpha steps per texture: particles fade by switching material, not by allocating one each.
const FADE_STEPS: usize = 6;

#[derive(Resource)]
struct FxArt {
    quad: Handle<Mesh>,
    /// `[frame][fade]` foam materials.
    foam: Vec<Vec<Handle<StandardMaterial>>>,
    /// `[texture][fade]` spray materials.
    spray: Vec<Vec<Handle<StandardMaterial>>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Lies flat on the water.
    Foam,
    /// Faces the camera, falls under gravity.
    Spray,
}

#[derive(Component)]
struct Particle {
    kind: Kind,
    vel: Vec3,
    age: f32,
    life: f32,
    size: (f32, f32),
    art: usize,
}

/// Per-boat emission state.
/// A hull spray emitter (boat local, unscaled): a point on a mesh `spray` surface's waterline edge
/// (CWaterspraySys, FUN_004f0a50) with its boat def `Waterspray <i>` entry.
#[derive(Clone, Copy)]
struct SprayPoint {
    pos: Vec3,
    out: Vec3,
    slope: f32,
    intensity: f32,
    due: f32,
}

#[derive(Component, Default)]
struct Emitter {
    foam_due: f32,
    spray_due: f32,
    /// Hull spray points, decoded from the boat mesh on first use (None = not yet).
    hull: Option<Vec<SprayPoint>>,
    flame_due: HashMap<usize, f32>,
    was_airborne: bool,
}

fn load_art(mut commands: Commands, mut models: Models) {
    let quad = models.meshes.add(Rectangle::new(1.0, 1.0));
    let fades = |models: &mut Models, tex: Handle<Image>, blend: AlphaMode, opacity: f32| -> Vec<Handle<StandardMaterial>> {
        (0..FADE_STEPS)
            .map(|k| {
                let a = 1.0 - k as f32 / FADE_STEPS as f32;
                models.materials.add(StandardMaterial {
                    base_color: Color::srgba(0.92, 0.96, 1.0, a * opacity),
                    base_color_texture: Some(tex.clone()),
                    alpha_mode: blend,
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                })
            })
            .collect()
    };
    // The whitewater flipbook is a tiling surface texture: give each frame a soft round patch
    // shape (alpha = brightness x radial falloff). Every 4th frame keeps it smooth with fewer
    // materials.
    let lux = models.content.lux.clone();
    let foam_tex: Vec<Handle<Image>> = (1..=60)
        .step_by(4)
        .filter_map(|n| lux.get(&format!("txtr1.pt_com_waterwhite_flip{n:02}")))
        .filter_map(|b| riptide_assets::image::decode_txtr(b).ok())
        .map(|mut img| {
            let (w, h) = (img.width as f32, img.height as f32);
            for y in 0..img.height {
                for x in 0..img.width {
                    let (dx, dy) = ((x as f32 + 0.5) / w * 2.0 - 1.0, (y as f32 + 0.5) / h * 2.0 - 1.0);
                    let r = (dx * dx + dy * dy).sqrt();
                    let edge = (1.0 - r).clamp(0.0, 1.0);
                    let px = &mut img.rgba[((y * img.width + x) * 4) as usize..][..4];
                    let lum = (px[0] as f32 + px[1] as f32 + px[2] as f32) / (3.0 * 255.0);
                    px[3] = (255.0 * edge * edge * (3.0 - 2.0 * edge) * (0.3 + lum)).min(255.0) as u8;
                    px[0..3].fill(255);
                }
            }
            models.images.add(crate::content::to_bevy_image(img))
        })
        .collect();
    let foam: Vec<Vec<Handle<StandardMaterial>>> = foam_tex.into_iter().map(|t| fades(&mut models, t, AlphaMode::Blend, 0.55)).collect();
    let mut spray_tex: Vec<Handle<Image>> = (1..=4).filter_map(|n| models.lux_texture(&format!("ET_COM_WaterSpray{n:02}"))).collect();
    spray_tex.extend(models.lux_texture("ET_COM_splash"));
    let spray: Vec<Vec<Handle<StandardMaterial>>> = spray_tex
        .into_iter()
        .map(|t| fades(&mut models, t, AlphaMode::Blend, phy::SPRAY_OPACITY))
        .collect();
    commands.insert_resource(FxArt { quad, foam, spray });
}

fn emit(
    mut commands: Commands,
    time: Res<Time>,
    art: Option<Res<FxArt>>,
    live: Query<(), With<Particle>>,
    h2water: Option<Res<crate::h2water::H2WaterOn>>,
    content: Res<crate::content::Content>,
    mut boats: Query<(Entity, &Boat, Option<&mut Emitter>)>,
) {
    let Some(art) = art else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let mut budget = (phy::FX_MAX_PARTICLES as usize).saturating_sub(live.iter().count());
    let mut seed = (time.elapsed_secs() * 1000.0) as u32 | 1;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 10_000) as f32 / 10_000.0
    };
    let scope = DespawnOnExit(Screen::Race);
    for (e, b, em) in &mut boats {
        let Some(mut em) = em else {
            commands.entity(e).insert(Emitter::default());
            continue;
        };
        let rot = Quat::from_rotation_y(b.yaw);
        let back = rot * Vec3::Z;
        let side = rot * Vec3::X;
        let s = b.info.scale;
        let speed = b.speed.max(0.0);
        let unit = (speed / b.info.def.max_speed_l1.max(1.0)).clamp(0.0, 1.3);
        let mut spawn = |kind: Kind, pos: Vec3, vel: Vec3, life: f32, size: (f32, f32), art_n: usize, rnd: f32| {
            if budget == 0 || art_n == 0 {
                return;
            }
            budget -= 1;
            let id = commands
                .spawn((
                    Mesh3d(art.quad.clone()),
                    MeshMaterial3d(match kind {
                        Kind::Foam => art.foam[0][0].clone(),
                        Kind::Spray => art.spray[0][0].clone(),
                    }),
                    Transform::from_translation(pos).with_scale(Vec3::splat(size.0)),
                    Particle { kind, vel, age: 0.0, life, size, art: (rnd * art_n as f32) as usize % art_n },
                    NotShadowCaster,
                    scope.clone(),
                ))
                .id();
            // Under H2Overdrive's water shader foam goes into its whitewash buffer instead
            // (crate::h2water::WhitewashCam), lit and tinted by the water.
            if h2water.is_some() && matches!(kind, Kind::Foam) {
                commands.entity(id).insert(bevy::camera::visibility::RenderLayers::layer(crate::h2water::WHITEWASH_LAYER));
            }
        };
        let on_water = !b.airborne;
        // Foam wake: patches left at the stern and both flanks, more the faster the boat goes.
        if on_water && speed > phy::WAKE_MIN_SPEED {
            em.foam_due += phy::WAKE_RATE * unit * dt;
            while em.foam_due >= 1.0 {
                em.foam_due -= 1.0;
                let flank = (rand() - 0.5) * 2.0;
                let p = b.pos + back * (30.0 * s) + side * flank * 18.0 * s + Vec3::Y * 1.5;
                let drift = side * flank * 40.0 - back * 0.0;
                spawn(Kind::Foam, p, drift, phy::WAKE_LIFE, (phy::WAKE_SIZE_START * s, phy::WAKE_SIZE_END * s), art.foam.len(), rand());
            }
        }
        // Hull spray: thrown off the hull's spray surfaces (stern plate, waterline strips) while
        // planing, each point along its outward normal at its Waterspray angle, harder with speed.
        if em.hull.is_none() {
            em.hull = Some(hull_spray_points(&content.lux, b.info.def));
            if std::env::var_os("RIPTIDE_DEBUG").is_some() {
                info!("hull spray: {} points for {}", em.hull.as_ref().map_or(0, |h| h.len()), b.info.def.mesh_name_local);
            }
        }
        if on_water && speed > phy::WAKE_MIN_SPEED {
            let carry = Vec3::new(b.vel.x, 0.0, b.vel.y) * phy::HULL_SPRAY_CARRY;
            let n_art = art.spray.len();
            for p in em.hull.as_mut().into_iter().flatten() {
                p.due += phy::HULL_SPRAY_RATE * p.intensity * unit * dt;
                while p.due >= 1.0 {
                    p.due -= 1.0;
                    let kick = speed * phy::HULL_SPRAY_KICK * p.intensity * (0.8 + 0.2 * rand());
                    let out = rot * p.out;
                    let vel = carry + out * kick + Vec3::Y * (kick * p.slope + phy::HULL_SPRAY_UP * rand());
                    spawn(Kind::Spray, b.pos + rot * p.pos * s, vel, phy::HULL_SPRAY_LIFE * (0.7 + 0.6 * rand()), (phy::SPRAY_SIZE_START * s * 0.6, phy::SPRAY_SIZE_END * s * 0.6), n_art, rand());
                }
            }
        }
        // Rooster tail: spray thrown up and back from the boat def's rooster offset while boosting.
        if on_water && b.boosting && speed > phy::WAKE_MIN_SPEED {
            let def = b.info.def;
            // D3D +Z is forward; after the Z mirror the offset's Z points backwards here.
            let at = b.pos + rot * Vec3::new(def.rooster_offset_x, def.rooster_offset_y, -def.rooster_offset_z) * s;
            em.spray_due += phy::SPRAY_RATE * unit * dt;
            while em.spray_due >= 1.0 {
                em.spray_due -= 1.0;
                let vel = back * speed * 0.25 + Vec3::Y * phy::SPRAY_UP * (0.6 + 0.6 * rand()) + side * (rand() - 0.5) * 80.0;
                spawn(Kind::Spray, at, vel, phy::SPRAY_LIFE, (phy::SPRAY_SIZE_START * s, phy::SPRAY_SIZE_END * s), art.spray.len(), rand());
            }
        }
        // Landing splash.
        if em.was_airborne && on_water {
            for _ in 0..phy::SPLASH_COUNT as usize {
                let a = rand() * std::f32::consts::TAU;
                let out = Vec3::new(a.cos(), 0.0, a.sin()) * (60.0 + 140.0 * rand());
                spawn(Kind::Spray, b.pos, out + Vec3::Y * phy::SPRAY_UP * (0.4 + 0.6 * rand()), phy::SPRAY_LIFE, (phy::SPRAY_SIZE_START * s, phy::SPRAY_SIZE_END * s), art.spray.len(), rand());
            }
        }
        em.was_airborne = b.airborne;
    }
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    art: Option<Res<FxArt>>,
    cam: Query<&Transform, (With<ChaseCam>, Without<Particle>)>,
    mut parts: Query<(Entity, &mut Particle, &mut Transform, &mut MeshMaterial3d<StandardMaterial>)>,
) {
    let Some(art) = art else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let eye = cam.single().map(|c| c.translation).ok();
    for (e, mut p, mut tf, mut mat) in &mut parts {
        p.age += dt;
        if p.age >= p.life {
            commands.entity(e).despawn();
            continue;
        }
        let t = p.age / p.life;
        let fade = ((t * FADE_STEPS as f32) as usize).min(FADE_STEPS - 1);
        tf.scale = Vec3::splat(p.size.0 + (p.size.1 - p.size.0) * t);
        match p.kind {
            Kind::Foam => {
                tf.translation += p.vel * dt;
                p.vel *= 1.0 - (2.0 * dt).min(1.0);
                tf.rotation = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
                // Walk the flipbook over the particle's life.
                let frame = (p.art + (t * art.foam.len() as f32) as usize) % art.foam.len();
                mat.0 = art.foam[frame][fade].clone();
            }
            Kind::Spray => {
                p.vel.y -= phy::GRAVITY * dt;
                p.vel *= (1.0 - phy::SPRAY_DRAG * dt).max(0.0);
                tf.translation += p.vel * dt;
                // FX_Waterspray draws each droplet as a camera-facing streak from where it is to where
                // it was: stretch the quad along the velocity, its face toward the eye.
                if let Some(eye) = eye {
                    let dir = p.vel.normalize_or(Vec3::Y);
                    let to_eye = (eye - tf.translation).normalize_or(Vec3::Z);
                    let side = dir.cross(to_eye).normalize_or(Vec3::X);
                    let face = side.cross(dir);
                    tf.rotation = Quat::from_mat3(&Mat3::from_cols(side, dir, face));
                    let w = tf.scale.x;
                    tf.scale = Vec3::new(w, w + p.vel.length() * phy::SPRAY_STREAK, 1.0);
                }
                let near = eye.map_or(1.0, |e| (tf.translation.distance(e) / phy::FX_NEAR_FADE).clamp(0.0, 1.0));
                let fade = fade.max((((1.0 - near) * FADE_STEPS as f32) as usize).min(FADE_STEPS - 1));
                mat.0 = art.spray[p.art][fade].clone();
            }
        }
    }
}

// ---- Boost flames: `h2_rocket_flames` -> up to four `h2_rocket_layers` -> `h2_motifs` ----------

const FLAME_STEPS: usize = 8;

/// Per rocket layer: one material per colour step over the particle's life.
#[derive(Resource, Default)]
struct FlameArt(HashMap<usize, Vec<Handle<StandardMaterial>>>);

#[derive(Component)]
struct Flame {
    /// From a level fire (`LevelFire`), not a boat: counted against its own particle budget.
    level: bool,
    layer: usize,
    vel: Vec3,
    age: f32,
    life: f32,
    size: f32,
    /// FX_RocketFlame's spin: a random start angle, and the rotation speed blended from birth to
    /// death (radians per unit of life, signed at random), applied as 0.5 x life x speed.
    spin0: f32,
    spin_birth: f32,
    spin_death: f32,
}

/// 0 -> 1 across a transition centred on `centre`, `width` wide (life fractions).
fn transition(t: f32, centre: f32, width: f32) -> f32 {
    if width <= 1e-4 {
        return if t >= centre { 1.0 } else { 0.0 };
    }
    let x = ((t - (centre - width * 0.5)) / width).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn motif_rgb(m: Option<usize>) -> Vec3 {
    m.map_or(Vec3::ONE, |i| {
        let c = H2_MOTIFS[i].color;
        Vec3::new(c[0], c[1], c[2]) * H2_MOTIFS[i].scale
    })
}

/// Colour (motif x amp) and opacity of a layer at life fraction `t`; the `High` set is the boost.
fn layer_color(l: &H2RocketLayersRow, t: f32) -> (Vec3, f32) {
    let keys = [
        (motif_rgb(l.high_color0_motif) * l.high_color0_amp, l.high_color0_opacity),
        (motif_rgb(l.high_color1_motif) * l.high_color1_amp, l.high_color1_opacity),
        (motif_rgb(l.high_color2_motif) * l.high_color2_amp, l.high_color2_opacity),
    ];
    let f0 = transition(t, l.high_colortran0_life, l.high_colortran0_width);
    let f1 = transition(t, l.high_colortran1_life, l.high_colortran1_width);
    let c = keys[0].0.lerp(keys[1].0, f0).lerp(keys[2].0, f1);
    let a = keys[0].1 + (keys[1].1 - keys[0].1) * f0;
    let a = a + (keys[2].1 - a) * f1;
    let start = l.high_fade_life_start;
    let fade = if t > start { 1.0 - (t - start) / (1.0 - start).max(1e-3) } else { 1.0 };
    (c, (a * fade).clamp(0.0, 1.0))
}

/// Scale0 at birth -> Scale1 at `ScaleTrans0 Life` -> Scale2 at death. FX_RocketFlame eases the first
/// stretch by the square root of its progress (fast growth, then settling), the second linearly.
fn layer_scale(l: &H2RocketLayersRow, t: f32) -> f32 {
    let m = l.high_scaletrans0_life.clamp(1e-3, 0.999);
    if t < m {
        l.high_scale0_val + (l.high_scale1_val - l.high_scale0_val) * (t / m).sqrt()
    } else {
        l.high_scale1_val + (l.high_scale2_val - l.high_scale1_val) * (t - m) / (1.0 - m)
    }
}

fn flame_materials(models: &mut Models, layer: usize) -> Vec<Handle<StandardMaterial>> {
    let l = &H2_ROCKET_LAYERS[layer];
    let tex = models.lux_texture(l.texture);
    // Smoke layers blend, flame layers add (evidence EVD_ROCKET_LAYERS).
    let add = !l.texture.to_ascii_lowercase().contains("smoke");
    let brightness = if add { phy::ROCKET_ADDITIVE_BRIGHTNESS } else { 1.0 };
    (0..FLAME_STEPS)
        .map(|k| {
            let (c, a) = layer_color(l, (k as f32 + 0.5) / FLAME_STEPS as f32);
            models.materials.add(StandardMaterial {
                base_color: Color::LinearRgba(LinearRgba::new(c.x * brightness, c.y * brightness, c.z * brightness,
                    a * if add { 1.0 } else { phy::ROCKET_SMOKE_OPACITY })),
                base_color_texture: tex.clone(),
                alpha_mode: if add { AlphaMode::Add } else { AlphaMode::Blend },
                unlit: true,
                double_sided: true,
                cull_mode: None,
                ..default()
            })
        })
        .collect()
}

fn emit_flames(
    mut commands: Commands,
    time: Res<Time>,
    art: Option<Res<FxArt>>,
    mut flames: ResMut<FlameArt>,
    mut models: Models,
    live: Query<&Flame>,
    mut boats: Query<(&Boat, &mut Emitter, Option<&crate::boatrig::BoatRig>, Option<&crate::race::Stern>)>,
    places: bevy::transform::helper::TransformHelper,
) {
    let Some(art) = art else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let mut budget = (phy::ROCKET_MAX_PARTICLES as usize).saturating_sub(live.iter().filter(|p| !p.level && p.age + dt < p.life).count());
    let mut seed = (time.elapsed_secs() * 7919.0) as u32 | 1;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 10_000) as f32 / 10_000.0 * 2.0 - 1.0
    };
    let mut emitters: Vec<_> = boats.iter_mut().collect();
    emitters.sort_by_key(|(b, _, _, _)| !b.player);
    for (b, mut em, rig, stern) in emitters {
        let def = b.info.def;
        let flame = if b.super_time > 0.0 {
            def.flamedef_super.or(def.flamedef_boost)
        } else if b.boosting {
            def.flamedef_boost
        } else {
            None
        };
        let Some(flame) = flame else {
            em.flame_due.clear();
            continue;
        };
        let f = &H2_ROCKET_FLAMES[flame];
        let rot = Quat::from_rotation_y(b.yaw);
        let s = b.info.scale;
        let back = rot * Vec3::Z;
        // The boat's own rocket nozzle bones (RKBOOST_*, RKSUPERBOOST for gold boost); boats
        // without a rig use the stern estimate (physics.rocket_nozzle_height).
        let fallback = match stern {
            Some(st) => b.pos + rot * st.0,
            None => b.pos + rot * Vec3::new(0.0, phy::ROCKET_NOZZLE_HEIGHT, -def.rooster_offset_z) * s,
        };
        let nozzles: Vec<(Vec3, Vec3)> = match rig {
            Some(r) => {
                let ids: Vec<Entity> = match (b.super_time > 0.0, r.super_nozzle) {
                    (true, Some(n)) => vec![n],
                    _ => r.nozzles.clone(),
                };
                // GlobalTransform is from the previous frame during Update; compute
                // from this frame's local boat/bone poses before render preparation.
                ids.iter().filter_map(|e| places.compute_global_transform(*e).ok()).map(|g| {
                    // Original emitter velocity follows the nozzle's local +Z. The
                    // asset decoder mirrors Z, so that axis is -Z in output space.
                    (g.translation(), g.affine().transform_vector3(Vec3::NEG_Z).normalize_or(back))
                }).collect()
            }
            None => Vec::new(),
        };
        let nozzles = if nozzles.is_empty() { vec![(fallback, back)] } else { nozzles };
        let carry = Vec3::new(b.vel.x, b.vy, b.vel.y);
        for layer in [f.layer0_def, f.layer1_def, f.layer2_def, f.layer3_def].into_iter().flatten() {
            let mat = flames.0.entry(layer).or_insert_with(|| flame_materials(&mut models, layer))[0].clone();
            let l = &H2_ROCKET_LAYERS[layer];
            // FUN_004d6500 fills the gap between the moving nozzle and its newest
            // puff at Motion Puff Dist intervals; 60 Hz is far too sparse for
            // the retail 40 ms flame. Use the layer's exhaust travel / puff distance.
            let rate = (l.high_motion_speed.abs() / l.high_motion_puff_dist.max(0.01)).max(phy::ROCKET_RATE);
            let due = em.flame_due.entry(layer).or_default();
            let due_before = *due;
            *due += rate * dt;
            let n = *due as usize;
            *due -= n as f32;
            for k in 0..n * nozzles.len() {
                let (nozzle, direction) = nozzles[k % nozzles.len()];
                // Emit throughout the frame. A 40 ms flame otherwise dies in the same
                // 50 ms capture step in which it was born, before it can be rendered.
                let born_at = (k / nozzles.len()) as f32 + 1.0 - due_before;
                let born_at = born_at / rate;
                // At least ~2.5 frames: a slow frame (browser, hitches) must not blank the jet.
                let life = (l.high_motion_life_secs + l.high_motion_life_secs_spread * rand()).max(0.02).max(2.5 * dt);
                let age = (dt - born_at).max(0.0);
                if age >= life { continue; }
                if budget == 0 { return; }
                budget -= 1;
                let speed = l.high_motion_speed + l.high_motion_speed_spread * rand();
                let size = phy::ROCKET_SIZE * s;
                let jitter = rot * Vec3::new(rand() * l.high_motion_pos_delta_x, rand() * l.high_motion_pos_delta_y, 0.0) * s;
                commands.spawn((
                    Mesh3d(art.quad.clone()),
                    MeshMaterial3d(mat.clone()),
                    // Interpolate the nozzle's birth position within this frame.
                    Transform::from_translation(nozzle - carry * age + jitter).with_scale(Vec3::splat(l.high_scale0_val.max(0.01) * size)),
                    Flame {
                        level: false,
                        layer,
                        vel: carry + direction * speed,
                        age: -born_at,
                        life,
                        size,
                        spin0: rand() * std::f32::consts::PI,
                        spin_birth: (l.high_rot_speed_birth * (1.0 + l.high_rot_speed_birth_spread * rand()).max(0.0)) * rand().signum(),
                        spin_death: l.high_rot_speed_death * (1.0 + l.high_rot_speed_death_spread * rand()).max(0.0),
                    },
                    NotShadowCaster,
                    DespawnOnExit(Screen::Race),
                ));
            }
        }
    }
}

fn animate_flames(
    mut commands: Commands,
    time: Res<Time>,
    flames: Res<FlameArt>,
    cam: Query<&Transform, (With<ChaseCam>, Without<Flame>)>,
    mut parts: Query<(Entity, &mut Flame, &mut Transform, &mut MeshMaterial3d<StandardMaterial>)>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    if std::env::var_os("RIPTIDE_DEBUG").is_some() {
        let n = parts.iter().count();
        let big = parts.iter().map(|(_, _, tf, _)| tf.scale.x).fold(0.0f32, f32::max);
        let d = cam.single().map(|c| c.translation).ok().map_or(0.0, |e| parts.iter().map(|(_, _, tf, _)| tf.translation.distance(e)).fold(f32::MAX, f32::min));
        info!("flames: {n} live, biggest {big:.0}, nearest to camera {d:.0}");
    }
    let eye = cam.single().map(|c| c.translation).ok();
    for (e, mut p, mut tf, mut mat) in &mut parts {
        let active_dt = (p.age + dt).max(0.0).min(dt);
        p.age += dt;
        if p.age >= p.life {
            commands.entity(e).despawn();
            continue;
        }
        let l = &H2_ROCKET_LAYERS[p.layer];
        let t = p.age / p.life;
        p.vel *= 1.0 - (l.high_motion_friction * active_dt).min(1.0);
        p.vel.y -= l.high_motion_gravity * active_dt;
        tf.translation += p.vel * active_dt;
        tf.scale = Vec3::splat(layer_scale(l, t).max(0.01) * p.size);
        if let Some(eye) = eye {
            tf.look_at(eye, Vec3::Y);
        }
        let speed = p.spin_birth + (p.spin_death * p.spin_birth.signum() - p.spin_birth) * t;
        tf.rotate_local_z(p.spin0 + 0.5 * t * speed);
        let near = eye.map_or(1.0, |e| (tf.translation.distance(e) / phy::ROCKET_NEAR_FADE).clamp(0.0, 1.0));
        if near < 1.0 {
            tf.scale *= near;
        }
        if let Some(m) = flames.0.get(&p.layer) {
            mat.0 = m[((t * FLAME_STEPS as f32) as usize).min(FLAME_STEPS - 1)].clone();
        }
    }
}

// ---- Hull Crusher lightning: bolt def `HullCrush` on the `HullCrush Dim` ellipsoid -------------

#[derive(Resource)]
struct BoltArt {
    /// `[fade]` core and glow materials.
    core: Vec<Handle<StandardMaterial>>,
    glow: Vec<Handle<StandardMaterial>>,
}

struct Bolt {
    /// Endpoints as directions on the unit sphere (scaled onto the ellipsoid each frame).
    a: Vec3,
    b: Vec3,
    timer: f32,
    thick: f32,
}

#[derive(Component)]
struct BoltPool {
    bolts: Vec<Bolt>,
    /// `[bolt][joint][core, glow]`, flattened.
    segs: Vec<Entity>,
    noise_t: f32,
    noise_seed: u32,
}

#[derive(Component)]
struct BoltSeg;

fn hash(mut x: u32) -> f32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x % 10_000) as f32 / 10_000.0
}

fn load_bolt_art(mut commands: Commands, mut models: Models) {
    let d = &H2_BOLTS[h2_bolts_ids::HULLCRUSH];
    let glow_tex = models.lux_texture("ET_COM_RadGrade");
    let blend = if d.blend_mode.eq_ignore_ascii_case("add") { AlphaMode::Add } else { AlphaMode::Blend };
    let mats = |models: &mut Models, c: &[f32], intensity: f32, tex: Option<Handle<Image>>, mode: AlphaMode| -> Vec<Handle<StandardMaterial>> {
        (0..FADE_STEPS)
            .map(|k| {
                let a = c[3] * intensity * (1.0 - k as f32 / FADE_STEPS as f32);
                models.materials.add(StandardMaterial {
                    base_color: Color::LinearRgba(LinearRgba::new(c[0], c[1], c[2], a)),
                    base_color_texture: tex.clone(),
                    alpha_mode: mode,
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                })
            })
            .collect()
    };
    let core = mats(&mut models, d.bolt_color_motif_color, 1.0, None, blend);
    let glow = mats(&mut models, d.glow_color_motif_color, d.glow_intensity, glow_tex, AlphaMode::Add);
    commands.insert_resource(BoltArt { core, glow });
}

#[allow(clippy::too_many_arguments)]
fn hull_bolts(
    mut commands: Commands,
    time: Res<Time>,
    art: Option<Res<FxArt>>,
    bolt_art: Option<Res<BoltArt>>,
    tuning: Res<Tuning>,
    cam: Query<&Transform, (With<ChaseCam>, Without<BoltSeg>)>,
    mut boats: Query<(Entity, &Boat, Option<&mut BoltPool>)>,
    mut segs: Query<(&mut Transform, &mut MeshMaterial3d<StandardMaterial>), With<BoltSeg>>,
) {
    let (Some(art), Some(bolt_art)) = (art, bolt_art) else { return };
    let d = &H2_BOLTS[h2_bolts_ids::HULLCRUSH];
    let g = &tuning.0;
    let dt = time.delta_secs().min(1.0 / 20.0);
    let joints = phy::BOLT_JOINTS.max(1.0) as usize;
    let eye = cam.single().map(|c| c.translation).ok();
    for (e, b, pool) in &mut boats {
        let grown = b.crush.grown(&tuning);
        let Some(mut pool) = pool else {
            if grown > 0.0 {
                let count = d.bolt_count.max(1) as usize;
                let segs: Vec<Entity> = (0..count * joints * 2)
                    .map(|_| {
                        commands
                            .spawn((
                                Mesh3d(art.quad.clone()),
                                MeshMaterial3d(bolt_art.core[0].clone()),
                                Transform::from_scale(Vec3::ZERO),
                                BoltSeg,
                                NotShadowCaster,
                                DespawnOnExit(Screen::Race),
                            ))
                            .id()
                    })
                    .collect();
                let bolts = (0..count).map(|_| Bolt { a: Vec3::X, b: Vec3::X, timer: 0.0, thick: 0.0 }).collect();
                commands.entity(e).insert(BoltPool { bolts, segs, noise_t: 0.0, noise_seed: e.index_u32() });
            }
            continue;
        };
        if grown <= 0.0 {
            for s in pool.segs.drain(..) {
                commands.entity(s).despawn();
            }
            commands.entity(e).remove::<BoltPool>();
            continue;
        }
        // Fade in while deploying, out while stowing; brightness flickers in its range.
        let fade = (grown / d.fade_in.max(1e-3)).min(1.0).min(grown / d.fade_out.max(1e-3));
        let s = b.info.scale * (1.0 + (phy::HULLCRUSH_SCALE - 1.0) * grown);
        let radii = Vec3::new(g.hullcrush_dim_ratio_x, g.hullcrush_dim_ratio_y, g.hullcrush_dim_ratio_z) * g.hullcrush_dim_scale * 0.5 * s;
        let rot = Quat::from_rotation_y(b.yaw);
        let centre = b.pos + Vec3::Y * (b.info.def.hullcrush_dy * s + radii.y * 0.5);
        pool.noise_t += dt;
        if pool.noise_t >= 1.0 / d.noise_speed.max(1.0) {
            pool.noise_t = 0.0;
            pool.noise_seed = pool.noise_seed.wrapping_add(0x9e37_79b9);
        }
        let seed = pool.noise_seed;
        let BoltPool { bolts, segs: seg_ids, .. } = &mut *pool;
        for (bi, bolt) in bolts.iter_mut().enumerate() {
            bolt.timer -= dt;
            if bolt.timer <= 0.0 {
                let k = seed ^ (bi as u32).wrapping_mul(0x85eb_ca6b);
                let dir = |u: f32, v: f32| {
                    let (z, a) = (u * 2.0 - 1.0, v * std::f32::consts::TAU);
                    let r = (1.0 - z * z).max(0.0).sqrt();
                    Vec3::new(r * a.cos(), z, r * a.sin())
                };
                bolt.a = dir(hash(k), hash(k + 1));
                bolt.b = dir(hash(k + 2), hash(k + 3));
                bolt.timer = d.bolt_switch_speed.max(0.02);
                bolt.thick = d.bolt_thickness_min + (d.bolt_thickness_max - d.bolt_thickness_min) * hash(k + 4);
            }
            let bright = d.min_brightness + (d.max_brightness - d.min_brightness) * hash(seed ^ bi as u32);
            let step = (((1.0 - fade * bright) * FADE_STEPS as f32) as usize).min(FADE_STEPS - 1);
            // Joints crawl over the hull: a slerp between the endpoints, out to the ellipsoid,
            // jittered by the def's noise.
            let point = |j: usize| -> Vec3 {
                let t = j as f32 / joints as f32;
                let dir = bolt.a.slerp(bolt.b, t).normalize_or(Vec3::Y);
                let n = if j == 0 || j == joints { 0.0 } else { 1.0 };
                let h = seed ^ ((bi * 97 + j) as u32).wrapping_mul(0x27d4_eb2d);
                let jitter = Vec3::new(hash(h) - 0.5, hash(h + 7) - 0.5, hash(h + 13) - 0.5) * 2.0 * d.noise_amp * s * n;
                centre + rot * (dir * radii + jitter)
            };
            for j in 0..joints {
                let (p0, p1) = (point(j), point(j + 1));
                let mid = (p0 + p1) * 0.5;
                let along = (p1 - p0).normalize_or(Vec3::Y);
                let to_eye = eye.map_or(Vec3::Z, |e| (e - mid).normalize_or(Vec3::Z));
                let side = along.cross(to_eye).normalize_or(Vec3::X);
                let face = side.cross(along);
                let orient = Quat::from_mat3(&Mat3::from_cols(side, along, face));
                let len = p0.distance(p1);
                for (layer, width, mats) in [
                    (0, bolt.thick, &bolt_art.core),
                    (1, bolt.thick * (1.0 + d.glow_width * phy::BOLT_GLOW_SCALE), &bolt_art.glow),
                ] {
                    let id = seg_ids[(bi * joints + j) * 2 + layer];
                    if let Ok((mut tf, mut mat)) = segs.get_mut(id) {
                        *tf = Transform { translation: mid, rotation: orient, scale: Vec3::new(width, len, 1.0) };
                        mat.0 = mats[step].clone();
                    }
                }
            }
        }
    }
}

// ---- Level fires, smoke, torches, leaks and splashes: `<code>_Fire` (CRocketFlameEntity) ------

/// A rocket flame def running at a fixed spot of the level, emitting along its local +Z (-Z in
/// Riptide space after the mirror), like a boat nozzle that never stops.
#[derive(Component)]
pub struct LevelFire {
    flame: usize,
    scale: f32,
    due: HashMap<usize, f32>,
}

impl LevelFire {
    pub fn new(def: &str, scale: f32) -> Option<Self> {
        let flame = H2_ROCKET_FLAMES.iter().position(|f| f.id.eq_ignore_ascii_case(def))?;
        Some(Self { flame, scale: scale.max(0.01), due: HashMap::new() })
    }
}

fn emit_level_fires(
    mut commands: Commands,
    time: Res<Time>,
    art: Option<Res<FxArt>>,
    mut flames: ResMut<FlameArt>,
    mut models: Models,
    live: Query<&Flame>,
    cam: Query<&Transform, With<ChaseCam>>,
    mut fires: Query<(&Transform, &mut LevelFire)>,
) {
    let Some(art) = art else { return };
    let Ok(eye) = cam.single().map(|c| c.translation) else { return };
    let dt = time.delta_secs().min(1.0 / 20.0);
    let mut budget = (phy::LEVEL_FIRE_MAX_PARTICLES as usize).saturating_sub(live.iter().filter(|p| p.level && p.age + dt < p.life).count());
    let mut seed = (time.elapsed_secs() * 6271.0) as u32 | 1;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 10_000) as f32 / 10_000.0 * 2.0 - 1.0
    };
    // Nearest first, so the budget goes to what the player can see.
    let mut near: Vec<_> = fires.iter_mut().filter(|(tf, _)| tf.translation.distance(eye) < phy::LEVEL_FIRE_RANGE).collect();
    near.sort_by(|a, b| a.0.translation.distance(eye).total_cmp(&b.0.translation.distance(eye)));
    for (tf, mut fire) in near {
        let f = &H2_ROCKET_FLAMES[fire.flame];
        let direction = tf.rotation * Vec3::NEG_Z;
        let s = fire.scale;
        for layer in [f.layer0_def, f.layer1_def, f.layer2_def, f.layer3_def].into_iter().flatten() {
            let mat = flames.0.entry(layer).or_insert_with(|| flame_materials(&mut models, layer))[0].clone();
            let l = &H2_ROCKET_LAYERS[layer];
            // A puff each time the last one has travelled Puff Dist (the emitter stands still).
            let rate = (l.high_motion_speed.abs() / l.high_motion_puff_dist.max(0.01)).min(phy::LEVEL_FIRE_MAX_RATE);
            let due = fire.due.entry(layer).or_default();
            *due += rate * dt;
            let n = *due as usize;
            *due -= n as f32;
            for k in 0..n {
                if budget == 0 {
                    return;
                }
                budget -= 1;
                let born_at = (k as f32 + rand().abs()) / rate;
                let life = (l.high_motion_life_secs + l.high_motion_life_secs_spread * rand()).max(0.02).max(2.5 * dt);
                let speed = l.high_motion_speed + l.high_motion_speed_spread * rand();
                let jitter = tf.rotation * Vec3::new(rand() * l.high_motion_pos_delta_x, rand() * l.high_motion_pos_delta_y, 0.0) * s;
                let size = phy::ROCKET_SIZE * s;
                commands.spawn((
                    Mesh3d(art.quad.clone()),
                    MeshMaterial3d(mat.clone()),
                    Transform::from_translation(tf.translation + jitter).with_scale(Vec3::splat(l.high_scale0_val.max(0.01) * size)),
                    Flame {
                        level: true,
                        layer,
                        vel: direction * speed,
                        age: -born_at,
                        life,
                        size,
                        spin0: rand() * std::f32::consts::PI,
                        spin_birth: (l.high_rot_speed_birth * (1.0 + l.high_rot_speed_birth_spread * rand()).max(0.0)) * rand().signum(),
                        spin_death: l.high_rot_speed_death * (1.0 + l.high_rot_speed_death_spread * rand()).max(0.0),
                    },
                    NotShadowCaster,
                    DespawnOnExit(Screen::Race),
                ));
            }
        }
    }
}

/// Hull spray points for a boat def: each mesh `spray` surface's waterline edge (its lowest
/// vertices), `Waterspray <i> Subdivisions` points spaced along it, raised by `OffsetY`, thrown
/// along the surface's outward horizontal normal at `tan(Angle)` upward slope.
fn hull_spray_points(lux: &riptide_assets::lux::LuxArchive, def: &crate::sheets::H2BoatdefsRow) -> Vec<SprayPoint> {
    let Some(blob) = lux.get(&format!("mesh32.{}", def.mesh_name_local)) else { return Vec::new() };
    let table = [
        (def.waterspray_0_angle, def.waterspray_0_intensity, def.waterspray_0_offsety, def.waterspray_0_subdivisions),
        (def.waterspray_1_angle, def.waterspray_1_intensity, def.waterspray_1_offsety, def.waterspray_1_subdivisions),
        (def.waterspray_2_angle, def.waterspray_2_intensity, def.waterspray_2_offsety, def.waterspray_2_subdivisions),
        (def.waterspray_3_angle, def.waterspray_3_intensity, def.waterspray_3_offsety, def.waterspray_3_subdivisions),
    ];
    let lines = riptide_assets::h2mesh::spray_lines(blob);
    // Heights from the hull's waterline (its lowest spray vertex), which rides at the water surface.
    let waterline = lines.iter().flat_map(|l| l.triangles.iter().flatten()).map(|p| p[1]).fold(f32::MAX, f32::min);
    let mut out = Vec::new();
    for line in lines {
        let Some(&(angle, intensity, offset_y, subdiv)) = table.get(line.index as usize) else { continue };
        let n = subdiv.round() as usize;
        if n == 0 || line.triangles.is_empty() {
            continue;
        }
        let verts: Vec<Vec3> = line.triangles.iter().flatten().map(|p| Vec3::from(*p)).collect();
        let low = verts.iter().map(|v| v.y).fold(f32::MAX, f32::min);
        let mut edge: Vec<Vec3> = verts.into_iter().filter(|v| v.y < low + 1.0).collect();
        let normal = line.normals.iter().map(|n| Vec3::new(n[0], 0.0, n[2])).sum::<Vec3>().normalize_or(Vec3::Z);
        // Order the waterline points along the strip and drop duplicates.
        let axis = Vec3::Y.cross(normal).normalize_or(Vec3::X);
        edge.sort_by(|a, b| a.dot(axis).total_cmp(&b.dot(axis)));
        edge.dedup_by(|a, b| a.distance(*b) < 0.01);
        let (Some(&first), Some(&last)) = (edge.first(), edge.last()) else { continue };
        for k in 0..n {
            let f = if n == 1 { 0.5 } else { k as f32 / (n - 1) as f32 };
            let pos = first.lerp(last, f) + Vec3::Y * (offset_y - waterline + phy::HULL_SPRAY_LIFT);
            out.push(SprayPoint { pos, out: normal, slope: angle.clamp(0.0, 80.0).to_radians().tan(), intensity, due: 0.0 });
        }
    }
    out
}

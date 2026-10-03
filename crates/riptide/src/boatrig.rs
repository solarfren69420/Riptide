//! Boats' moving parts. An H2Overdrive boat is a skeleton: its `mesh32` pieces ride bones, its
//! upgrade meshes (`h2_upgrades`) hang off "channel" bones, and its `anim4` clips move the bones.
//! - Boost clip (`boats.anim`): frames up to the boat def's `Anim Boost Partition Frame` deploy
//!   the boosters (played while boosting), the rest stows them, at `Anim Boost Scale` speed.
//! - Wing clip (an upgrade's `anim`, e.g. Rogue Runner's wings): plays forward while airborne at
//!   `Anim Wing Deploy` speed, backward on landing at `Anim Wing Stow`.
//! - `RKBOOST_*` / `RKSUPERBOOST` bones are the rocket nozzles the boost flames come out of.

use crate::content::{BoatInfo, Models, Rig};
use crate::race::Boat;
use crate::sheets::{BoatLoopsWhen, BOATS, BOAT_LOOPS, H2_UPGRADES};
use crate::Screen;
use bevy::prelude::*;
use riptide_assets::h2anim::{keys_at, Clip, FPS};
use std::collections::HashMap;
use std::sync::Arc;

pub struct BoatRigPlugin;

impl Plugin for BoatRigPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (animate.run_if(in_state(Screen::Race)), pose_menu.run_if(in_state(Screen::Menu))));
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
enum Phase {
    #[default]
    Idle,
    Deploy,
    Stow,
}

/// One clip bound to the bone entities its tracks name.
struct Bound {
    clip: Arc<Clip>,
    /// (track index, bone entity, the bone's rest transform).
    nodes: Vec<(usize, Entity, Transform)>,
    /// Seconds into the clip.
    t: f32,
    phase: Phase,
}

/// A spawned boat skeleton (on the boat entity).
#[derive(Component)]
pub struct BoatRig {
    boost: Option<Bound>,
    wings: Option<Bound>,
    /// Looping clips (`boat_loops`) and when they play.
    loops: Vec<(Bound, BoatLoopsWhen)>,
    /// Boat def timing: boost partition (s), boost clip speed, wing deploy / stow speeds.
    partition: f32,
    boost_speed: f32,
    wing_deploy: f32,
    wing_stow: f32,
    /// Rocket nozzles (`RKBOOST_*`), and the gold super-boost one (`RKSUPERBOOST`).
    pub nozzles: Vec<Entity>,
    pub super_nozzle: Option<Entity>,
}

fn mat(m: &[f32; 16]) -> Mat4 {
    Mat4::from_cols_array(m)
}

/// Spawn `rig`'s bones under `parent`. A bone whose parent isn't spawned hangs off `parent` with
/// its transform relative to `base` (the rest pose `parent` stands for). Pieces ride their bones.
/// Returns the bone entities by lower-case name.
fn spawn_bones(commands: &mut Commands, rig: &Rig, parent: Entity, base: Mat4, skip: &[bool]) -> HashMap<String, Entity> {
    let n = rig.bones.len();
    let mut ent: Vec<Option<Entity>> = vec![None; n];
    // Parents before children: a bounded number of passes over the list.
    for _ in 0..n {
        let mut progress = false;
        for (i, b) in rig.bones.iter().enumerate() {
            if ent[i].is_some() || skip[i] {
                continue;
            }
            let (owner, owner_rest) = match b.parent {
                Some(p) if !skip[p] => match ent[p] {
                    Some(e) => (e, mat(&rig.bones[p].rest)),
                    None => continue,
                },
                _ => (parent, base),
            };
            let local = owner_rest.inverse() * mat(&b.rest);
            let e = commands.spawn((Transform::from_matrix(local), Visibility::default(), Name::new(b.name.clone()))).id();
            commands.entity(owner).add_child(e);
            ent[i] = Some(e);
            progress = true;
        }
        if !progress {
            break;
        }
    }
    for p in &rig.pieces {
        // A piece rides its bone; if that bone wasn't spawned (an attachment's copy of the
        // channel bone or the root above it), it hangs off `parent`, placed by its bone's rest
        // pose relative to `base`: a root at the model origin is not guaranteed (Scorpion's
        // sits 49 units forward).
        let (owner, place) = match p.bone.map(|b| (b, ent.get(b as usize).copied().flatten())) {
            Some((_, Some(e))) => (e, Transform::IDENTITY),
            Some((b, None)) => (parent, rig.bones.get(b as usize).map_or(Transform::IDENTITY, |bone| Transform::from_matrix(base.inverse() * mat(&bone.rest)))),
            None => (parent, Transform::IDENTITY),
        };
        commands.entity(owner).with_children(|c| {
            c.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(p.material.clone()), place));
        });
    }
    rig.bones.iter().zip(ent).filter_map(|(b, e)| Some((b.name.to_ascii_lowercase(), e?))).collect()
}

fn bind(clip: Arc<Clip>, bones: &HashMap<String, Entity>, rests: &HashMap<Entity, Transform>) -> Bound {
    let nodes = clip
        .tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let e = *bones.get(&t.name.to_ascii_lowercase())?;
            Some((i, e, rests.get(&e).copied().unwrap_or_default()))
        })
        .collect();
    Bound { clip, nodes, t: 0.0, phase: Phase::Idle }
}

/// Build `info`'s skeleton under `model` (the boat's scaled model node), with its upgrade parts
/// for `boats.upgrade_level`. `None` when the boat has no rig (Hydro Thunder boats): draw it whole.
pub fn spawn(commands: &mut Commands, models: &mut Models, info: &BoatInfo, model: Entity) -> Option<BoatRig> {
    let mesh = info.row.model.strip_prefix("lux:mesh32.")?;
    let rig = models.lux_rig(mesh)?;
    if rig.bones.is_empty() {
        return None;
    }
    let mut bones = spawn_bones(commands, &rig, model, Mat4::IDENTITY, &vec![false; rig.bones.len()]);
    let rest_of = |rig: &Rig, name: &str| rig.bones.iter().find(|b| b.name.eq_ignore_ascii_case(name)).map(|b| mat(&b.rest));
    let mut rests: HashMap<Entity, Transform> = HashMap::new();
    for b in &rig.bones {
        if let Some(&e) = bones.get(&b.name.to_ascii_lowercase()) {
            let local = match b.parent {
                Some(p) => mat(&rig.bones[p].rest).inverse() * mat(&b.rest),
                None => mat(&b.rest),
            };
            rests.insert(e, Transform::from_matrix(local));
        }
    }

    // Upgrade parts for this boat at its level, on their channel bones.
    let mut wing_clip = None;
    let def = info.def.id;
    for row in H2_UPGRADES.iter().filter(|u| u.level == info.row.upgrade_level && u.boat.is_some_and(|b| crate::sheets::H2_BOATDEFS[b].id == def)) {
        let Some(part) = row.mesh.strip_prefix("lux:mesh32.") else { continue };
        let Some(att) = models.lux_rig(part) else { continue };
        let channel = bones.get(&row.channel.to_ascii_lowercase()).copied().unwrap_or(model);
        // The attachment carries its own copy of the channel bone (and the root above it):
        // those are the main boat's; the rest hangs under the main channel entity.
        let ch = att.bones.iter().position(|b| b.name.eq_ignore_ascii_case(row.channel));
        let mut skip = vec![false; att.bones.len()];
        if let Some(c) = ch {
            let mut k = Some(c);
            while let Some(i) = k {
                skip[i] = true;
                k = att.bones[i].parent;
            }
        }
        let base = ch.map(|c| mat(&att.bones[c].rest)).or_else(|| rest_of(&rig, row.channel)).unwrap_or(Mat4::IDENTITY);
        let added = spawn_bones(commands, &att, channel, base, &skip);
        for b in &att.bones {
            if let Some(&e) = added.get(&b.name.to_ascii_lowercase()) {
                let parent_rest = match b.parent {
                    Some(p) if !skip[p] => mat(&att.bones[p].rest),
                    _ => base,
                };
                rests.insert(e, Transform::from_matrix(parent_rest.inverse() * mat(&b.rest)));
            }
        }
        bones.extend(added);
        if let Some(a) = row.anim.strip_prefix("lux:anim4.") {
            wing_clip = models.lux_clip(a);
        }
    }

    let boost = info.row.anim.strip_prefix("lux:anim4.").and_then(|a| models.lux_clip(a)).map(|c| bind(c, &bones, &rests));
    let wings = wing_clip.map(|c| bind(c, &bones, &rests));
    // Looping clips for this boat (`boat_loops`): overlays and boost loops.
    let loops = BOAT_LOOPS
        .iter()
        .filter(|l| l.boat.is_some_and(|b| BOATS[b].id == info.row.id))
        .filter_map(|l| {
            let clip = models.lux_clip(l.clip.strip_prefix("lux:anim4.")?)?;
            let bound = bind(clip, &bones, &rests);
            if std::env::var_os("RIPTIDE_DEBUG").is_some() {
                let names: Vec<&str> = bound.nodes.iter().map(|(ti, _, _)| bound.clip.tracks[*ti].name.as_str()).collect();
                info!("rig loop {} on {}: {} bones {:?}", l.id, info.name, names.len(), names);
            }
            (!bound.nodes.is_empty()).then_some((bound, l.when))
        })
        .collect();
    let mut nozzles: Vec<(String, Entity)> = bones.iter().filter(|(n, _)| n.starts_with("rkboost")).map(|(n, e)| (n.clone(), *e)).collect();
    nozzles.sort();
    let def = info.def;
    Some(BoatRig {
        boost,
        wings,
        loops,
        partition: def.anim_boost_partition_frame as f32 / FPS,
        boost_speed: def.anim_boost_scale.max(0.05),
        wing_deploy: def.anim_wing_deploy.max(0.05),
        wing_stow: def.anim_wing_stow.max(0.05),
        nozzles: nozzles.into_iter().map(|(_, e)| e).collect(),
        super_nozzle: bones.get("rksuperboost").copied(),
    })
}

/// Direct3D local key -> output space (Z mirrored).
fn sample(b: &Bound, track: usize, rest: Transform) -> Transform {
    let tr = &b.clip.tracks[track];
    let f = (b.t / b.clip.duration.max(1e-3)).clamp(0.0, 1.0);
    let translation = keys_at(&tr.pos, f).map_or(rest.translation, |(a, c, k)| {
        let v = Vec3::from(a).lerp(Vec3::from(c), k);
        Vec3::new(v.x, v.y, -v.z)
    });
    let rotation = keys_at(&tr.rot, f).map_or(rest.rotation, |(a, c, k)| {
        let q = |r: [f32; 4]| Quat::from_xyzw(-r[0], -r[1], r[2], r[3]).normalize();
        q(a).slerp(q(c), k)
    });
    let scale = keys_at(&tr.scale, f).map_or(rest.scale, |(a, c, k)| Vec3::from(a).lerp(Vec3::from(c), k));
    Transform { translation, rotation, scale }
}

/// Advance a rig: `on` = boost held (deploy, hold, stow), `airborne` = wings out; loops play
/// per their `boat_loops.when`.
fn step(rig: &mut BoatRig, on: bool, airborne: bool, dt: f32) {
    let (partition, speed, wing_deploy, wing_stow) = (rig.partition, rig.boost_speed, rig.wing_deploy, rig.wing_stow);
    let mut deployed = false;
    {
        // Boost: deploy to the partition frame and hold; stow through the rest of the clip.
        if let Some(c) = rig.boost.as_mut() {
            let end = c.clip.duration.max(1e-3);
            let p = partition.clamp(0.0, end);
            c.phase = match (c.phase, on) {
                (Phase::Idle, true) => {
                    c.t = 0.0;
                    Phase::Deploy
                }
                (Phase::Deploy, false) => {
                    // Mirror the deploy progress into the stow half.
                    c.t = p + (1.0 - c.t / p.max(1e-3)) * (end - p);
                    Phase::Stow
                }
                (Phase::Stow, true) => {
                    c.t = p * (1.0 - (c.t - p) / (end - p).max(1e-3));
                    Phase::Deploy
                }
                (phase, _) => phase,
            };
            match c.phase {
                Phase::Deploy => c.t = (c.t + dt * speed).min(p),
                Phase::Stow => {
                    c.t += dt * speed;
                    if c.t >= end {
                        c.t = 0.0;
                        c.phase = Phase::Idle;
                    }
                }
                Phase::Idle => c.t = 0.0,
            }
            deployed = c.phase == Phase::Deploy && c.t >= p;
        }
    }
    // Wings: open in the air, fold on the water.
    if let Some(w) = rig.wings.as_mut() {
        let end = w.clip.duration.max(1e-3);
        w.t = if airborne { (w.t + dt * wing_deploy).min(end) } else { (w.t - dt * wing_stow).max(0.0) };
    }
    // Loops: overlays always; boost loops once the boost parts are out; idle loops otherwise.
    let no_boost = rig.boost.is_none();
    for (bound, when) in &mut rig.loops {
        let play = match when {
            BoatLoopsWhen::Always => true,
            BoatLoopsWhen::Boosting => deployed || (no_boost && on),
            BoatLoopsWhen::Idle => !on,
        };
        if play {
            bound.t = (bound.t + dt) % bound.clip.duration.max(1e-3);
        }
    }
}

/// Pose a rig's bones: boost and wing clips, then any playing loops on top.
fn apply(rig: &BoatRig, on: bool, bones: &mut Query<&mut Transform, Without<Boat>>) {
    let loops = rig.loops.iter().filter(|(_, when)| *when != BoatLoopsWhen::Idle || !on).map(|(b, _)| b);
    for bound in [rig.boost.as_ref(), rig.wings.as_ref()].into_iter().flatten().chain(loops) {
        for &(ti, e, rest) in &bound.nodes {
            if let Ok(mut tf) = bones.get_mut(e) {
                *tf = sample(bound, ti, rest);
            }
        }
    }
}

pub(crate) fn animate(time: Res<Time>, mut boats: Query<(&Boat, &mut BoatRig)>, mut bones: Query<&mut Transform, Without<Boat>>) {
    let dt = time.delta_secs().min(0.1);
    for (b, mut rig) in &mut boats {
        let on = b.boosting || b.super_time > 0.0;
        step(&mut rig, on, b.airborne, dt);
        apply(&rig, on, &mut bones);
    }
}

/// The menu's turntable boat shows its moving parts: every 8 s it deploys and stows its
/// boosters (and opens any wings), with its loops playing. `RIPTIDE_TEST_RIG_T=<seconds>`
/// freezes the boost clip at that time instead (for checking the clips).
fn pose_menu(time: Res<Time>, mut rigs: Query<&mut BoatRig, Without<Boat>>, mut bones: Query<&mut Transform, Without<Boat>>) {
    let fixed = std::env::var("RIPTIDE_TEST_RIG_T").ok().and_then(|s| s.parse::<f32>().ok());
    let cycle = time.elapsed_secs() % 8.0;
    let (on, airborne) = ((1.5..5.0).contains(&cycle), (5.5..7.5).contains(&cycle));
    for mut rig in &mut rigs {
        match fixed {
            Some(t) => {
                let r = &mut *rig;
                for bound in [r.boost.as_mut(), r.wings.as_mut()].into_iter().flatten() {
                    bound.t = t.min(bound.clip.duration);
                }
            }
            None => step(&mut rig, on, airborne, time.delta_secs().min(0.1)),
        }
        apply(&rig, on, &mut bones);
    }
}

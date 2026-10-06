//! Hydro Thunder animated objects (seagulls, parrots, bats): a `G<name>H1` geometry's nodes moved
//! by its `A<name>H1` clip (riptide_assets::htanim). Each node piece is a child of the instance;
//! its transform is the clip's pose at the current frame times the inverse of frame 0 (the
//! geometry is stored in its frame-0 pose), looping.

use bevy::prelude::*;
use riptide_assets::htanim::HtClip;
use std::sync::Arc;

#[derive(Component)]
pub struct HtAnimator {
    pub clip: Arc<HtClip>,
    pub t: f32,
}

/// A node piece: which clip track moves it.
#[derive(Component)]
pub struct HtNode {
    pub track: usize,
}

/// The clip track for a piece tagged with node id `bone`: the track naming that node, else
/// track 0 (the root part, the object's first group).
pub fn track_for(clip: &HtClip, bone: Option<u16>) -> usize {
    bone.and_then(|b| clip.tracks.iter().position(|t| t.node == Some(b))).unwrap_or(0)
}

pub fn animate(time: Res<Time>, mut anims: Query<(&mut HtAnimator, &Children)>, mut nodes: Query<(&HtNode, &mut Transform)>) {
    let dt = time.delta_secs();
    for (mut a, children) in &mut anims {
        a.t += dt;
        let frames = a.clip.tracks.first().map_or(1, |t| t.poses.len()).max(1);
        let f = (a.t / a.clip.seconds_per_frame.max(1e-3)) as usize % frames;
        for c in children.iter() {
            let Ok((node, mut tf)) = nodes.get_mut(c) else { continue };
            let Some(track) = a.clip.tracks.get(node.track) else { continue };
            let (Some(now), Some(first)) = (track.poses.get(f), track.poses.first()) else { continue };
            let m = Mat4::from_cols_array(now) * Mat4::from_cols_array(first).inverse();
            *tf = Transform::from_matrix(m);
        }
    }
}

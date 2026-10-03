//! The water material: Bevy's StandardMaterial extended with a GPU water shader (water.wgsl):
//! waves on the GPU (the same formula the boats bob on), two scrolling layers of the level's
//! water normal map, fresnel and crest foam. Settings come from the `physics` sheet.

use crate::sheets::physics as phy;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

pub type WaterMat = ExtendedMaterial<StandardMaterial, WaterExt>;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "water.wgsl");
        app.add_plugins(MaterialPlugin::<WaterMat>::default());
    }
}

#[derive(Clone, Copy, Debug, ShaderType, Reflect)]
pub struct WaterParams {
    pub time: f32,
    pub amplitude: f32,
    pub wave_k: f32,
    pub wave_speed: f32,
    pub normal_scale: f32,
    pub normal_strength: f32,
    pub foam: f32,
    pub gloss: f32,
}

impl Default for WaterParams {
    fn default() -> Self {
        Self {
            time: 0.0,
            amplitude: phy::WAVE_AMPLITUDE,
            wave_k: std::f32::consts::TAU / phy::WAVE_LENGTH.max(1.0),
            wave_speed: phy::WAVE_SPEED,
            normal_scale: 1.0 / phy::WATER_RIPPLE_SIZE.max(1.0),
            normal_strength: phy::WATER_RIPPLE_STRENGTH,
            foam: phy::WATER_FOAM,
            gloss: phy::WATER_GLOSS,
        }
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct WaterExt {
    #[uniform(100)]
    pub params: WaterParams,
    /// The level's water normal map (ripples).
    #[texture(101)]
    #[sampler(102)]
    pub ripples: Option<Handle<Image>>,
}

impl MaterialExtension for WaterExt {
    fn vertex_shader() -> ShaderRef {
        "embedded://riptide/water.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "embedded://riptide/water.wgsl".into()
    }
}

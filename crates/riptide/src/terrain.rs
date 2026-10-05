//! H2Overdrive's two-texture terrain (`shad4.OP_2VP` family): Bevy's standard material extended
//! with the second texture on the second UV set, blended in by its alpha x the vertex alpha as
//! the original pixel program does (terrain.wgsl).

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

pub type TerrainMat = ExtendedMaterial<StandardMaterial, TerrainExt>;

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "terrain.wgsl");
        app.add_plugins(MaterialPlugin::<TerrainMat>::default());
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TerrainExt {
    /// The material's second texture (texture slot 1).
    #[texture(100)]
    #[sampler(101)]
    pub second: Handle<Image>,
    /// x: 0 = two-texture blend (OP_2V*), 1 = lightmap (OP_*L*: `second` is the `_LM` texture on the
    /// second UV set, adding its light); y: lightmap strength.
    #[uniform(102)]
    pub params: Vec4,
}

impl MaterialExtension for TerrainExt {
    fn fragment_shader() -> ShaderRef {
        "embedded://riptide/terrain.wgsl".into()
    }
}

//! H2Overdrive's own boat shader running in Riptide: `shad4.FX_BoatLocal`'s pixel program (the
//! paint model: diffuse x tint, three light masks x their colours, a tangent-space normal map,
//! sun and ambient light, specular, and a cube-map reflection scaled by the paint's gloss alpha),
//! translated to WGSL by `d3d9-shader`. Riptide draws its boat parts rigidly on their bones, so
//! the vertex side is Bevy's own mesh transform (`boatshader_vs.wgsl`) handing the pixel program
//! the varyings the original vertex program produced: world position, normal, binormal, tangent
//! and the texture coordinates.
//!
//! Inputs from the scene: the camera position (Bevy's view), the race's sun and ambient light
//! and the course's sky cube map (`sync_lights`). Material constants: H2Overdrive boat materials
//! carry white colour slots and a specular exponent of 20 (their `0x4d0` records). On by default;
//! `RIPTIDE_BOATSHADER=0` keeps Bevy's standard material.

use crate::h2water::D3d9Block;
use bevy::asset::uuid_handle;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;
use d3d9_shader::{wgsl, Program};

const FRAGMENT: Handle<Shader> = uuid_handle!("6f0b8c52-3a51-4b0e-9a8e-2f1d6c0a9e21");

pub struct BoatShaderPlugin;

impl Plugin for BoatShaderPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "boatshader_vs.wgsl");
        app.add_plugins(MaterialPlugin::<BoatMaterial>::default()).add_systems(PostUpdate, sync_lights);
    }
}

/// Sampler bindings from 2 (d3d9_shader's layout): s0 paint, s1-s3 light masks, s4 normal map,
/// s5 reflection cube.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct BoatMaterial {
    #[uniform(0)]
    pub ps: D3d9Block,
    #[texture(2)]
    #[sampler(3)]
    pub paint: Handle<Image>,
    #[texture(4)]
    #[sampler(5)]
    pub mask1: Handle<Image>,
    #[texture(6)]
    #[sampler(7)]
    pub mask2: Handle<Image>,
    #[texture(8)]
    #[sampler(9)]
    pub mask3: Handle<Image>,
    #[texture(10)]
    #[sampler(11)]
    pub normal: Handle<Image>,
    #[texture(12, dimension = "cube")]
    #[sampler(13)]
    pub sky: Handle<Image>,
}

impl Material for BoatMaterial {
    fn vertex_shader() -> ShaderRef {
        "embedded://riptide/boatshader_vs.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        FRAGMENT.into()
    }
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
            Mesh::ATTRIBUTE_TANGENT.at_shader_location(3),
        ])?];
        if let Some(f) = descriptor.fragment.as_mut() {
            f.entry_point = Some("fragment".into());
        }
        Ok(())
    }
}

/// The translated pixel program (for its constant table) once installed.
#[derive(Clone)]
pub struct BoatShader {
    pub ps: Program,
}

static INSTALLED: std::sync::OnceLock<BoatShader> = std::sync::OnceLock::new();

/// Translate `shad4.FX_BoatLocal`'s first pixel program once per run.
pub fn install(shaders: &mut Assets<Shader>, blob: &[u8]) -> Option<BoatShader> {
    if let Some(sh) = INSTALLED.get() {
        if shaders.contains(&FRAGMENT) {
            return Some(sh.clone());
        }
    }
    let ps = d3d9_shader::programs(blob).into_iter().find(|p| p.pixel)?;
    let mut o = wgsl::Options { group: "#{MATERIAL_BIND_GROUP}".into(), constants_binding: 0, first_texture: 2, entry: "fragment".into(), ..default() };
    o.prelude = "#import bevy_pbr::mesh_view_bindings::view".into();
    if let Some(c) = ps.constants.iter().find(|c| c.name == "g_CamPos_WS" && c.set == 2) {
        o.constants.insert(c.index as u32, "vec4<f32>(view.world_position, 1.0)".into());
    }
    let src = wgsl::translate(&ps, &o);
    if std::env::var_os("RIPTIDE_BOATSHADER_DUMP").is_some() {
        let _ = std::fs::write("boatshader_ps.wgsl", &src);
    }
    let _ = shaders.insert(&FRAGMENT, Shader::from_wgsl(src, "riptide://boatshader_ps.wgsl"));
    let sh = BoatShader { ps };
    let _ = INSTALLED.set(sh.clone());
    Some(sh)
}

/// Set constant `name` (element `i`) in every register file the program declares it in.
fn set(block: &mut D3d9Block, p: &Program, name: &str, i: usize, v: Vec4) {
    for c in p.constants.iter().filter(|c| c.name == name && i < c.count.max(1) as usize) {
        let r = c.index as usize + i;
        match c.set {
            2 if r < 256 => block.c[r] = v,
            1 if r < 16 => block.i[r] = v.as_ivec4(),
            0 if r < 16 => block.b[r] = UVec4::splat((v.x != 0.0) as u32),
            _ => {}
        }
    }
}

/// The material constants for one boat part (the scene's lights are filled in by `sync_lights`).
pub fn constants(sh: &BoatShader, specular_exponent: f32) -> D3d9Block {
    let mut b = D3d9Block::default();
    let one = Vec4::ONE;
    for n in ["g_vDiffuseColor", "g_vGlossAlpha", "g_vReflectionColor", "g_vSpecularColor", "g_vTintColor", "g_vEmissiveColor", "g_vEmissiveColor2", "g_vEmissiveColor3"] {
        set(&mut b, &sh.ps, n, 0, one);
    }
    set(&mut b, &sh.ps, "g_fSpecularExponent", 0, Vec4::splat(specular_exponent));
    set(&mut b, &sh.ps, "g_DirLight_uCount", 0, Vec4::splat(1.0));
    set(&mut b, &sh.ps, "g_DirLight_avUnitDir", 0, Vec4::new(-0.4, 0.8, 0.45, 0.0).normalize());
    set(&mut b, &sh.ps, "g_DirLight_avColor", 0, Vec4::new(1.0, 0.95, 0.85, 1.0));
    set(&mut b, &sh.ps, "g_AmbLight_vColor", 0, Vec4::new(0.45, 0.47, 0.5, 1.0));
    b
}

/// Feed every boat material the scene's sun (direction to it, colour scaled to the original's
/// 0..1 range) and ambient light, and the race camera's sky cube map for reflections.
fn sync_lights(
    sun: Query<(&DirectionalLight, &GlobalTransform)>,
    ambient: Option<Res<GlobalAmbientLight>>,
    mut materials: ResMut<Assets<BoatMaterial>>,
    mut applied: Local<(Vec3, Vec3, usize)>,
) {
    let Some(sh) = INSTALLED.get() else { return };
    let Some((light, tf)) = sun.iter().next() else { return };
    // Only when the sun, its colour or the set of materials changed (touching a material re-uploads it).
    let key = (-tf.forward().as_vec3(), light.color.to_linear().to_vec3(), materials.len());
    if key.0.abs_diff_eq(applied.0, 1e-4) && key.1.abs_diff_eq(applied.1, 1e-4) && key.2 == applied.2 {
        return;
    }
    *applied = key;
    for (_, m) in materials.iter_mut() {
            let to_sun = -tf.forward().as_vec3();
            let c = light.color.to_linear();
            let peak = c.red.max(c.green).max(c.blue).max(1e-3);
            set(&mut m.ps, &sh.ps, "g_DirLight_avUnitDir", 0, to_sun.extend(0.0));
            set(&mut m.ps, &sh.ps, "g_DirLight_avColor", 0, Vec4::new(c.red / peak, c.green / peak, c.blue / peak, 1.0));
            if let Some(a) = ambient.as_ref() {
                let a = a.color.to_linear();
                set(&mut m.ps, &sh.ps, "g_AmbLight_vColor", 0, Vec4::new(a.red, a.green, a.blue, 1.0) * crate::sheets::physics::BOATSHADER_AMBIENT);
            }
    }
}

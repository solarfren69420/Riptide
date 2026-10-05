//! H2Overdrive's own water shader running in Riptide. `shad4.FX_Water2`'s vertex and pixel
//! programs are translated to WGSL by `d3d9-shader` when a level loads, and every water sector
//! (the stretch between two `CSPropWaterEdge`s) is drawn with the constants the original engine
//! fed them: the sector's corners and inward edge normals, a Catmull-Rom spline through the
//! neighbouring edges (the flow texture follows the river), each edge's waves, bump scroll,
//! colours and light, blended across the sector by the shader itself.
//!
//! The engine side the original supplied per frame comes from Bevy: the camera matrix and
//! position and the time (constant overrides read Bevy's view uniform, so the same material
//! serves any camera), and the refraction texture (Bevy's view transmission texture: the opaque
//! scene behind the water). Still stand-ins (CHECKLIST.md): the reflection render (a sky colour),
//! the screen-space whitewash buffer (none), and the wave / bump / flow numbers derived from an
//! edge's Wave Type and speeds (being decoded from sdaemon.exe).
//!
//! The default water on H2Overdrive courses; `RIPTIDE_H2WATER=0` uses Riptide's own water
//! (crate::water) instead.

use bevy::asset::uuid_handle;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;
use d3d9_shader::{wgsl, Program};
use crate::sheets::physics as phy;
use riptide_assets::h2level::{H2Level, WaterEdge};

const VERTEX: Handle<Shader> = uuid_handle!("6f0b8c52-3a51-4b0e-9a8e-2f1d6c0a9e11");
const FRAGMENT: Handle<Shader> = uuid_handle!("6f0b8c52-3a51-4b0e-9a8e-2f1d6c0a9e12");

pub struct H2WaterPlugin;

impl Plugin for H2WaterPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<H2WaterMaterial>::default())
            .add_plugins(MaterialPlugin::<WaterfallMaterial>::default())
            .add_systems(PostUpdate, follow_reflection.before(bevy::transform::TransformSystems::Propagate));
    }
}

/// One Direct3D 9 constant file: 256 float4, 16 int4, 16 bool registers (d3d9_shader::wgsl's
/// `D3d9Constants`).
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct D3d9Block {
    pub c: [Vec4; 256],
    pub i: [IVec4; 16],
    pub b: [UVec4; 16],
}

impl Default for D3d9Block {
    fn default() -> Self {
        Self { c: [Vec4::ZERO; 256], i: [IVec4::ZERO; 16], b: [UVec4::ZERO; 16] }
    }
}

/// Sampler bindings follow d3d9_shader's layout from binding 2: `sN` at 2 + 2N (texture) and
/// 3 + 2N (sampler). s1 (refraction) is Bevy's view transmission texture instead.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct H2WaterMaterial {
    #[uniform(0)]
    pub vs: D3d9Block,
    #[uniform(1)]
    pub ps: D3d9Block,
    /// s0: `txtr1.wavesbump`.
    #[texture(2)]
    #[sampler(3)]
    pub bump: Handle<Image>,
    /// s2: the reflection render.
    #[texture(6)]
    #[sampler(7)]
    pub reflection: Handle<Image>,
    /// s3: the screen-space whitewash buffer (FX_WaterWhite draws wakes into it).
    #[texture(8)]
    #[sampler(9)]
    pub whitewash: Handle<Image>,
}

impl Material for H2WaterMaterial {
    fn vertex_shader() -> ShaderRef {
        VERTEX.into()
    }
    fn fragment_shader() -> ShaderRef {
        FRAGMENT.into()
    }
    fn reads_view_transmission_texture(&self) -> bool {
        true
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The vertex program reads one input: texcoord0 = (x, z) from the sector's mesh origin.
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?];
        descriptor.vertex.entry_point = Some("vertex".into());
        if let Some(f) = descriptor.fragment.as_mut() {
            f.entry_point = Some("fragment".into());
        }
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// The translated programs (kept for their constant tables: values are set by name).
#[derive(Resource, Clone)]
pub struct H2WaterShaders {
    pub vs: Program,
    pub ps: Program,
}

/// Translate `shad4.FX_Water2` (its fullest vertex / pixel pair: leading and trailing edges,
/// lights, tidal waves) and install it as the material's shaders.
pub fn install(shaders: &mut Assets<Shader>, blob: &[u8]) -> Option<H2WaterShaders> {
    // Once per run: inserting the shaders again (a restart) replaced them under the materials of
    // the new race, which then drew as a flat mirror.
    static DONE: std::sync::OnceLock<H2WaterShaders> = std::sync::OnceLock::new();
    if let Some(sh) = DONE.get() {
        if shaders.contains(&VERTEX) && shaders.contains(&FRAGMENT) {
            return Some(sh.clone());
        }
    }
    let sh = install_once(shaders, blob)?;
    let _ = DONE.set(sh.clone());
    Some(sh)
}

fn install_once(shaders: &mut Assets<Shader>, blob: &[u8]) -> Option<H2WaterShaders> {
    let progs = d3d9_shader::programs(blob);
    let vs = progs.iter().filter(|p| !p.pixel).max_by_key(|p| p.tokens.len())?.clone();
    let ps = progs.iter().filter(|p| p.pixel).max_by_key(|p| p.tokens.len())?.clone();

    let mut o = wgsl::Options { group: "#{MATERIAL_BIND_GROUP}".into(), constants_binding: 0, first_texture: 2, entry: "vertex".into(), ..default() };
    o.prelude = "#import bevy_pbr::mesh_view_bindings::{view, globals}".into();
    let reg = |p: &Program, name: &str| p.constants.iter().find(|c| c.name == name && c.set == 2).map(|c| c.index as u32);
    // World = identity (positions are world space): g_MtxWorldViewProj's register k is row k of
    // Bevy's clip_from_world (the program dp4s against it).
    if let Some(r) = reg(&vs, "g_MtxWorldViewProj") {
        for k in 0..4 {
            o.constants.insert(r + k, format!("vec4<f32>(view.clip_from_world[0][{k}], view.clip_from_world[1][{k}], view.clip_from_world[2][{k}], view.clip_from_world[3][{k}])"));
        }
    }
    // g_MtxProjTexture maps world to screen texture coordinates (u right, v down), scaled by w; the
    // program sums x * c[r] + y * c[r+1] + z * c[r+2] + c[r+3], so register k is column k.
    if let Some(r) = reg(&vs, "g_MtxProjTexture") {
        for k in 0..4 {
            let m = format!("view.clip_from_world[{k}]");
            o.constants.insert(r + k, format!("vec4<f32>(0.5 * ({m}.x + {m}.w), 0.5 * ({m}.w - {m}.y), {m}.z, {m}.w)"));
        }
    }
    if let Some(r) = reg(&vs, "g_CamPos_WS") {
        o.constants.insert(r, "vec4<f32>(view.world_position, 1.0)".into());
    }
    if let Some(r) = reg(&vs, "g_fElapsedSecs") {
        o.constants.insert(r, "vec4<f32>(globals.time)".into());
    }
    let vs_src = wgsl::translate(&vs, &o);

    let mut o = wgsl::Options { group: "#{MATERIAL_BIND_GROUP}".into(), constants_binding: 1, first_texture: 2, entry: "fragment".into(), ..default() };
    o.prelude = "#import bevy_pbr::mesh_view_bindings::{view_transmission_texture, view_transmission_sampler}\nfn h2_flip_v(c: vec2<f32>) -> vec2<f32> { return vec2<f32>(c.x, 1.0 - c.y); }".into();
    o.samplers.insert(1, ("view_transmission_texture".into(), "view_transmission_sampler".into()));
    // The reflection camera renders the mirror image upside down (see `follow_reflection`).
    o.coords.insert(2, "h2_flip_v".into());
    let ps_src = wgsl::translate(&ps, &o);

    if std::env::var_os("RIPTIDE_H2WATER_DUMP").is_some() {
        let _ = std::fs::write("h2water_vs.wgsl", &vs_src);
        let _ = std::fs::write("h2water_ps.wgsl", &ps_src);
    }
    let _ = shaders.insert(&VERTEX, Shader::from_wgsl(vs_src, "riptide://h2water_vs.wgsl"));
    let _ = shaders.insert(&FRAGMENT, Shader::from_wgsl(ps_src, "riptide://h2water_ps.wgsl"));
    Some(H2WaterShaders { vs, ps })
}

/// Set constant `name` (element `i`) in `block` to `v`, in every register file the program
/// declares it in (a loop count is both an int and a float constant).
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

fn xz(p: [f32; 3]) -> Vec2 {
    Vec2::new(p[0], p[2])
}

/// Unit direction for an authored angle in degrees (original space; Riptide mirrors Z).
fn heading(deg: f32) -> Vec2 {
    let (s, c) = deg.to_radians().sin_cos();
    Vec2::new(c, -s)
}

/// Waves for an edge: (unit direction XZ, speed, angular wave number, height, shape exponent).
/// STAND-IN until the original's Wave Type table is decoded (sdaemon.exe; CHECKLIST.md).
fn waves(e: &WaterEdge) -> Vec<(Vec2, f32, f32, f32, f32)> {
    let d = heading(e.wave_direction);
    let k = |len: f32| std::f32::consts::TAU / len;
    let h = e.wave_intensity;
    match e.wave_type.as_str() {
        "Calm" => vec![],
        "Choppy" => vec![(d, 160.0, k(500.0), 14.0 * h, 1.5), (d.perp(), 120.0, k(300.0), 8.0 * h, 1.5)],
        "Stormy" => vec![(d, 220.0, k(900.0), 30.0 * h, 2.0), (d.perp(), 160.0, k(500.0), 14.0 * h, 1.5)],
        "Rapids" => vec![(d, 300.0, k(400.0), 16.0 * h, 2.0), (d.perp(), 200.0, k(250.0), 8.0 * h, 1.5)],
        _ => vec![(d, 140.0, k(800.0), 10.0 * h, 1.0), (d.perp(), 100.0, k(500.0), 5.0 * h, 1.0)],
    }
}

/// Per-edge constants under `side` ("Leading" / "Trailing").
fn edge_constants(vs: &mut D3d9Block, ps: &mut D3d9Block, sh: &H2WaterShaders, side: &str, e: &WaterEdge) {
    let w = waves(e);
    set(vs, &sh.vs, &format!("g_u{side}_WaveCount"), 0, Vec4::splat(w.len() as f32));
    for (i, (dir, speed, num, height, shape)) in w.iter().enumerate().take(4) {
        set(vs, &sh.vs, &format!("g_av{side}_WaveUnitDirXZ"), i, Vec4::new(dir.x, 0.0, dir.y, 0.0));
        set(vs, &sh.vs, &format!("g_af{side}_WaveSpeed"), i, Vec4::splat(*speed));
        set(vs, &sh.vs, &format!("g_af{side}_WaveWaveNum"), i, Vec4::splat(*num));
        set(vs, &sh.vs, &format!("g_af{side}_WaveWaveHeight"), i, Vec4::splat(*height));
        set(vs, &sh.vs, &format!("g_af{side}_WaveShapeExp"), i, Vec4::splat(*shape));
    }
    // STAND-IN until read from the running game: bump scroll from Bump Direction / Speed, and the
    // flow layers (which the program scrolls down the river over time) as visible as the edge's
    // Flow Speed (Wild America's edges have Bump Speed 0: with no flow the water sat still).
    let bump = heading(e.bump_direction) * e.bump_speed * 100.0;
    set(vs, &sh.vs, &format!("g_v{side}_BumpScrollVelXZ"), 0, Vec4::new(bump.x, 0.0, bump.y, 0.0));
    let flow = (e.flow_speed * phy::H2WATER_FLOW_VISIBILITY).clamp(0.0, 1.0);
    set(vs, &sh.vs, &format!("g_f{side}_FlowUnitVisibility"), 0, Vec4::splat(flow));
    set(ps, &sh.ps, &format!("g_f{side}_FlowUnitVisibility"), 0, Vec4::splat(flow));
    set(ps, &sh.ps, &format!("g_f{side}_BumpMagnitude"), 0, Vec4::splat(1.0));
    set(ps, &sh.ps, &format!("g_f{side}_WaterUnitOpaqueness"), 0, Vec4::splat(e.opaqueness));
    set(ps, &sh.ps, &format!("g_v{side}_WaterColor"), 0, Vec4::from_array(e.water_color));
    set(ps, &sh.ps, &format!("g_v{side}_WhitewashColor"), 0, Vec4::from_array(e.whitewash_color));
    set(ps, &sh.ps, &format!("g_v{side}_SpecularColor"), 0, Vec4::from_array(e.specular_color));
    set(ps, &sh.ps, &format!("g_v{side}_ReflectionTint"), 0, Vec4::from_array(e.reflection_tint));
    let l = e.light.unwrap_or_default();
    let to_light = -Vec3::from_array(l.direction).normalize_or(Vec3::NEG_Y);
    set(ps, &sh.ps, &format!("g_v{side}_UnitDirToLight_WS"), 0, to_light.extend(0.0));
    set(ps, &sh.ps, &format!("g_v{side}_DirLightColor"), 0, Vec4::from_array(l.color) * l.intensity);
    set(ps, &sh.ps, &format!("g_v{side}_AmbDirLightColor"), 0, Vec4::from_array(l.ambient) * l.ambient_intensity);
}

/// Spawn every water sector of `level` with H2Overdrive's water shader.
pub fn spawn(
    commands: &mut Commands,
    level: &H2Level,
    sh: &H2WaterShaders,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<H2WaterMaterial>,
    images: &mut Assets<Image>,
    bump: Handle<Image>,
    cell: f32,
) -> usize {
    let mut pixel = |c: [f32; 4]| {
        let img = Image::new_fill(
            bevy::render::render_resource::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            bevy::render::render_resource::TextureDimension::D2,
            &c.map(|v| (v * 255.0) as u8),
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        images.add(img)
    };
    let _unused = pixel([0.0, 0.0, 0.0, 0.0]);
    let whitewash = spawn_whitewash(commands, images, UVec2::new(960, 540));
    let reflection = spawn_reflection(commands, images, UVec2::new(960, 540));
    commands.insert_resource(H2WaterOn);
    let edges = &level.water_edges;
    let mut count = 0;
    let mut surface = H2Waves::default();
    for s in &level.water_sectors {
        let (a, b) = (&edges[s.leading], &edges[s.trailing]);
        let at = |p: [f32; 3], h: f32| Vec3::new(p[0], h, p[2]);
        // Corners: leading start, trailing start, trailing end (the program's g_avSectorCorner_WS).
        let (c0, c1, c2, c3) = (at(a.start, a.water), at(b.start, b.water), at(b.end, b.water), at(a.end, a.water));
        let centre = (c0 + c1 + c2 + c3) / 4.0;
        let inward = |p: Vec3, q: Vec3| {
            let d = (q - p).xz();
            let n = Vec2::new(-d.y, d.x).normalize_or_zero();
            let mid = (p + q).xz() * 0.5;
            if n.dot(centre.xz() - mid) < 0.0 { -n } else { n }
        };
        // Inward normals: [0] side through c0, [1] trailing edge, [2] side through c2, [3] leading edge.
        let normals = [inward(c0, c1), inward(c1, c2), inward(c2, c3), inward(c3, c0)];

        let mut vs = D3d9Block::default();
        let mut ps = D3d9Block::default();
        for (i, c) in [c0, c1, c2].iter().enumerate() {
            set(&mut vs, &sh.vs, "g_avSectorCorner_WS", i, c.extend(1.0));
        }
        for (i, n) in normals.iter().enumerate() {
            set(&mut vs, &sh.vs, "g_avSectorUnitNormIn_WS", i, Vec4::new(n.x, 0.0, n.y, 0.0));
        }
        set(&mut vs, &sh.vs, "g_MeshOrigin_WS", 0, centre.extend(1.0));

        // Catmull-Rom through the previous, leading, trailing and next edges, each as
        // (start.x, start.z, end.x, end.z): the program evaluates the edge across the river at
        // its distance along the sector, for the flow texture's across coordinate.
        let prev = level.water_sectors.iter().find(|o| o.trailing == s.leading).map(|o| &edges[o.leading]);
        let next = level.water_sectors.iter().find(|o| o.leading == s.trailing).map(|o| &edges[o.trailing]);
        let e4 = |e: &WaterEdge| Vec4::new(e.start[0], e.start[2], e.end[0], e.end[2]);
        let (p1, p2) = (e4(a), e4(b));
        let p0 = prev.map_or(p1 * 2.0 - p2, e4);
        let p3 = next.map_or(p2 * 2.0 - p1, e4);
        set(&mut vs, &sh.vs, "g_avCatmullRomTerm1", 0, p1);
        set(&mut vs, &sh.vs, "g_avCatmullRomTerm2", 0, (p2 - p0) * 0.5);
        set(&mut vs, &sh.vs, "g_avCatmullRomTerm3", 0, (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * 0.5);
        set(&mut vs, &sh.vs, "g_avCatmullRomTerm4", 0, (-p0 + p1 * 3.0 - p2 * 3.0 + p3) * 0.5);
        // STAND-IN: flow coordinates along the sector in units of 1000.
        let len = ((c1 + c2) * 0.5 - (c0 + c3) * 0.5).length();
        set(&mut vs, &sh.vs, "g_fFlowScrollScale", 0, Vec4::splat(len / 1000.0));
        set(&mut vs, &sh.vs, "g_fFlowScrollOffset", 0, Vec4::ZERO);

        edge_constants(&mut vs, &mut ps, sh, "Leading", a);
        edge_constants(&mut vs, &mut ps, sh, "Trailing", b);
        let corners = [c0, c1, c2, c3];
        surface.sectors.push(SectorWaves {
            c0,
            c1,
            c2,
            normals,
            lead: waves(a),
            trail: waves(b),
            lo: corners.iter().fold(Vec2::MAX, |m, c| m.min(c.xz())),
            hi: corners.iter().fold(Vec2::MIN, |m, c| m.max(c.xz())),
        });

        // Waterfalls are drawn by FX_Waterfall (spawn_waterfalls), not as a steep slab of water.
        if is_waterfall(a, b) {
            continue;
        }
        // A grid over the sector's quad; positions are (x, z) from the mesh origin.
        let along = ((c1 - c0).length().max((c2 - c3).length()) / cell).ceil().clamp(1.0, 96.0) as u32;
        let across = ((c3 - c0).length().max((c2 - c1).length()) / cell).ceil().clamp(1.0, 96.0) as u32;
        let mut pos = Vec::new();
        for j in 0..=along {
            let t = j as f32 / along as f32;
            let (l, r) = (c0.lerp(c1, t), c3.lerp(c2, t));
            for i in 0..=across {
                let p = l.lerp(r, i as f32 / across as f32) - centre;
                pos.push([p.x, p.z, 0.0]);
            }
        }
        let mut idx = Vec::new();
        let row = across + 1;
        for j in 0..along {
            for i in 0..across {
                let k = j * row + i;
                idx.extend_from_slice(&[k, k + row, k + 1, k + 1, k + row, k + row + 1]);
            }
        }
        let mesh = Mesh::new(PrimitiveTopology::TriangleList, bevy::asset::RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_indices(Indices::U32(idx));
        let material = materials.add(H2WaterMaterial { vs, ps, bump: bump.clone(), reflection: reflection.clone(), whitewash: whitewash.clone() });
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            bevy::camera::visibility::NoFrustumCulling,
            bevy::camera::visibility::RenderLayers::layer(WATER_LAYER),
            DespawnOnExit(crate::Screen::Race),
            Name::new(format!("h2water {}", count)),
        ));
        count += 1;
    }
    commands.insert_resource(surface);
    count
}

/// The render layer the water is on: the main camera sees it, the reflection camera doesn't.
const WATER_LAYER: usize = 9;

/// The reflection camera (the original's `g_TexReflection` render): the main camera mirrored in
/// the water plane under it, rendering everything but the water into an image the shader reads
/// at the main view's screen position.
#[derive(Component)]
pub struct ReflectionCam;

/// Create the reflection target and camera (`size` pixels; the main view's aspect).
pub fn spawn_reflection(commands: &mut Commands, images: &mut Assets<Image>, size: UVec2) -> Handle<Image> {
    let image = images.add(Image::new_target_texture(
        size.x,
        size.y,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    commands.spawn((
        Camera3d::default(),
        Camera { order: -1, ..default() },
        bevy::camera::RenderTarget::Image(image.clone().into()),
        bevy::camera::visibility::RenderLayers::layer(0),
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        Msaa::Off,
        ReflectionCam,
        DespawnOnExit(crate::Screen::Race),
        Name::new("h2water reflection"),
    ));
    image
}

/// Mirror the main camera in the water under it each frame, and let it see the water layer.
///
/// A mirror flips handedness, which a camera transform can't hold, so the reflection camera is
/// the mirror image with its up axis turned over (a proper rotation; triangles keep their
/// winding) and the shader reads its image with v flipped (`h2_flip_v`).
fn follow_reflection(
    mut commands: Commands,
    track: Option<Res<crate::track::Track>>,
    main: Query<(Entity, &Transform, &Projection, Option<&DistanceFog>, Has<bevy::camera::visibility::RenderLayers>), (With<crate::race::ChaseCam>, Without<ReflectionCam>)>,
    mut refl: Query<(Entity, &mut Transform, &mut Projection), (With<ReflectionCam>, Without<crate::race::ChaseCam>, Without<WhitewashCam>)>,
    mut white: Query<(&mut Transform, &mut Projection), (With<WhitewashCam>, Without<crate::race::ChaseCam>, Without<ReflectionCam>)>,
) {
    let Ok((cam_e, cam, proj, fog, layered)) = main.single() else { return };
    if let Ok((mut t, mut p)) = white.single_mut() {
        *t = *cam;
        *p = proj.clone();
    }
    let Ok((refl_e, mut t, mut p)) = refl.single_mut() else { return };
    if !layered {
        commands.entity(cam_e).insert(bevy::camera::visibility::RenderLayers::from_layers(&[0, WATER_LAYER]));
    }
    if let Some(fog) = fog {
        commands.entity(refl_e).insert(fog.clone());
    }
    let h = track.map_or(0.0, |tr| tr.locate_anywhere(cam.translation).water);
    let mirror = |v: Vec3| Vec3::new(v.x, -v.y, v.z);
    let (x, y, z) = (cam.rotation * Vec3::X, cam.rotation * Vec3::Y, cam.rotation * Vec3::Z);
    t.translation = Vec3::new(cam.translation.x, 2.0 * h - cam.translation.y, cam.translation.z);
    t.rotation = Quat::from_mat3(&Mat3::from_cols(mirror(x), -mirror(y), mirror(z))).normalize();
    *p = proj.clone();
    // Oblique near plane on the water (Lengyel; Bevy's near_clip_plane): nothing below the surface
    // (riverbed, the bases of rocks) is mirrored up into the reflection. A plane transforms into
    // view space by the transpose of world-from-view.
    if let Projection::Perspective(pp) = &mut *p {
        let plane = Vec4::new(0.0, 1.0, 0.0, -(h + phy::H2WATER_CLIP_LIFT));
        pp.near_clip_plane = t.to_matrix().transpose() * plane;
    }
}

/// The render layer foam wakes go on under this water: only the whitewash camera sees them.
pub const WHITEWASH_LAYER: usize = 10;

/// Present while a race uses this water (effects put foam in the whitewash buffer).
#[derive(Resource)]
pub struct H2WaterOn;

/// The whitewash camera (the original's FX_WaterWhite pass into `g_TexWhitewash`): the main
/// view of the foam wakes alone over black; the water shader reads it at its screen position and
/// mixes toward the edge's whitewash colour, lit like the water.
#[derive(Component)]
pub struct WhitewashCam;

pub fn spawn_whitewash(commands: &mut Commands, images: &mut Assets<Image>, size: UVec2) -> Handle<Image> {
    let image = images.add(Image::new_target_texture(size.x, size.y, bevy::render::render_resource::TextureFormat::Rgba8Unorm, None));
    commands.spawn((
        Camera3d::default(),
        Camera { order: -2, clear_color: ClearColorConfig::Custom(Color::BLACK), ..default() },
        bevy::camera::RenderTarget::Image(image.clone().into()),
        bevy::camera::visibility::RenderLayers::layer(WHITEWASH_LAYER),
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        Msaa::Off,
        WhitewashCam,
        DespawnOnExit(crate::Screen::Race),
        Name::new("h2water whitewash"),
    ));
    image
}

/// One wave: unit direction XZ, speed, angular wave number, height, shape exponent.
type Wave = (Vec2, f32, f32, f32, f32);

struct SectorWaves {
    c0: Vec3,
    c1: Vec3,
    c2: Vec3,
    normals: [Vec2; 4],
    lead: Vec<Wave>,
    trail: Vec<Wave>,
    lo: Vec2,
    hi: Vec2,
}

/// The water surface the shader draws, on the CPU (FX_Water2's vertex program: base height
/// blended leading to trailing edge, plus each edge's waves blended the same way), so boats sit
/// on the surface that is drawn.
#[derive(Resource, Default)]
pub struct H2Waves {
    sectors: Vec<SectorWaves>,
}

impl H2Waves {
    /// Still-water height of the sector under `p` (x, z): where sectors stack (a chute beside its
    /// side pools, a river under a bridge) the one nearest `near` in height. Physics rides this, so a
    /// chute slopes down while the pools beside it stay at their own level.
    pub fn surface(&self, p: Vec2, near: f32) -> Option<f32> {
        let mut best: Option<f32> = None;
        for s in &self.sectors {
            if p.x < s.lo.x || p.y < s.lo.y || p.x > s.hi.x || p.y > s.hi.y {
                continue;
            }
            let (a, c) = (s.c0.xz(), s.c2.xz());
            let (d0, d1, d2, d3) = ((p - a).dot(s.normals[0]), (p - c).dot(s.normals[1]), (p - c).dot(s.normals[2]), (p - a).dot(s.normals[3]));
            if d0 < -1.0 || d1 < -1.0 || d2 < -1.0 || d3 < -1.0 {
                continue;
            }
            let f = (d3 / (d3 + d1).max(1e-7)).clamp(0.0, 1.0);
            let h = s.c0.y + (s.c1.y - s.c0.y) * f;
            if best.is_none_or(|b| (h - near).abs() < (b - near).abs()) {
                best = Some(h);
            }
        }
        best
    }

    /// Surface height at `p` (x, z) at shader time `t`, if `p` is in a water sector.
    pub fn height(&self, p: Vec2, t: f32) -> Option<f32> {
        let sum = |ws: &[Wave]| -> f32 {
            ws.iter()
                .map(|(dir, speed, num, height, shape)| {
                    let s = (num * (speed * t - dir.dot(p))).sin() * 0.5 + 0.5;
                    height * s.max(0.0).powf(*shape)
                })
                .sum()
        };
        for s in &self.sectors {
            if p.x < s.lo.x || p.y < s.lo.y || p.x > s.hi.x || p.y > s.hi.y {
                continue;
            }
            let (a, c) = (s.c0.xz(), s.c2.xz());
            let (d0, d1, d2, d3) = ((p - a).dot(s.normals[0]), (p - c).dot(s.normals[1]), (p - c).dot(s.normals[2]), (p - a).dot(s.normals[3]));
            if d0 < -1.0 || d1 < -1.0 || d2 < -1.0 || d3 < -1.0 {
                continue;
            }
            let f = (d3 / (d3 + d1).max(1e-7)).clamp(0.0, 1.0);
            let waves = sum(&s.lead) + (sum(&s.trail) - sum(&s.lead)) * f;
            return Some(s.c0.y + (s.c1.y - s.c0.y) * f + waves);
        }
        None
    }
}

/// Water edges and sectors for a course with none of its own (Hydro Thunder): one edge per river
/// cross-section (shared between neighbouring sectors, so the flow spline joins up) and one
/// sector per river sector, with the look of an ordinary H2Overdrive river edge. The water colour
/// comes from the course's own water art later (`tint_from`).
pub fn edges_from_river(level: &mut H2Level, river: &[([f32; 3], [f32; 3], f32, [f32; 3], [f32; 3], f32)]) {
    use riptide_assets::h2level::{WaterLight, WaterSector};
    let mut index: Vec<(([i32; 3], [i32; 3], i32), usize)> = Vec::new();
    let key = |s: [f32; 3], e: [f32; 3], w: f32| (s.map(|v| v.round() as i32), e.map(|v| v.round() as i32), w.round() as i32);
    let mut edge = |level: &mut H2Level, s: [f32; 3], e: [f32; 3], w: f32| -> usize {
        let k = key(s, e, w);
        if let Some((_, i)) = index.iter().find(|(kk, _)| *kk == k) {
            return *i;
        }
        let i = level.water_edges.len();
        level.water_edges.push(WaterEdge {
            name: format!("river{i}"),
            start: s,
            end: e,
            water: w,
            wave_type: "Normal".into(),
            wave_intensity: phy::HT_WATER_WAVE_INTENSITY,
            wave_direction: 0.0,
            bump_direction: 0.0,
            bump_speed: 0.0,
            flow_speed: phy::HT_WATER_FLOW_SPEED,
            opaqueness: phy::HT_WATER_OPAQUENESS,
            reflection_type: "Real".into(),
            water_color: [0.05, 0.2, 0.25, 1.0],
            whitewash_color: [0.8, 0.8, 0.8, 0.0],
            specular_color: [0.65, 0.65, 0.65, 1.0],
            reflection_tint: [phy::HT_WATER_REFLECTION, phy::HT_WATER_REFLECTION, phy::HT_WATER_REFLECTION, 1.0],
            light: Some(WaterLight { direction: [-0.5, -0.7, 0.3], color: [1.0, 0.95, 0.85, 1.0], intensity: 1.0, ambient: [0.75, 0.8, 0.85, 1.0], ambient_intensity: 0.4 }),
            waterfall: riptide_assets::h2level::WaterfallFields { disable: false, width: 1.0, speed: 0.5, flare: 0.2, curve: 0.2, light_scale: 1.3, color: [1.0; 4] },
        });
        index.push((k, i));
        i
    };
    for &(a0, a1, aw, b0, b1, bw) in river {
        let leading = edge(level, a0, a1, aw);
        let trailing = edge(level, b0, b1, bw);
        if leading != trailing {
            level.water_sectors.push(WaterSector { leading, trailing });
        }
    }
}

/// Give generated edges (`edges_from_river`) the average colour of the course's water art.
pub fn tint_from(level: &mut H2Level, image: &Image) {
    let Some(data) = image.data.as_ref() else { return };
    if data.len() < 4 {
        return;
    }
    let mut sum = [0f64; 3];
    for px in data.chunks_exact(4) {
        for c in 0..3 {
            sum[c] += px[c] as f64;
        }
    }
    let n = (data.len() / 4) as f64 * 255.0;
    // The art is sRGB; the shader works in linear.
    let lin = |v: f64| ((v / n) as f32).powf(2.2);
    let color = [lin(sum[0]), lin(sum[1]), lin(sum[2]), 1.0];
    for e in level.water_edges.iter_mut().filter(|e| e.name.starts_with("river")) {
        e.water_color = color;
    }
}

// ---- Waterfalls: shad4.FX_Waterfall ----------------------------------------------------------

const FALL_VERTEX: Handle<Shader> = uuid_handle!("6f0b8c52-3a51-4b0e-9a8e-2f1d6c0a9e31");
const FALL_FRAGMENT: Handle<Shader> = uuid_handle!("6f0b8c52-3a51-4b0e-9a8e-2f1d6c0a9e32");

/// The original's falling sheet of water where a sector drops steeply: a grid launched from the
/// top edge that falls under gravity (its vertex program), textured by scrolling layers of the
/// waterfall art faded by a ramp (its pixel program).
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct WaterfallMaterial {
    #[uniform(0)]
    pub vs: D3d9Block,
    #[uniform(1)]
    pub ps: D3d9Block,
    #[texture(2)]
    #[sampler(3)]
    pub art: Handle<Image>,
    #[texture(4)]
    #[sampler(5)]
    pub ramp: Handle<Image>,
}

impl Material for WaterfallMaterial {
    fn vertex_shader() -> ShaderRef {
        FALL_VERTEX.into()
    }
    fn fragment_shader() -> ShaderRef {
        FALL_FRAGMENT.into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // One input: texcoord0 = (column, row) of the grid.
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?];
        descriptor.vertex.entry_point = Some("vertex".into());
        if let Some(f) = descriptor.fragment.as_mut() {
            f.entry_point = Some("fragment".into());
        }
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// Translate `shad4.FX_Waterfall`'s fullest vertex / pixel pair once per run.
fn install_waterfall(shaders: &mut Assets<Shader>, blob: &[u8]) -> Option<H2WaterShaders> {
    static DONE: std::sync::OnceLock<H2WaterShaders> = std::sync::OnceLock::new();
    if let Some(sh) = DONE.get() {
        if shaders.contains(&FALL_VERTEX) {
            return Some(sh.clone());
        }
    }
    let progs = d3d9_shader::programs(blob);
    let vs = progs.iter().filter(|p| !p.pixel).max_by_key(|p| p.tokens.len())?.clone();
    let ps = progs.iter().filter(|p| p.pixel).max_by_key(|p| p.tokens.len())?.clone();
    let mut o = wgsl::Options { group: "#{MATERIAL_BIND_GROUP}".into(), constants_binding: 0, first_texture: 2, entry: "vertex".into(), ..default() };
    o.prelude = "#import bevy_pbr::mesh_view_bindings::{view, globals}".into();
    let reg = |p: &Program, name: &str| p.constants.iter().find(|c| c.name == name && c.set == 2).map(|c| c.index as u32);
    if let Some(r) = reg(&vs, "g_MtxViewProj") {
        for k in 0..4 {
            o.constants.insert(r + k, format!("vec4<f32>(view.clip_from_world[0][{k}], view.clip_from_world[1][{k}], view.clip_from_world[2][{k}], view.clip_from_world[3][{k}])"));
        }
    }
    if let Some(r) = reg(&vs, "g_CamPos_WS") {
        o.constants.insert(r, "vec4<f32>(view.world_position, 1.0)".into());
    }
    if let Some(r) = reg(&vs, "g_fElapsedSecs") {
        o.constants.insert(r, "vec4<f32>(globals.time)".into());
    }
    let vs_src = wgsl::translate(&vs, &o);
    let o = wgsl::Options { group: "#{MATERIAL_BIND_GROUP}".into(), constants_binding: 1, first_texture: 2, entry: "fragment".into(), ..default() };
    let ps_src = wgsl::translate(&ps, &o);
    if std::env::var_os("RIPTIDE_H2WATER_DUMP").is_some() {
        let _ = std::fs::write("waterfall_vs.wgsl", &vs_src);
        let _ = std::fs::write("waterfall_ps.wgsl", &ps_src);
    }
    let _ = shaders.insert(&FALL_VERTEX, Shader::from_wgsl(vs_src, "riptide://waterfall_vs.wgsl"));
    let _ = shaders.insert(&FALL_FRAGMENT, Shader::from_wgsl(ps_src, "riptide://waterfall_ps.wgsl"));
    let sh = H2WaterShaders { vs, ps };
    let _ = DONE.set(sh.clone());
    Some(sh)
}

/// Is the sector from `a` to `b` a waterfall (steep drop)?
pub fn is_waterfall(a: &WaterEdge, b: &WaterEdge) -> bool {
    let drop = a.water - b.water;
    let mid = |e: &WaterEdge| Vec2::new((e.start[0] + e.end[0]) * 0.5, (e.start[2] + e.end[2]) * 0.5);
    let run = mid(a).distance(mid(b)).max(1.0);
    !a.waterfall.disable && drop > phy::WATERFALL_MIN_DROP && drop > run * phy::WATERFALL_MIN_STEEPNESS
}

/// A fade ramp for the pixel program's second sampler: opaque in the middle of the sheet,
/// fading out at its sides and lightly towards the bottom.
fn waterfall_ramp(images: &mut Assets<Image>) -> Handle<Image> {
    let n = 64u32;
    let mut data = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let u = (x as f32 + 0.5) / n as f32;
            let v = (y as f32 + 0.5) / n as f32;
            let side = ((u * std::f32::consts::PI).sin() * 1.6).min(1.0);
            let a = (side * (1.0 - 0.35 * v)).clamp(0.0, 1.0);
            data.extend_from_slice(&[255, 255, 255, (a * 255.0) as u8]);
        }
    }
    let mut img = Image::new(
        bevy::render::render_resource::Extent3d { width: n, height: n, depth_or_array_layers: 1 },
        bevy::render::render_resource::TextureDimension::D2,
        data,
        bevy::render::render_resource::TextureFormat::Rgba8Unorm,
        bevy::asset::RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = bevy::image::ImageSampler::Descriptor(crate::content::repeat_sampler());
    images.add(img)
}

/// Spawn the waterfalls of `level` (steep sectors): returns how many.
pub fn spawn_waterfalls(
    commands: &mut Commands,
    level: &H2Level,
    shaders: &mut Assets<Shader>,
    blob: &[u8],
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<WaterfallMaterial>,
    images: &mut Assets<Image>,
    art: Handle<Image>,
) -> usize {
    let Some(sh) = install_waterfall(shaders, blob) else { return 0 };
    let ramp = waterfall_ramp(images);
    let (nx, ny) = (phy::WATERFALL_COLUMNS.max(2.0) as u32, phy::WATERFALL_ROWS.max(2.0) as u32);
    let mut pos = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            pos.push([i as f32, j as f32, 0.0]);
        }
    }
    let mut idx = Vec::new();
    for j in 0..ny - 1 {
        for i in 0..nx - 1 {
            let k = j * nx + i;
            idx.extend_from_slice(&[k, k + nx, k + 1, k + 1, k + nx, k + nx + 1]);
        }
    }
    let mesh = meshes.add(
        Mesh::new(PrimitiveTopology::TriangleList, bevy::asset::RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
            .with_inserted_indices(Indices::U32(idx)),
    );
    let edges = &level.water_edges;
    let mut count = 0;
    for s in &level.water_sectors {
        let (a, b) = (&edges[s.leading], &edges[s.trailing]);
        if !is_waterfall(a, b) {
            continue;
        }
        let mid = |e: &WaterEdge| Vec3::new((e.start[0] + e.end[0]) * 0.5, e.water, (e.start[2] + e.end[2]) * 0.5);
        let width = |e: &WaterEdge| Vec2::new(e.end[0] - e.start[0], e.end[2] - e.start[2]).length();
        let (top, bottom) = (mid(a), mid(b));
        let dir = (bottom - top).xz().normalize_or(Vec2::Y);
        let run = (bottom - top).xz().length();
        let drop = top.y - bottom.y;
        // Launched flat, falling under gravity: the fall time lands the sheet on the bottom edge.
        let g = phy::WATERFALL_GRAVITY.max(1.0);
        let t = (2.0 * drop / g).sqrt().max(0.05);
        let mut vs = D3d9Block::default();
        let mut ps = D3d9Block::default();
        let w = |vs: &mut D3d9Block, n: &str, v: Vec4| set(vs, &sh.vs, n, 0, v);
        w(&mut vs, "g_vTopPos", top.extend(1.0));
        w(&mut vs, "g_vUnitDirXZ", Vec4::new(dir.x, 0.0, dir.y, 0.0));
        w(&mut vs, "g_fTopWidth", Vec4::splat(width(a) * a.waterfall.width));
        w(&mut vs, "g_fBottomWidthMult", Vec4::splat((width(b) / width(a).max(1.0)).clamp(0.25, 4.0)));
        w(&mut vs, "g_fTopSpeed", Vec4::splat(run / t));
        w(&mut vs, "g_fTopPitchSin", Vec4::splat(0.0));
        w(&mut vs, "g_fTopPitchCos", Vec4::splat(1.0));
        w(&mut vs, "g_fGravity", Vec4::splat(-g));
        w(&mut vs, "g_fDescentSecs", Vec4::splat(t));
        w(&mut vs, "g_fDeltaSecsBetweenVtx", Vec4::splat(t / (ny - 1) as f32));
        w(&mut vs, "g_fVtxInvCountX", Vec4::splat(1.0 / (nx - 1) as f32));
        w(&mut vs, "g_fVtxInvCountY", Vec4::splat(1.0 / (ny - 1) as f32));
        w(&mut vs, "g_fFlowSpeedMult", Vec4::splat(a.waterfall.speed));
        // White Bias is CSWaterfall's own property (a brightness multiplier on the light: the program
        // clamps light x bias to 0..1), not the edge's Waterfall Flare.
        w(&mut vs, "g_fWhiteBias", Vec4::splat(phy::WATERFALL_WHITE_BIAS));
        let l = a.light.unwrap_or_default();
        let to_light = -Vec3::from_array(l.direction).normalize_or(Vec3::NEG_Y);
        w(&mut vs, "g_DirLight_uCount", Vec4::splat(1.0));
        w(&mut vs, "g_DirLight_avUnitDir", to_light.extend(0.0));
        w(&mut vs, "g_DirLight_avColor", Vec4::from_array(l.color) * l.intensity * a.waterfall.light_scale);
        w(&mut vs, "g_AmbLight_vColor", Vec4::from_array(l.ambient) * l.ambient_intensity.max(0.3) * a.waterfall.light_scale);
        set(&mut ps, &sh.ps, "g_vColorTint", 0, Vec4::from_array(a.waterfall.color));
        let material = materials.add(WaterfallMaterial { vs, ps, art: art.clone(), ramp: ramp.clone() });
        commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            bevy::camera::visibility::NoFrustumCulling,
            DespawnOnExit(crate::Screen::Race),
            Name::new(format!("h2 waterfall {count}")),
        ));
        count += 1;
    }
    count
}

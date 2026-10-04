// Riptide water: Bevy's standard PBR lighting with
// - waves moved on the GPU (the same three travelling sines the boats bob on, race.rs);
// - three scrolling layers of the level's water normal map for ripples (the finest keeps
//   distant water from smearing);
// - fresnel (clearer looking down, glossy and opaque at grazing angles) and crest foam;
// - with the camera's depth prepass: how thick the water is in front of what lies behind it,
//   for foam along banks and rocks, clear shallows and darker deep water.

#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput, FragmentOutput},
    view_transformations::{position_world_to_clip, depth_ndc_to_view_z},
    prepass_utils,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}

struct WaterParams {
    time: f32,
    amplitude: f32,
    wave_k: f32,
    wave_speed: f32,
    normal_scale: f32,
    normal_strength: f32,
    foam: f32,
    gloss: f32,
    detail_scale: f32,
    shore_width: f32,
    depth_fade: f32,
    deep_darken: f32,
    shallow_alpha: f32,
    sky_reflect: f32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var ripple_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var ripple_sampler: sampler;

// Height and slope (d/dx, d/dz) of the waves at world (x, z).
fn wave(p: vec2<f32>) -> vec3<f32> {
    let k = water.wave_k;
    let t = water.time * water.wave_speed;
    let x = p.x * k;
    let z = p.y * k;
    let a = water.amplitude;
    let h = a * (sin(x + t) + 0.6 * sin(z * 0.8 - t * 1.3) + 0.3 * sin((x + z) * 0.6 + t * 0.7));
    let dx = a * k * (cos(x + t) + 0.3 * 0.6 * cos((x + z) * 0.6 + t * 0.7));
    let dz = a * k * (0.6 * 0.8 * cos(z * 0.8 - t * 1.3) + 0.3 * 0.6 * cos((x + z) * 0.6 + t * 0.7));
    return vec3<f32>(h, dx, dz);
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    var world = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    let w = wave(world.xz);
    world.y += w.x;
    out.world_position = world;
    out.position = position_world_to_clip(world.xyz);
    out.world_normal = normalize(vec3<f32>(-w.y, 1.0, -w.z));
#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(world_from_local, vertex.tangent, vertex.instance_index);
#endif
#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
    return out;
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
#ifdef MULTISAMPLED
    @builtin(sample_index) sample_index: u32,
#endif
) -> FragmentOutput {
#ifndef MULTISAMPLED
    let sample_index = 0u;
#endif
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Ripples: three layers of the normal map drifting across each other.
    let p = in.world_position.xz * water.normal_scale;
    let t = water.time;
    let n1 = textureSample(ripple_texture, ripple_sampler, p + vec2<f32>(0.021, 0.013) * t).xyz * 2.0 - 1.0;
    let n2 = textureSample(ripple_texture, ripple_sampler, p * 1.73 + vec2<f32>(-0.017, 0.026) * t).xyz * 2.0 - 1.0;
    let n3 = textureSample(ripple_texture, ripple_sampler, in.world_position.xz * water.detail_scale + vec2<f32>(0.031, -0.022) * t).xyz * 2.0 - 1.0;
    let ripple = vec2<f32>(n1.x + n2.x + 0.6 * n3.x, n1.y + n2.y + 0.6 * n3.y) * water.normal_strength;
    let n = normalize(in.world_normal + vec3<f32>(ripple.x, 0.0, ripple.y));
    pbr_input.N = n;
    pbr_input.world_normal = n;

    // Fresnel: looking straight down you see into the water; at grazing angles it turns into a
    // glossy, opaque mirror of the sky.
    let ndv = clamp(dot(n, pbr_input.V), 0.0, 1.0);
    let fres = pow(1.0 - ndv, 5.0);
    pbr_input.material.perceptual_roughness = mix(pbr_input.material.perceptual_roughness, water.gloss, fres);
    // Less of the (generated, pale) sky: it washed the water out.
    pbr_input.specular_occlusion = water.sky_reflect;
    var color = pbr_input.material.base_color;

    // How much water lies in front of the riverbed or bank behind this pixel (view-space depth
    // difference): clear and light in the shallows, darker when deep, foam along the edges.
    var thick = water.depth_fade;
#ifdef DEPTH_PREPASS
    let behind = depth_ndc_to_view_z(prepass_utils::prepass_depth(in.position, sample_index));
    let here = depth_ndc_to_view_z(in.position.z);
    thick = max(here - behind, 0.0);
#endif
    let deep = smoothstep(0.0, water.depth_fade, thick);
    color = vec4<f32>(color.rgb * mix(1.0, water.deep_darken, deep), mix(water.shallow_alpha, color.a, deep));
    color.a = mix(color.a, 1.0, fres);
    // Shore foam, broken up by the ripple texture so it isn't a flat band.
    let edge = 1.0 - smoothstep(0.0, water.shore_width, thick);
    let lace = smoothstep(0.35, 0.75, 0.5 + 0.25 * (n1.x + n3.y) + 0.5 * edge);
    let shore = edge * lace;
    color = vec4<f32>(mix(color.rgb, vec3<f32>(0.93, 0.97, 1.0), shore), max(color.a, shore * 0.95));

    // Foam on the crests.
    let w = wave(in.world_position.xz);
    let crest = smoothstep(water.amplitude * 1.1, water.amplitude * 1.75, w.x) * water.foam;
    color = vec4<f32>(mix(color.rgb, vec3<f32>(0.92, 0.96, 1.0), crest), max(color.a, crest));
    pbr_input.material.base_color = alpha_discard(pbr_input.material, color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}

// Vertex side of H2Overdrive's boat shader (crate::boatshader): Bevy's mesh transform, handing
// the translated FX_BoatLocal pixel program the varyings its own vertex program produced, at the
// locations d3d9-shader gives them (texcoordN at N, normal 10, tangent 12, binormal 13).
#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_world, mesh_normal_local_to_world, mesh_tangent_local_to_world}
#import bevy_pbr::view_transformations::position_world_to_clip

struct VsIn {
    @builtin(instance_index) instance: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec4<f32>,
    @location(10) normal: vec4<f32>,
    @location(13) binormal: vec4<f32>,
    @location(12) tangent: vec4<f32>,
    @location(1) uv01: vec4<f32>,
    @location(2) uv23: vec4<f32>,
    @location(3) uv4: vec4<f32>,
};

@vertex
fn vertex(v: VsIn) -> VsOut {
    let world_from_local = get_world_from_local(v.instance);
    let world = mesh_position_local_to_world(world_from_local, vec4<f32>(v.position, 1.0));
    let n = normalize(mesh_normal_local_to_world(v.normal, v.instance));
    let t4 = mesh_tangent_local_to_world(world_from_local, v.tangent, v.instance);
    let t = normalize(t4.xyz);
    let b = cross(n, t) * t4.w;
    var out: VsOut;
    out.clip = position_world_to_clip(world.xyz);
    out.world = vec4<f32>(world.xyz, 1.0);
    out.normal = vec4<f32>(n, 0.0);
    out.binormal = vec4<f32>(b, 0.0);
    out.tangent = vec4<f32>(t, 0.0);
    // One UV set: every map reads it. texcoord3.z is the original's baked-light scale
    // (1 - vertex alpha x g_vVertexColor.w: 1 for boats), .w its reflection blend (vertex red: 0).
    out.uv01 = vec4<f32>(v.uv, v.uv);
    out.uv23 = vec4<f32>(v.uv, v.uv);
    out.uv4 = vec4<f32>(v.uv, 1.0, 0.0);
    return out;
}

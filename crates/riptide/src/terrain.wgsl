// H2Overdrive's two-texture vertex-lit terrain (shad4.OP_2V*: OP_2VP / 2VA / 2VN): the second texture, on the
// second UV set, laid over the first by its own alpha x the vertex alpha:
//     colour = lerp(texture0(uv0), texture1(uv1), texture1.a x vertex.a)
// (the original pixel program's `lrp`). Lighting stays Bevy's (sun, ambient, fog), like the rest
// of Riptide's world. Vertex colour is white with the original's alpha, which is the blend.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var second_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var second_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> terrain: vec4<f32>;

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    // The surface textures tile on the first UV set; the mesh's second set is a 0..1 atlas (mostly
    // zero on these parts), where both textures came out as a transparent black corner.
    let uv1 = in.uv;
    if terrain.x > 0.5 {
        // Lightmap (OP_*L*): the baked light on the second UV set (a 0..1 atlas) adds to the lit
        // surface, as the original pixel program adds lightmap x emissive to its light.
#ifdef VERTEX_UVS_B
        let lm = textureSample(second_texture, second_sampler, in.uv_b).rgb;
#else
        let lm = vec3<f32>(0.0);
#endif
        pbr_input.material.emissive = vec4<f32>(pbr_input.material.base_color.rgb * lm * terrain.y, 1.0);
        var lit: FragmentOutput;
        lit.color = apply_pbr_lighting(pbr_input);
        lit.color = main_pass_post_lighting_processing(pbr_input, lit.color);
        return lit;
    }
    let second = textureSample(second_texture, second_sampler, uv1);
#ifdef VERTEX_COLORS
    let blend = clamp(second.a * in.color.a, 0.0, 1.0);
#else
    let blend = second.a;
#endif
    let base = pbr_input.material.base_color;
    pbr_input.material.base_color = vec4<f32>(mix(base.rgb, second.rgb, blend), 1.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}

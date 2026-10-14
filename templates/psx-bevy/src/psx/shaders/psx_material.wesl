// PlayStation-style mesh shading for `PsxMaterial`.
//
// The look comes from how the PS1 computed things, not from its limits:
// - vertices snap to whole pixels of the low-resolution target (wobble),
// - UVs and colours interpolate linearly in screen space (affine warping),
// - lighting is per vertex (Gouraud) and works on gamma-encoded colours where
//   1.0 is neutral and up to 2.0 brightens a texture (the GPU's 128 = 1.0),
// - output is 15-bit colour with the GPU's 4x4 ordered dither.

import bevy_pbr::render::{
    mesh_functions,
    forward_io::{Vertex, decompress_vertex},
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};
// Bevy only binds fog for cameras with `DistanceFog`.
@if(DISTANCE_FOG)
import bevy_pbr::render::mesh_view_bindings::fog;

const MAX_LIGHTS: u32 = 8u;

struct PsxUniform {
    color: vec4<f32>,
    ambient: vec4<f32>,
    light_dirs: array<vec4<f32>, MAX_LIGHTS>,
    light_colors: array<vec4<f32>, MAX_LIGHTS>,
    light_count: u32,
    flags: u32,
};

const FLAG_TEXTURED: u32 = 1u;
const FLAG_LIT: u32 = 2u;
const FLAG_FOG: u32 = 4u;
const FLAG_DITHER: u32 = 8u;
const FLAG_SNAP: u32 = 16u;
const FLAG_CUTOUT: u32 = 32u;
const FLAG_BLEND_HALF: u32 = 64u;
const FLAG_BLEND_QUARTER: u32 = 128u;

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> material: PsxUniform;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var color_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var color_sampler: sampler;

struct PsxVarying {
    @builtin(position) position: vec4<f32>,
    // `linear` = screen-space (non perspective-correct) interpolation.
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(linear) color: vec3<f32>,
    // Distance from the camera, interpolated per vertex like the GTE's depth cue.
    @location(2) @interpolate(linear) distance: f32,
};

fn has(flag: u32) -> bool {
    return (material.flags & flag) != 0u;
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3(0.0)) + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

@vertex
fn vertex(vertex_in: Vertex) -> PsxVarying {
    let v = decompress_vertex(vertex_in, vertex_in.instance_index);
    let world_from_local = mesh_functions::get_world_from_local(vertex_in.instance_index);
    let world = mesh_functions::mesh_position_local_to_world(world_from_local, vec4(v.position, 1.0));

    var clip = position_world_to_clip(world.xyz);
    if has(FLAG_SNAP) && clip.w > 0.0 {
        let size = view.viewport.zw;
        let pixel = round((clip.xy / clip.w * 0.5 + 0.5) * size);
        clip = vec4((pixel / size * 2.0 - 1.0) * clip.w, clip.z, clip.w);
    }

    var out: PsxVarying;
    out.position = clip;
    out.uv = vec2(0.0);
    @if(VERTEX_UVS_A)
    out.uv = v.uv;

    var color = material.color.rgb;
    @if(VERTEX_COLORS)
    color = color * linear_to_srgb(v.color.rgb);

    var light = vec3(1.0);
    @if(VERTEX_NORMALS)
    if has(FLAG_LIT) {
        let n = normalize(mesh_functions::mesh_normal_local_to_world(v.normal, vertex_in.instance_index));
        // Lights live in the material uniform: Bevy's light buffer is fragment-only.
        light = material.ambient.rgb;
        for (var i = 0u; i < min(material.light_count, MAX_LIGHTS); i++) {
            light += material.light_colors[i].rgb * max(dot(n, material.light_dirs[i].xyz), 0.0);
        }
    }
    out.color = color * light;

    out.distance = length(world.xyz - view.world_position);
    return out;
}

@fragment
fn fragment(in: PsxVarying) -> @location(0) vec4<f32> {
    var rgb = in.color;
    if has(FLAG_TEXTURED) {
        let texel = textureSample(color_texture, color_sampler, in.uv);
        if has(FLAG_CUTOUT) && texel.a < 0.5 {
            discard;
        }
        rgb = linear_to_srgb(texel.rgb) * min(rgb, vec3(2.0));
    } else {
        rgb = min(rgb, vec3(1.0));
    }
    @if(DISTANCE_FOG)
    if has(FLAG_FOG) {
        var f = 0.0;
        if fog.mode == 1u {
            f = saturate((in.distance - fog.be.x) / max(fog.be.y - fog.be.x, 0.0001));
        } else if fog.mode == 2u {
            f = 1.0 - exp(-fog.be.x * in.distance);
        } else if fog.mode == 3u {
            f = 1.0 - exp(-pow(fog.be.x * in.distance, 2.0));
        }
        rgb = mix(rgb, linear_to_srgb(fog.base_color.rgb), f);
    }

    // 15-bit colour at 8-bit precision, dithered like the GPU.
    var c8 = rgb * 255.0;
    if has(FLAG_DITHER) {
        var matrix = array<f32, 16>(-4.0, 0.0, -3.0, 1.0, 2.0, -2.0, 3.0, -1.0, -3.0, 1.0, -4.0, 0.0, 3.0, -1.0, 2.0, -2.0);
        let p = vec2<u32>(in.position.xy) % vec2(4u);
        c8 += vec3(matrix[p.y * 4u + p.x]);
    }
    let q = floor(clamp(c8, vec3(0.0), vec3(255.0)) / 8.0);
    rgb = (q * 8.0 + floor(q / 4.0)) / 255.0;

    var alpha = 1.0;
    if has(FLAG_BLEND_HALF) {
        alpha = 0.5;
    }
    if has(FLAG_BLEND_QUARTER) {
        rgb *= 0.25;
    }
    return vec4(srgb_to_linear(rgb), alpha);
}

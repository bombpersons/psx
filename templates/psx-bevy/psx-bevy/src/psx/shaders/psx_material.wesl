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
    view_transformations::{position_world_to_clip, position_world_to_view},
};
// Bevy only binds fog for cameras with `DistanceFog`.
@if(DISTANCE_FOG)
import bevy_pbr::render::mesh_view_bindings::fog;

const MAX_LIGHTS: u32 = 8u;

struct PsxUniform {
    color: vec4<f32>,
    ambient: vec4<f32>,
    depth_cue: vec4<f32>,
    far_color: vec4<f32>,
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
const FLAG_STP_TEXEL: u32 = 64u;
const FLAG_OPAQUE_ONLY: u32 = 128u;
const FLAG_DEPTH_CUE: u32 = 512u;
const BLEND_SHIFT: u32 = 12u;
const BLEND_HALF: u32 = 1u;
const BLEND_ADD: u32 = 2u;
const BLEND_SUBTRACT: u32 = 3u;
const BLEND_QUARTER: u32 = 4u;
// Texel alpha classes written by `psx textures`: 0 transparent, ~0.5 STP, 1 opaque.
const ALPHA_TRANSPARENT: f32 = 0.25;
const ALPHA_STP: f32 = 0.75;

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> material: PsxUniform;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var color_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var color_sampler: sampler;

// Affine (screen-space linear) interpolation, like the PS1 rasteriser. Each
// attribute A is sent as A·w with perspective-correct interpolation and divided
// by the interpolated w in the fragment shader, which gives exactly the
// screen-linear A. This avoids `@interpolate(linear)`, whose values some
// drivers (e.g. Mesa llvmpipe) extrapolate wrongly on triangles that cross the
// viewport edge.
// A vertex behind the camera has w <= 0, and on the visible part of its
// triangle A·w over w is no longer an average of the vertices' values: near
// the camera it runs away (full fog, wild colours and UVs). So the weight is kept
// positive (`affine_weight`); it equals w for every vertex in front of the
// near plane, so unclipped triangles are unchanged.
struct PsxVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv_w: vec2<f32>,
    @location(1) color_w: vec3<f32>,
    // World position, perspective-correct: DistanceFog per pixel.
    @location(2) world: vec3<f32>,
    @location(3) w: f32,
};

const MIN_AFFINE_WEIGHT: f32 = 1e-4;
fn affine_weight(clip_w: f32) -> f32 {
    return max(clip_w, MIN_AFFINE_WEIGHT);
}

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

// B/2+F/2 on an encoded target: F is the 5-bit value without its expansion
// bits, and the target keeps a hair under half (alpha HALF_ALPHA), so the
// 8-bit result cuts to exactly (B+F)>>1 in 5 bits, as on the GPU
// (`material.rs`, `encoded_blend_matches_gpu`).
const HALF_ALPHA: f32 = 0.501953125;
fn half_source(rgb: vec3<f32>) -> vec3<f32> {
    let q = floor(round(clamp(rgb, vec3(0.0), vec3(1.0)) * 255.0) / 8.0);
    return q * 8.0 / 255.0;
}

// F/4 of the GPU's B+F/4: a quarter of the 5-bit value, truncated.
fn quarter(rgb: vec3<f32>) -> vec3<f32> {
    let q = floor(floor(round(clamp(rgb, vec3(0.0), vec3(1.0)) * 255.0) / 8.0) / 4.0);
    return (q * 8.0 + floor(q / 4.0)) / 255.0;
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
    let w = affine_weight(clip.w);
    out.w = w;
    var uv = vec2(0.0);
    @if(VERTEX_UVS_A)
    uv = v.uv;
    out.uv_w = uv * w;

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
    var shaded = color * light;
    if has(FLAG_DEPTH_CUE) {
        // Bevy looks down -Z. View-space depth also works for orthographic
        // cameras, whose clip.w is always 1. Evaluate the reciprocal here,
        // before interpolating colours or modulating with texture samples.
        let depth = max(-position_world_to_view(world.xyz).z, 0.0001);
        let cue = saturate(material.depth_cue.x + material.depth_cue.y / depth);
        shaded = mix(shaded, material.far_color.rgb, cue);
    }
    out.color_w = shaded * w;
    out.world = world.xyz;
    return out;
}

@fragment
fn fragment(in: PsxVarying) -> @location(0) vec4<f32> {
    let uv = in.uv_w / in.w;
    var rgb = in.color_w / in.w;
    // Untextured surfaces count as all-STP: a semi-transparent one blends whole.
    var stp = true;
    if has(FLAG_TEXTURED) {
        let texel = textureSample(color_texture, color_sampler, uv);
        if has(FLAG_CUTOUT) && texel.a < ALPHA_TRANSPARENT {
            discard;
        }
        stp = texel.a < ALPHA_STP;
        rgb = linear_to_srgb(texel.rgb) * min(rgb, vec3(2.0));
    } else {
        rgb = min(rgb, vec3(1.0));
    }
    let blend = (material.flags >> BLEND_SHIFT) & 7u;
    var semi = blend != 0u;
    if has(FLAG_STP_TEXEL) {
        semi = semi && stp;
        // One draw cannot both replace and subtract: the opaque texels are
        // drawn by a `PsxStp::OpaqueOnly` copy.
        if blend == BLEND_SUBTRACT && !stp {
            discard;
        }
    }
    if has(FLAG_OPAQUE_ONLY) && stp {
        discard;
    }
    @if(DISTANCE_FOG)
    if has(FLAG_FOG) {
        let distance = length(in.world - view.world_position);
        var f = 0.0;
        if fog.mode == 1u {
            f = saturate((distance - fog.be.x) / max(fog.be.y - fog.be.x, 0.0001));
        } else if fog.mode == 2u {
            f = 1.0 - exp(-fog.be.x * distance);
        } else if fog.mode == 3u {
            f = 1.0 - exp(-pow(fog.be.x * distance, 2.0));
        }
        rgb = mix(rgb, linear_to_srgb(fog.base_color.rgb), f);
    }

    // 15-bit colour at 8-bit precision, dithered like the GPU.
    var c8 = round(rgb * 255.0);
    if has(FLAG_DITHER) {
        var matrix = array<f32, 16>(-4.0, 0.0, -3.0, 1.0, 2.0, -2.0, 3.0, -1.0, -3.0, 1.0, -4.0, 0.0, 3.0, -1.0, 2.0, -2.0);
        let p = vec2<u32>(in.position.xy) % vec2(4u);
        c8 += vec3(matrix[p.y * 4u + p.x]);
    }
    let q = floor(clamp(c8, vec3(0.0), vec3(255.0)) / 8.0);
    rgb = (q * 8.0 + floor(q / 4.0)) / 255.0;
    return psx_output(rgb, semi, blend);
}

// Premultiplied output for the fixed-function blend set by `PsxMaterial`
// (`material.rs`, `blend_state`): the target keeps (1 − alpha) of what is
// below. `PsxView` targets store encoded colour (ENCODED_OUTPUT), so the
// blend acts on encoded values like the GPU's; other targets take linear.
fn psx_output(srgb: vec3<f32>, semi: bool, blend: u32) -> vec4<f32> {
    var rgb = srgb;
    if semi && blend == BLEND_QUARTER {
        rgb = quarter(rgb);
    }
    @if(ENCODED_OUTPUT)
    if semi && blend == BLEND_HALF {
        rgb = half_source(rgb);
    }
    @if(!ENCODED_OUTPUT)
    rgb = srgb_to_linear(rgb);
    // Opaque texels: (F, 1) replaces. Subtract has its own blend operation.
    var alpha = 1.0;
    if semi && blend == BLEND_HALF {
        rgb *= 0.5;
        alpha = HALF_ALPHA;
    } else if semi && (blend == BLEND_ADD || blend == BLEND_QUARTER) {
        alpha = 0.0;
    }
    return vec4(rgb, alpha);
}

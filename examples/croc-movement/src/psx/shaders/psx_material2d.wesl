// PlayStation-style 2D polygons for `PsxMaterial2d` (Bevy's Mesh2d vertex
// shader; 2D positions are already screen pixels, and screen-space
// interpolation is the PS1's affine mapping).
//
// Colour work happens on gamma-encoded values, as on the GPU: texel × colour
// (1.0 = unchanged, up to 2.0), 4x4 ordered dither, 15-bit quantisation, then
// one of the four blend equations, with per-texel STP.

import bevy_sprite_render::mesh2d::vertex_output::VertexOutput;

struct Psx2dUniform {
    color: vec4<f32>,
    flags: u32,
    palette_row: u32,
};

const FLAG_TEXTURED: u32 = 1u;
const FLAG_DITHER: u32 = 8u;
const FLAG_STP_TEXEL: u32 = 64u;
const FLAG_OPAQUE_ONLY: u32 = 128u;
const FLAG_PALETTE: u32 = 256u;
const BLEND_SHIFT: u32 = 12u;
const BLEND_HALF: u32 = 1u;
const BLEND_ADD: u32 = 2u;
const BLEND_SUBTRACT: u32 = 3u;
const BLEND_QUARTER: u32 = 4u;
const ALPHA_TRANSPARENT: f32 = 0.25;
const ALPHA_STP: f32 = 0.75;

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> material: Psx2dUniform;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var color_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var color_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var palette: texture_2d<f32>;

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

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var rgb = material.color.rgb;
    @if(VERTEX_COLORS)
    rgb = rgb * linear_to_srgb(in.color.rgb);
    var stp = true;
    if has(FLAG_TEXTURED) {
        var texel = textureSample(color_texture, color_sampler, in.uv);
        if has(FLAG_PALETTE) {
            let size = textureDimensions(palette);
            let index = min(u32(round(texel.r * 255.0)), size.x - 1u);
            texel = textureLoad(palette, vec2(index, min(material.palette_row, size.y - 1u)), 0);
        }
        if texel.a < ALPHA_TRANSPARENT {
            discard;
        }
        stp = texel.a < ALPHA_STP;
        rgb = linear_to_srgb(texel.rgb) * min(rgb, vec3(2.0));
    }
    let blend = (material.flags >> BLEND_SHIFT) & 7u;
    var semi = blend != 0u;
    if has(FLAG_STP_TEXEL) {
        semi = semi && stp;
        if blend == BLEND_SUBTRACT && !stp {
            discard;
        }
    }
    if has(FLAG_OPAQUE_ONLY) && stp {
        discard;
    }

    var c8 = round(clamp(rgb, vec3(0.0), vec3(1.0)) * 255.0);
    if has(FLAG_DITHER) {
        var matrix = array<f32, 16>(-4.0, 0.0, -3.0, 1.0, 2.0, -2.0, 3.0, -1.0, -3.0, 1.0, -4.0, 0.0, 3.0, -1.0, 2.0, -2.0);
        let p = vec2<u32>(in.position.xy) % vec2(4u);
        c8 += vec3(matrix[p.y * 4u + p.x]);
    }
    let q = floor(clamp(c8, vec3(0.0), vec3(255.0)) / 8.0);
    rgb = (q * 8.0 + floor(q / 4.0)) / 255.0;

    // Premultiplied output; the target keeps (1 − alpha) of what is below
    // (`material.rs`, `blend_state`).
    if semi && blend == BLEND_QUARTER {
        rgb = quarter(rgb);
    }
    @if(SRGB_OUTPUT)
    if semi && blend == BLEND_HALF {
        rgb = half_source(rgb);
    }
    @if(!SRGB_OUTPUT)
    rgb = srgb_to_linear(rgb);
    var alpha = 1.0;
    if semi && blend == BLEND_HALF {
        rgb *= 0.5;
        alpha = HALF_ALPHA;
    } else if semi && (blend == BLEND_ADD || blend == BLEND_QUARTER) {
        alpha = 0.0;
    }
    return vec4(rgb, alpha);
}

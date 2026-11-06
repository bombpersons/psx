// The canvas, scaled into the window's viewport. An sRGB surface encodes
// on write, so the encoded canvas is decoded first.
struct Present {
    // 1 when the surface is sRGB
    srgb: vec4<u32>,
};
@group(0) @binding(0) var canvas: texture_2d<f32>;
@group(0) @binding(1) var canvas_sampler: sampler;
@group(0) @binding(2) var<uniform> present: Present;

struct BlitOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) i: u32) -> BlitOut {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    var out: BlitOut;
    out.position = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3(0.0)) + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

@fragment
fn fs_blit(in: BlitOut) -> @location(0) vec4<f32> {
    var rgb = textureSample(canvas, canvas_sampler, in.uv).rgb;
    if present.srgb.x != 0u {
        rgb = srgb_to_linear(rgb);
    }
    return vec4(rgb, 1.0);
}

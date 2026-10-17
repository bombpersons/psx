// Final screen pass: copies the canvas that `PsxView` cameras drew into the
// `PsxScreen` image, quantised to 15-bit colour like the PS1 framebuffer
// (`PsxLook::quantize`). Runs once over 320x240 pixels.

import bevy_sprite_render::mesh2d::vertex_output::VertexOutput;

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var canvas: texture_2d<f32>;

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

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var rgb = textureLoad(canvas, vec2<i32>(in.position.xy), 0).rgb;
    @if(QUANTIZE) {
        // 8-bit sRGB → 5 bits per channel → back to 8 bits as the GPU expands them.
        let q = floor(round(clamp(linear_to_srgb(rgb), vec3(0.0), vec3(1.0)) * 255.0) / 8.0);
        rgb = srgb_to_linear((q * 8.0 + floor(q / 4.0)) / 255.0);
    }
    @if(SRGB_OUTPUT)
    rgb = linear_to_srgb(rgb);
    return vec4(rgb, 1.0);
}

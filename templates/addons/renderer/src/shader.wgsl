// Models and sprites, in either look. The canvas stores gamma-encoded colour
// as plain numbers (Rgba8Unorm), so the semi-transparency blends act on the
// encoded values as the PS1 GPU's did on VRAM.
//
// The PS1 look comes from how the PS1 computed things:
// - vertices snap to whole pixels of the low-resolution canvas (wobble),
// - UVs and colours interpolate linearly in screen space (affine warping),
// - lighting is per vertex (Gouraud) on encoded colour, where a vertex colour
//   of 1.0 leaves a texture unchanged and up to 2.0 brightens it,
// - output is 15-bit colour with the GPU's 4x4 ordered dither.
// The modern look turns each of those off: perspective-correct, full colour.

const MAX_LIGHTS: u32 = 4u;

struct Globals {
    clip_from_world: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    ambient: vec4<f32>,
    light_dirs: array<vec4<f32>, 4>,
    light_colors: array<vec4<f32>, 4>,
    fog_color: vec4<f32>,
    // near, far, on (1 or 0), unused
    fog: vec4<f32>,
    // canvas width and height, layout width and height
    sizes: vec4<f32>,
    // look flags, light count
    look: vec4<u32>,
};

struct Draw {
    world_from_local: mat4x4<f32>,
    normal_from_local: mat4x4<f32>,
    tint: vec4<f32>,
    // material flags, blend mode
    flags: vec4<u32>,
};

const LOOK_SNAP: u32 = 1u;
const LOOK_AFFINE: u32 = 2u;
const LOOK_DITHER: u32 = 4u;
const LOOK_QUANTIZE: u32 = 8u;

const TEXTURED: u32 = 1u;
const LIT: u32 = 2u;
const CUTOUT: u32 = 4u;
const STP_TEXEL: u32 = 8u;

const BLEND_HALF: u32 = 1u;
const BLEND_ADD: u32 = 2u;
const BLEND_SUBTRACT: u32 = 3u;
const BLEND_QUARTER: u32 = 4u;

// Texel alpha classes written by `psx textures`: 0 transparent, ~0.5 STP, 1 opaque.
const ALPHA_TRANSPARENT: f32 = 0.25;
const ALPHA_STP: f32 = 0.75;

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<uniform> d: Draw;
@group(1) @binding(0) var color_texture: texture_2d<f32>;
@group(1) @binding(1) var color_sampler: sampler;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};

// Affine interpolation: each attribute A is sent as A·w with
// perspective-correct interpolation and divided by the interpolated w, which
// gives the screen-linear A. The weight stays positive so a vertex behind the
// camera cannot make it run away. Without LOOK_AFFINE, w is 1.
struct Varying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv_w: vec2<f32>,
    @location(1) color_w: vec4<f32>,
    @location(2) w: f32,
};

fn look(flag: u32) -> bool {
    return (g.look.x & flag) != 0u;
}

fn material(flag: u32) -> bool {
    return (d.flags.x & flag) != 0u;
}

fn weight(clip_w: f32) -> f32 {
    if look(LOOK_AFFINE) {
        return max(clip_w, 1e-4);
    }
    return 1.0;
}

@vertex
fn vs_model(v: VertexIn) -> Varying {
    let world = d.world_from_local * vec4(v.position, 1.0);
    var clip = g.clip_from_world * world;
    if look(LOOK_SNAP) && clip.w > 0.0 {
        let size = g.sizes.xy;
        let pixel = round((clip.xy / clip.w * 0.5 + 0.5) * size);
        clip = vec4((pixel / size * 2.0 - 1.0) * clip.w, clip.z, clip.w);
    }
    var light = vec3(1.0);
    if material(LIT) {
        let n = normalize((d.normal_from_local * vec4(v.normal, 0.0)).xyz);
        light = g.ambient.rgb;
        for (var i = 0u; i < min(g.look.y, MAX_LIGHTS); i++) {
            light += g.light_colors[i].rgb * max(dot(n, g.light_dirs[i].xyz), 0.0);
        }
    }
    var shaded = v.color.rgb * d.tint.rgb * light;
    if g.fog.z > 0.0 {
        // Depth cue per vertex, before the texture modulates, as the GTE did.
        let depth = -(g.view_from_world * world).z;
        let f = clamp((depth - g.fog.x) / max(g.fog.y - g.fog.x, 1e-4), 0.0, 1.0);
        shaded = mix(shaded, g.fog_color.rgb, f);
    }
    var out: Varying;
    out.position = clip;
    let w = weight(clip.w);
    out.w = w;
    out.uv_w = v.uv * w;
    out.color_w = vec4(shaded, v.color.a * d.tint.a) * w;
    return out;
}

// Sprites: position in layout pixels (origin top left), no lighting.
@vertex
fn vs_sprite(v: VertexIn) -> Varying {
    let screen = g.sizes.zw;
    var out: Varying;
    out.position = vec4(v.position.x / screen.x * 2.0 - 1.0, 1.0 - v.position.y / screen.y * 2.0, 0.0, 1.0);
    out.w = 1.0;
    out.uv_w = v.uv;
    out.color_w = v.color * d.tint;
    return out;
}

// B/2+F/2 on the 15-bit values: F without its expansion bits, and the canvas
// keeps a hair under half (HALF_ALPHA), so the 8-bit result cuts to exactly
// (B+F)>>1 in 5 bits.
const HALF_ALPHA: f32 = 0.501953125;
fn half_source(rgb: vec3<f32>) -> vec3<f32> {
    let q = floor(round(clamp(rgb, vec3(0.0), vec3(1.0)) * 255.0) / 8.0);
    return q * 8.0 / 255.0;
}

// F/4 of B+F/4: a quarter of the 5-bit value, truncated.
fn quarter(rgb: vec3<f32>) -> vec3<f32> {
    let q = floor(floor(round(clamp(rgb, vec3(0.0), vec3(1.0)) * 255.0) / 8.0) / 4.0);
    return (q * 8.0 + floor(q / 4.0)) / 255.0;
}

fn shade(in: Varying, dither: bool) -> vec4<f32> {
    let uv = in.uv_w / in.w;
    let color = in.color_w / in.w;
    var rgb = color.rgb;
    // Untextured surfaces count as all-STP: a semi-transparent one blends whole.
    var stp = true;
    if material(TEXTURED) {
        let texel = textureSample(color_texture, color_sampler, uv);
        if material(CUTOUT) && texel.a < ALPHA_TRANSPARENT {
            discard;
        }
        stp = texel.a < ALPHA_STP;
        rgb = texel.rgb * min(rgb, vec3(2.0));
    } else {
        rgb = min(rgb, vec3(1.0));
    }
    let blend = d.flags.y;
    var semi = blend != 0u;
    if material(STP_TEXEL) {
        semi = semi && stp;
        // One draw cannot both replace and subtract: subtract draws only
        // its STP texels.
        if blend == BLEND_SUBTRACT && !stp {
            discard;
        }
    }
    let quantize = look(LOOK_QUANTIZE);
    if quantize {
        // 15-bit colour at 8-bit precision, dithered like the GPU.
        var c8 = round(rgb * 255.0);
        if dither && look(LOOK_DITHER) {
            var matrix = array<f32, 16>(-4.0, 0.0, -3.0, 1.0, 2.0, -2.0, 3.0, -1.0, -3.0, 1.0, -4.0, 0.0, 3.0, -1.0, 2.0, -2.0);
            let p = vec2<u32>(in.position.xy) % vec2(4u);
            c8 += vec3(matrix[p.y * 4u + p.x]);
        }
        let q = floor(clamp(c8, vec3(0.0), vec3(255.0)) / 8.0);
        rgb = (q * 8.0 + floor(q / 4.0)) / 255.0;
    }
    if semi && blend == BLEND_QUARTER {
        if quantize {
            rgb = quarter(rgb);
        } else {
            rgb = rgb * 0.25;
        }
    }
    // Premultiplied output for the blend state of the draw's mode: the
    // canvas keeps (1 - alpha) of what is below. Subtract has its own.
    var alpha = 1.0;
    if semi && blend == BLEND_HALF {
        if quantize {
            rgb = half_source(rgb);
        }
        rgb *= 0.5;
        alpha = HALF_ALPHA;
    } else if semi && (blend == BLEND_ADD || blend == BLEND_QUARTER) {
        alpha = 0.0;
    }
    return vec4(rgb, alpha);
}

@fragment
fn fs_model(in: Varying) -> @location(0) vec4<f32> {
    return shade(in, true);
}

// Sprites are not dithered, as on the PS1.
@fragment
fn fs_sprite(in: Varying) -> @location(0) vec4<f32> {
    if (in.color_w.a <= 0.0) {
        discard;
    }
    return shade(in, false);
}

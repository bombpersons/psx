//! `PsxMaterial`: a Bevy `Material` with PlayStation-style shading.
//! Use it like `StandardMaterial`: `MeshMaterial3d(materials.add(PsxMaterial { .. }))`.
use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{Material, MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::{
        render_asset::RenderAssets,
        render_resource::{
            AsBindGroup, AsBindGroupShaderType, BlendComponent, BlendFactor, BlendOperation, BlendState,
            CompareFunction, DepthBiasState, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
            TextureFormat,
        },
        texture::GpuImage,
    },
    shader::ShaderRef,
};

use super::{MAX_LIGHTS, PsxLighting, PsxLook};

/// PS1 semi-transparency equations (B = what is already drawn, F = this surface).
/// Under a `PsxView` camera, 2D or 3D, they act on gamma-encoded colour like
/// the GPU's (on the 15-bit values, saturating per channel), not on linear light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect)]
pub enum PsxBlend {
    #[default]
    Opaque,
    /// B/2 + F/2: glass, shadows, ghosts.
    Half,
    /// B + F: fire, glows, light beams.
    Add,
    /// B − F: darkening effects.
    Subtract,
    /// B + F/4: subtle additive haze.
    AddQuarter,
}

/// Which texels of a semi-transparent surface blend: the PS1's per-texel
/// STP bit (bit 15 of a texel or CLUT entry). Images from `psx textures` and
/// [`super::psx_palette`] encode it in alpha: 0 = transparent (0000h),
/// about 0.5 = STP set, 1 = opaque.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Reflect)]
pub enum PsxStp {
    /// Every texel uses the surface's blend: ordinary textures. Default.
    #[default]
    All,
    /// The PS1 rule: only texels with alpha below 0.75 (STP set) blend;
    /// the others draw opaque. Untextured surfaces blend whole.
    /// The GPU's `Subtract` replaces with some texels and subtracts with
    /// others, which one draw cannot do: with `Subtract`, only STP texels are
    /// drawn, so pair it with a copy using [`PsxStp::OpaqueOnly`].
    Texel,
    /// Draw only texels without STP, opaque (the other half of a
    /// `Subtract` + [`PsxStp::Texel`] pair). Ignores the blend mode.
    OpaqueOnly,
}

/// Texel alpha below this is transparent (texel 0000h).
pub const ALPHA_TRANSPARENT: f32 = 0.25;

// Material flags shared by the PSX shaders (psx_material, psx_material2d, psx_sprite).
pub(super) const TEXTURED: u32 = 1;
pub(super) const LIT: u32 = 2;
pub(super) const FOG: u32 = 4;
pub(super) const DITHER: u32 = 8;
pub(super) const SNAP: u32 = 16;
pub(super) const CUTOUT: u32 = 32;
pub(super) const STP_TEXEL: u32 = 64;
pub(super) const OPAQUE_ONLY: u32 = 128;
pub(super) const PALETTE: u32 = 256;
const DEPTH_CUE: u32 = 512;
/// Bits 12-14: the blend mode, as [`blend_code`].
pub(super) const BLEND_SHIFT: u32 = 12;

/// Flags for the semi-transparency options.
pub(super) fn blend_flags(blend: PsxBlend, stp: PsxStp) -> u32 {
    let bits = match stp {
        PsxStp::All => 0,
        PsxStp::Texel => STP_TEXEL,
        PsxStp::OpaqueOnly => OPAQUE_ONLY,
    };
    bits | blend_code(drawn_blend(blend, stp)) << BLEND_SHIFT
}

/// The blend mode a material actually draws with.
pub(super) fn drawn_blend(blend: PsxBlend, stp: PsxStp) -> PsxBlend {
    if stp == PsxStp::OpaqueOnly { PsxBlend::Opaque } else { blend }
}

fn blend_code(blend: PsxBlend) -> u32 {
    match blend {
        PsxBlend::Opaque => 0,
        PsxBlend::Half => 1,
        PsxBlend::Add => 2,
        PsxBlend::Subtract => 3,
        PsxBlend::AddQuarter => 4,
    }
}

/// Fixed-function blending for a PSX blend mode. Shaders output a
/// premultiplied colour and, in alpha, how much of the background to keep:
/// opaque texels (F, 1) replace, `Half` gives (F/2, ½), `Add` (F, 0),
/// `AddQuarter` (F/4, 0). `Subtract` needs its own operation. `None` = opaque.
pub(super) fn blend_state(blend: PsxBlend) -> Option<BlendState> {
    let color = match blend {
        PsxBlend::Opaque => return None,
        PsxBlend::Half | PsxBlend::Add | PsxBlend::AddQuarter => BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::OneMinusSrcAlpha,
            operation: BlendOperation::Add,
        },
        PsxBlend::Subtract => BlendComponent {
            src_factor: BlendFactor::One,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::ReverseSubtract,
        },
    };
    // The target keeps its alpha.
    let alpha =
        BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add };
    Some(BlendState { color, alpha })
}

/// Sets the first colour target's blending.
pub(super) fn set_blend(descriptor: &mut RenderPipelineDescriptor, blend: PsxBlend) {
    if let Some(target) = descriptor.fragment.as_mut().and_then(|f| f.targets.first_mut()).and_then(|t| t.as_mut()) {
        target.blend = blend_state(blend);
    }
}

/// Whether a main pass target stores gamma-encoded colour as plain numbers:
/// the `Rgba8Unorm` texture of `PsxView` cameras (see `encode_psx_views`).
/// The shader then writes encoded values and the blend works on them, as on
/// the GPU. Other targets (sRGB, HDR) take linear light.
pub(super) fn stores_encoded(format: TextureFormat) -> bool {
    matches!(format, TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm)
}

/// Per-vertex reciprocal depth cue: `clamp(bias + scale / view_depth, 0, 1)`.
/// Positive depth is forward from the camera, in scene units. The result blends
/// the lit vertex colour toward `far_color`, before affine interpolation and
/// texture modulation, as in the GTE -> GPU pipeline. This is authored floating
/// point shading, without the original register precision or divide limits.
#[derive(Clone, Copy, Debug, Reflect)]
pub struct PsxDepthCue {
    /// Dimensionless offset of the reciprocal curve.
    pub bias: f32,
    /// Reciprocal coefficient in scene units; normally negative for distant fog.
    pub scale: f32,
    /// Target vertex colour, in sRGB modulation units (white = neutral on a
    /// texture, 2x white = twice as bright). Alpha is unused. Black darkens.
    pub far_color: Color,
}

impl PsxDepthCue {
    /// A reciprocal curve with no cue at `near` and full cue at `far`.
    /// Both distances use scene units. Invalid/non-finite ranges return None.
    pub fn from_distances(near: f32, far: f32, far_color: Color) -> Option<Self> {
        if !near.is_finite() || !far.is_finite() || near <= 0.0 || far <= near {
            return None;
        }
        let bias = far / (far - near);
        let cue = Self { bias, scale: -near * bias, far_color };
        cue.is_valid().then_some(cue)
    }

    /// The authored factor, useful for tools and previews. Nonpositive depth is
    /// clamped just ahead of the camera to keep the reciprocal finite.
    pub fn factor(&self, view_depth: f32) -> f32 {
        (self.bias + self.scale / view_depth.max(0.0001)).clamp(0.0, 1.0)
    }

    fn is_valid(&self) -> bool {
        let c = self.far_color.to_srgba();
        [self.bias, self.scale, c.red, c.green, c.blue].iter().all(|v| v.is_finite())
            && c.red >= 0.0
            && c.green >= 0.0
            && c.blue >= 0.0
    }
}

/// [`PsxMaterial::depth_bias`]: GPU depth bias in the direction away from the
/// camera, whatever the depth convention (Bevy's depth is reversed). The
/// offset is `constant` steps of depth precision plus `slope_scale` times the
/// polygon's depth slope; slope keeps surfaces seen at a grazing angle apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Reflect)]
pub struct PsxDepthBias {
    pub constant: i32,
    pub slope_scale: f32,
}

impl PsxDepthBias {
    pub const NONE: Self = Self { constant: 0, slope_scale: 0.0 };
    /// Enough to sit just behind a coplanar surface at any angle.
    pub const BEHIND: Self = Self { constant: 4, slope_scale: 1.0 };

    /// The pipeline state for Bevy's reversed depth (near = 1, test
    /// `GreaterEqual`): away from the camera is negative.
    fn state(self) -> DepthBiasState {
        DepthBiasState { constant: -self.constant, slope_scale: -self.slope_scale, clamp: 0.0 }
    }
}

#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[uniform(0, PsxUniform)]
#[bind_group_data(PsxMaterialKey)]
pub struct PsxMaterial {
    /// Tint in sRGB. White is neutral; textured surfaces may go up to 2.0 to brighten.
    pub color: Color,
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    pub blend: PsxBlend,
    /// Which texels `blend` applies to (PS1 per-texel STP bit).
    pub stp: PsxStp,
    /// Gouraud lighting from Bevy `DirectionalLight`s and `AmbientLight`.
    pub lit: bool,
    /// Apply the camera's `DistanceFog`, from each pixel's distance to the camera.
    /// Used only when `depth_cue` is None (or contains invalid values).
    pub fog: bool,
    /// Optional GTE-style reciprocal cue; overrides camera `DistanceFog` for
    /// this material, independently of `fog`. None preserves ordinary fog.
    pub depth_cue: Option<PsxDepthCue>,
    /// Dither this surface when [`PsxLook::dither`] is on. The GPU dithers
    /// shaded and texture-modulated polygons; raw-textured ones were not.
    pub dither: bool,
    pub double_sided: bool,
    /// Texels with alpha < 0.25 are not drawn (the PS1's transparent colour 0x0000).
    pub cutout: bool,
    /// Painter's order instead of the depth buffer, like the PS1's ordering
    /// tables. When set, the surface neither tests nor writes depth and is
    /// drawn after every surface with a lower order and before every higher
    /// one (all ordered surfaces go through Bevy's sorted transparent pass).
    /// Equal orders fall back to back-to-front. `None` (default) depth-tests.
    pub draw_order: Option<u32>,
    /// Pushes this surface's depth away from the camera for the depth test,
    /// so it loses to coplanar surfaces (decals, or a game's crack-filler
    /// copy of a polygon it also draws subdivided). Zero (default) is none.
    /// Ignored with `draw_order`, which does not test depth.
    pub depth_bias: PsxDepthBias,
    /// Copied from the [`PsxLook`] resource; edit the resource instead.
    pub look: PsxLook,
    /// Copied from scene lights each frame they change; not set by hand.
    #[reflect(ignore)]
    pub lighting: PsxLighting,
}

impl Default for PsxMaterial {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            texture: None,
            blend: PsxBlend::Opaque,
            stp: PsxStp::All,
            lit: true,
            fog: true,
            depth_cue: None,
            dither: true,
            double_sided: false,
            cutout: true,
            draw_order: None,
            depth_bias: PsxDepthBias::NONE,
            look: PsxLook::default(),
            lighting: PsxLighting::default(),
        }
    }
}

impl PsxMaterial {
    pub fn textured(texture: Handle<Image>) -> Self {
        Self { texture: Some(texture), ..default() }
    }
    pub fn color(color: Color) -> Self {
        Self { color, ..default() }
    }
}

#[derive(Clone, Default, ShaderType)]
pub struct PsxUniform {
    pub color: Vec4,
    pub ambient: Vec4,
    pub depth_cue: Vec4,
    pub far_color: Vec4,
    pub light_dirs: [Vec4; MAX_LIGHTS],
    pub light_colors: [Vec4; MAX_LIGHTS],
    pub light_count: u32,
    pub flags: u32,
}

impl AsBindGroupShaderType<PsxUniform> for PsxMaterial {
    fn as_bind_group_shader_type(&self, _: &RenderAssets<GpuImage>) -> PsxUniform {
        let srgb = self.color.to_srgba();
        let flag = |on: bool, bit: u32| if on { bit } else { 0 };
        let depth_cue = self.depth_cue.filter(PsxDepthCue::is_valid);
        let (curve, far_color) = depth_cue.map_or((Vec4::ZERO, Vec4::ZERO), |cue| {
            let c = cue.far_color.to_srgba();
            (Vec4::new(cue.bias, cue.scale, 0.0, 0.0), Vec4::new(c.red, c.green, c.blue, 0.0))
        });
        PsxUniform {
            color: Vec4::new(srgb.red, srgb.green, srgb.blue, srgb.alpha),
            ambient: self.lighting.ambient.extend(0.0),
            depth_cue: curve,
            far_color,
            light_dirs: self.lighting.directions.map(|d| d.extend(0.0)),
            light_colors: self.lighting.colors.map(|c| c.extend(0.0)),
            light_count: self.lighting.count,
            flags: flag(self.texture.is_some(), TEXTURED)
                | flag(self.lit, LIT)
                | flag(self.fog && depth_cue.is_none(), FOG)
                | flag(depth_cue.is_some(), DEPTH_CUE)
                | flag(self.look.dither && self.dither, DITHER)
                | flag(self.look.vertex_snap, SNAP)
                | flag(self.cutout, CUTOUT)
                | blend_flags(self.blend, self.stp),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PsxMaterialKey {
    blend: PsxBlend,
    double_sided: bool,
    painter: bool,
    /// `depth_bias` as (constant, slope_scale bits): f32 isn't `Hash`.
    depth_bias: (i32, u32),
}

impl From<&PsxMaterial> for PsxMaterialKey {
    fn from(m: &PsxMaterial) -> Self {
        let painter = m.draw_order.is_some();
        let bias = if painter { PsxDepthBias::NONE } else { m.depth_bias };
        Self {
            blend: drawn_blend(m.blend, m.stp),
            double_sided: m.double_sided,
            painter,
            // +0.0 so that -0.0 shares a pipeline with it.
            depth_bias: (bias.constant, (bias.slope_scale + 0.0).to_bits()),
        }
    }
}

/// Sort-distance step between consecutive [`PsxMaterial::draw_order`]s. Bevy
/// sorts transparent items by view depth (metres) plus the material's depth
/// bias, so each step must exceed the depth range of anything on screen.
const DRAW_ORDER_STEP: f32 = 8192.0;

const SHADER: Handle<Shader> = super::shaders::MATERIAL;

impl Material for PsxMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        // Semi-transparent surfaces go through Bevy's sorted transparent pass;
        // the exact equation is set in `specialize`.
        // Painter's-order surfaces join that pass too, to be sorted by order.
        match drawn_blend(self.blend, self.stp) {
            PsxBlend::Opaque if self.draw_order.is_none() => AlphaMode::Opaque,
            _ => AlphaMode::Blend,
        }
    }

    fn depth_bias(&self) -> f32 {
        // Bevy only uses this for sorting: the transparent pass draws in
        // ascending (view depth + bias), so a larger order draws later.
        self.draw_order.map_or(0.0, |order| order as f32 * DRAW_ORDER_STEP)
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let k = key.bind_group_data;
        descriptor.primitive.cull_mode = if k.double_sided { None } else { Some(Face::Back) };
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            if k.painter {
                depth.depth_compare = Some(CompareFunction::Always);
                depth.depth_write_enabled = Some(false);
            } else if k.depth_bias != (0, 0) {
                let (constant, slope) = k.depth_bias;
                depth.bias = PsxDepthBias { constant, slope_scale: f32::from_bits(slope) }.state();
            }
        }
        // Opaque replaces what is below (painter's-order surfaces are in
        // the transparent pass, which would otherwise alpha-blend them).
        set_blend(descriptor, k.blend);
        if stores_encoded(key.mesh_key.target_format())
            && let Some(fragment) = descriptor.fragment.as_mut()
        {
            fragment.shader_defs.push("ENCODED_OUTPUT".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reciprocal_ranges_match_known_cue_values_and_reject_invalid_inputs() {
        // King's Field 80061008, setting=40 scene units: cue=1.25-25/z.
        // This example is produced by the generic 20..100 distance range.
        let cue = PsxDepthCue::from_distances(20.0, 100.0, Color::BLACK).unwrap();
        assert_eq!((cue.bias, cue.scale), (1.25, -25.0));
        for (depth, expected) in
            [(-1.0, 0.0), (0.0, 0.0), (10.0, 0.0), (20.0, 0.0), (40.0, 0.625), (100.0, 1.0), (200.0, 1.0)]
        {
            assert_eq!(cue.factor(depth), expected);
        }
        // A different authored curve and a coloured target are supported too.
        let cue = PsxDepthCue { bias: 0.0, scale: 10.0, far_color: Color::srgb(0.2, 0.4, 0.6) };
        assert_eq!(cue.factor(20.0), 0.5);
        for (near, far) in
            [(0.0, 10.0), (-1.0, 10.0), (10.0, 10.0), (20.0, 10.0), (1.0, f32::INFINITY), (f32::NAN, 10.0)]
        {
            assert!(PsxDepthCue::from_distances(near, far, Color::BLACK).is_none());
        }
    }

    #[test]
    fn recorded_gte_reference_bounds_authored_float_precision() {
        // Generated with psx-gte RTPS: identity rotation, H=200, DQA=-12800,
        // DQB=0x01400000, translation Z below. These correspond to King's Field
        // PAL's setting 16000. See research/template-depth-cue.md in the toolkit.
        // GTE reciprocal and IR0 truncation are deliberately not reproduced by
        // the authored shader. This fixture bounds their visible-scale error.
        let cue = PsxDepthCue { bias: 1.25, scale: -0.625 * 16000.0 / 1024.0, far_color: Color::BLACK };
        for (depth, ir0) in [
            (2000, 0),
            (4096, 0),
            (8192, 120),
            (12500, 1841),
            (16000, 2560),
            (20000, 3073),
            (20480, 3120),
            (30000, 3754),
            (40000, 4095),
            (60000, 4096),
        ] {
            assert!((cue.factor(depth as f32 / 1024.0) - ir0 as f32 / 4096.0).abs() < 0.00055, "depth {depth}");
        }
    }

    #[test]
    fn depth_cue_uniforms_override_only_ordinary_fog_and_default_to_disabled() {
        let images = RenderAssets::<GpuImage>::default();
        let mut material = PsxMaterial::default();
        let uniform: PsxUniform = material.as_bind_group_shader_type(&images);
        assert!(material.depth_cue.is_none());
        assert_eq!(uniform.flags & (FOG | DEPTH_CUE), FOG);
        material.depth_cue = PsxDepthCue::from_distances(20.0, 100.0, Color::srgb(0.2, 0.4, 0.6));
        for fog in [true, false] {
            material.fog = fog;
            let uniform: PsxUniform = material.as_bind_group_shader_type(&images);
            assert_eq!(uniform.flags & (FOG | DEPTH_CUE), DEPTH_CUE);
            assert_eq!(uniform.depth_cue, Vec4::new(1.25, -25.0, 0.0, 0.0));
            assert_eq!(uniform.far_color, Vec4::new(0.2, 0.4, 0.6, 0.0));
        }
        material.fog = true;
        material.depth_cue.as_mut().unwrap().scale = f32::NAN;
        let uniform: PsxUniform = material.as_bind_group_shader_type(&images);
        assert_eq!(uniform.flags & (FOG | DEPTH_CUE), FOG);
        assert_eq!(uniform.depth_cue, Vec4::ZERO);
        assert_eq!(uniform.far_color, Vec4::ZERO);
    }

    #[test]
    fn opaque_only_draws_opaque() {
        assert_eq!(drawn_blend(PsxBlend::Subtract, PsxStp::OpaqueOnly), PsxBlend::Opaque);
        assert_eq!(blend_flags(PsxBlend::Subtract, PsxStp::OpaqueOnly), OPAQUE_ONLY);
        assert_eq!(blend_flags(PsxBlend::Half, PsxStp::Texel), STP_TEXEL | 1 << BLEND_SHIFT);
        assert!(blend_state(PsxBlend::Opaque).is_none());
    }

    /// 5-bit channel as the shaders and the screen pass expand it to 8 bits.
    fn expand(q: u32) -> f32 {
        (q * 8 + q / 4) as f32 / 255.0
    }

    /// What the shaders write on an encoded (`PsxView`) target for a
    /// semi-transparent texel of 5-bit colour `f`: premultiplied colour and
    /// alpha (`psx_output` in psx_material.wesl, likewise in the 2D shaders).
    fn shader_output(blend: PsxBlend, f: u32) -> (f32, f32) {
        match blend {
            PsxBlend::Opaque => (expand(f), 1.0),
            PsxBlend::Half => (f as f32 * 8.0 / 255.0 * 0.5, 0.5 + 1.0 / 512.0),
            PsxBlend::Add => (expand(f), 0.0),
            PsxBlend::Subtract => (expand(f), 1.0),
            PsxBlend::AddQuarter => (expand(f / 4), 0.0),
        }
    }

    /// The GPU's fixed-function colour blend for `state`, then the store into
    /// the 8-bit target and the screen pass's cut to 5 bits.
    fn blend_on_gpu(state: Option<BlendState>, (src, alpha): (f32, f32), dst: f32) -> u32 {
        let value = match state {
            None => src,
            Some(BlendState { color, .. }) => {
                let factor = |f| match f {
                    BlendFactor::One => 1.0,
                    BlendFactor::Zero => 0.0,
                    BlendFactor::OneMinusSrcAlpha => 1.0 - alpha,
                    other => panic!("unexpected factor {other:?}"),
                };
                let (s, d) = (src * factor(color.src_factor), dst * factor(color.dst_factor));
                match color.operation {
                    BlendOperation::Add => s + d,
                    BlendOperation::ReverseSubtract => d - s,
                    other => panic!("unexpected operation {other:?}"),
                }
            }
        };
        (value.clamp(0.0, 1.0) * 255.0).round() as u32 / 8
    }

    #[test]
    fn depth_bias_keys_pipelines_and_is_dropped_by_painters_order() {
        let key = |depth_bias, draw_order| PsxMaterialKey::from(&PsxMaterial { depth_bias, draw_order, ..default() });
        let flat = key(PsxDepthBias::NONE, None);
        assert_ne!(key(PsxDepthBias::BEHIND, None), flat);
        assert_eq!(key(PsxDepthBias { constant: 0, slope_scale: -0.0 }, None), flat, "-0.0 is no bias");
        assert_eq!(key(PsxDepthBias::BEHIND, Some(3)), key(PsxDepthBias::NONE, Some(3)), "painter's order ignores it");
        // Bevy's depth is reversed, so "behind" is a negative GPU bias.
        let state = PsxDepthBias::BEHIND.state();
        assert!(state.constant < 0 && state.slope_scale < 0.0);
    }

    /// Blending gamma-encoded values (what `PsxView` targets store) gives
    /// the GPU's 5-bit results exactly, for every pair of 5-bit colours.
    #[test]
    fn encoded_blend_matches_gpu() {
        let gpu = |blend, b: u32, f: u32| match blend {
            PsxBlend::Opaque => f,
            PsxBlend::Half => (b + f) >> 1,
            PsxBlend::Add => (b + f).min(31),
            PsxBlend::Subtract => b.saturating_sub(f),
            PsxBlend::AddQuarter => (b + (f >> 2)).min(31),
        };
        for blend in [PsxBlend::Opaque, PsxBlend::Half, PsxBlend::Add, PsxBlend::Subtract, PsxBlend::AddQuarter] {
            for b in 0..32 {
                for f in 0..32 {
                    let got = blend_on_gpu(blend_state(blend), shader_output(blend, f), expand(b));
                    assert_eq!(got, gpu(blend, b, f), "{blend:?} B={b} F={f}");
                }
            }
        }
    }
}

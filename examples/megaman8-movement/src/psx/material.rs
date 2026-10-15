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
            CompareFunction, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureFormat,
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
    /// Depth cueing from the camera's `DistanceFog` (per vertex, like the GTE).
    pub fog: bool,
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
            dither: true,
            double_sided: false,
            cutout: true,
            draw_order: None,
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
    pub light_dirs: [Vec4; MAX_LIGHTS],
    pub light_colors: [Vec4; MAX_LIGHTS],
    pub light_count: u32,
    pub flags: u32,
}

impl AsBindGroupShaderType<PsxUniform> for PsxMaterial {
    fn as_bind_group_shader_type(&self, _: &RenderAssets<GpuImage>) -> PsxUniform {
        let srgb = self.color.to_srgba();
        let flag = |on: bool, bit: u32| if on { bit } else { 0 };
        PsxUniform {
            color: Vec4::new(srgb.red, srgb.green, srgb.blue, srgb.alpha),
            ambient: self.lighting.ambient.extend(0.0),
            light_dirs: self.lighting.directions.map(|d| d.extend(0.0)),
            light_colors: self.lighting.colors.map(|c| c.extend(0.0)),
            light_count: self.lighting.count,
            flags: flag(self.texture.is_some(), TEXTURED)
                | flag(self.lit, LIT)
                | flag(self.fog, FOG)
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
}

impl From<&PsxMaterial> for PsxMaterialKey {
    fn from(m: &PsxMaterial) -> Self {
        Self { blend: drawn_blend(m.blend, m.stp), double_sided: m.double_sided, painter: m.draw_order.is_some() }
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
        if k.painter
            && let Some(depth) = descriptor.depth_stencil.as_mut()
        {
            depth.depth_compare = Some(CompareFunction::Always);
            depth.depth_write_enabled = Some(false);
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

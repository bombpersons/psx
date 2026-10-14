//! `PsxMaterial`: a Bevy `Material` with PlayStation-style shading.
//! Use it like `StandardMaterial`: `MeshMaterial3d(materials.add(PsxMaterial { .. }))`.
use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{Material, MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::{
        render_asset::RenderAssets,
        render_resource::{
            AsBindGroup, AsBindGroupShaderType, BlendComponent, BlendFactor, BlendOperation,
            BlendState, CompareFunction, Face, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
        },
        texture::GpuImage,
    },
    shader::ShaderRef,
};

use super::{PsxLighting, PsxLook, MAX_LIGHTS};

/// PS1 semi-transparency equations (B = what is already drawn, F = this surface).
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
    /// Gouraud lighting from Bevy `DirectionalLight`s and `AmbientLight`.
    pub lit: bool,
    /// Depth cueing from the camera's `DistanceFog` (per vertex, like the GTE).
    pub fog: bool,
    pub double_sided: bool,
    /// Texels with alpha < 0.5 are not drawn (the PS1's transparent colour 0x0000).
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
            lit: true,
            fog: true,
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

const TEXTURED: u32 = 1;
const LIT: u32 = 2;
const FOG: u32 = 4;
const DITHER: u32 = 8;
const SNAP: u32 = 16;
const CUTOUT: u32 = 32;
const BLEND_HALF: u32 = 64;
const BLEND_QUARTER: u32 = 128;

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
                | flag(self.look.dither, DITHER)
                | flag(self.look.vertex_snap, SNAP)
                | flag(self.cutout, CUTOUT)
                | flag(self.blend == PsxBlend::Half, BLEND_HALF)
                | flag(self.blend == PsxBlend::AddQuarter, BLEND_QUARTER),
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
        Self { blend: m.blend, double_sided: m.double_sided, painter: m.draw_order.is_some() }
    }
}

/// Sort-distance step between consecutive [`PsxMaterial::draw_order`]s. Bevy
/// sorts transparent items by view depth (metres) plus the material's depth
/// bias, so each step must exceed the depth range of anything on screen.
const DRAW_ORDER_STEP: f32 = 8192.0;

const SHADER: &str = concat!("embedded://", env!("CARGO_CRATE_NAME"), "/psx/shaders/psx_material.wesl");

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
        match self.blend {
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
        let color = match k.blend {
            // Opaque replaces what is below (painter's-order surfaces are in
            // the transparent pass, which would otherwise alpha-blend them).
            PsxBlend::Opaque => {
                if let Some(target) = descriptor.fragment.as_mut().and_then(|f| f.targets.first_mut()).and_then(|t| t.as_mut()) {
                    target.blend = None;
                }
                return Ok(());
            }
            PsxBlend::Half => BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            PsxBlend::Add | PsxBlend::AddQuarter => BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            PsxBlend::Subtract => BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            },
        };
        let keep_alpha = BlendComponent {
            src_factor: BlendFactor::Zero,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        };
        if let Some(target) = descriptor.fragment.as_mut().and_then(|f| f.targets.first_mut()).and_then(|t| t.as_mut()) {
            target.blend = Some(BlendState { color, alpha: keep_alpha });
        }
        Ok(())
    }
}

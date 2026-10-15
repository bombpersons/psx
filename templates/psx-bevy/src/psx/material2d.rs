//! PS1 look for 2D: semi-transparency equations, per-texel STP and CLUT
//! palettes for ordinary Bevy 2D entities.
//!
//! - [`PsxSprite`] extends a normal `Sprite` through Bevy's
//!   `SpriteMaterial<PsxSprite>`: atlases, flips, `rect` and anchors keep
//!   working. Add it for blend modes or palettes; plain sprites need nothing.
//! - [`PsxMaterial2d`] is a `Material2d` for `Mesh2d` (any 2D polygon, with
//!   vertex colours as Gouraud shading); it also dithers like the GPU.
//!
//! Palettes: the texture holds 8-bit indices (load it with [`load_indices`] or
//! build it with [`psx_indices`]) and `palette` is an N×rows RGBA image
//! ([`psx_palette`], or a `clut_*.png` from `psx textures`), one palette per
//! row like CLUTs in VRAM. Entry alpha follows the PS1: 0 (colour 0000h) is
//! transparent, about 0.5 has the STP bit, 1 is opaque. Swap or cycle
//! palettes by changing `palette_row`, `palette` or the palette image's pixels.
use bevy::{
    asset::RenderAssetUsages,
    image::{ImageLoaderSettings, ImageSampler},
    mesh::MeshVertexBufferLayoutRef,
    prelude::*,
    render::{
        render_asset::RenderAssets,
        render_resource::{
            AsBindGroup, AsBindGroupShaderType, Extent3d, RenderPipelineDescriptor, ShaderType,
            SpecializedMeshPipelineError, TextureDimension, TextureFormat,
        },
        texture::GpuImage,
    },
    shader::ShaderRef,
    sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPipeline},
};

use super::{
    PsxBlend, PsxLook, PsxStp,
    material::{ALPHA_TRANSPARENT, DITHER, PALETTE, TEXTURED, blend_flags, drawn_blend, set_blend},
};

const MATERIAL2D_SHADER: Handle<Shader> = super::shaders::MATERIAL2D;
const SPRITE_SHADER: Handle<Shader> = super::shaders::SPRITE;

/// A `Material2d` with PS1 semantics for `Mesh2d`: texture × colour
/// modulation (white = unchanged, up to 2.0 brightens), vertex colours as
/// Gouraud shading, the four blend equations with per-texel STP, optional
/// CLUT palette, and the GPU's dither when [`PsxLook::dither`] is on.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
#[uniform(0, Psx2dUniform)]
#[bind_group_data(PsxBlendKey)]
pub struct PsxMaterial2d {
    /// Tint in sRGB, multiplied with vertex colours.
    pub color: Color,
    /// Colours, or 8-bit palette indices when `palette` is set.
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    /// Palette image (one palette per row), looked up by the texture's indices.
    #[texture(3)]
    pub palette: Option<Handle<Image>>,
    pub palette_row: u32,
    pub blend: PsxBlend,
    pub stp: PsxStp,
    /// Dither when [`PsxLook::dither`] is on (the GPU dithers shaded and
    /// texture-modulated polygons, not sprites or raw textures).
    pub dither: bool,
    /// Copied from the [`PsxLook`] resource; edit the resource instead.
    pub look: PsxLook,
}

impl Default for PsxMaterial2d {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            texture: None,
            palette: None,
            palette_row: 0,
            blend: PsxBlend::Opaque,
            stp: PsxStp::All,
            dither: true,
            look: PsxLook::default(),
        }
    }
}

impl PsxMaterial2d {
    pub fn textured(texture: Handle<Image>) -> Self {
        Self { texture: Some(texture), ..default() }
    }
    /// `indices` drawn through row `row` of `palette`.
    pub fn indexed(indices: Handle<Image>, palette: Handle<Image>, row: u32) -> Self {
        Self { texture: Some(indices), palette: Some(palette), palette_row: row, ..default() }
    }
}

#[derive(Clone, Default, ShaderType)]
pub struct Psx2dUniform {
    pub color: Vec4,
    pub flags: u32,
    pub palette_row: u32,
}

fn flag(on: bool, bit: u32) -> u32 {
    if on { bit } else { 0 }
}

impl AsBindGroupShaderType<Psx2dUniform> for PsxMaterial2d {
    fn as_bind_group_shader_type(&self, _: &RenderAssets<GpuImage>) -> Psx2dUniform {
        let c = self.color.to_srgba();
        Psx2dUniform {
            color: Vec4::new(c.red, c.green, c.blue, c.alpha),
            flags: flag(self.texture.is_some(), TEXTURED)
                | flag(self.palette.is_some(), PALETTE)
                | flag(self.look.dither && self.dither, DITHER)
                | blend_flags(self.blend, self.stp),
            palette_row: self.palette_row,
        }
    }
}

/// Pipeline key for the PSX 2D materials: the blend mode they draw with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect)]
pub struct PsxBlendKey {
    blend: PsxBlend,
}

impl From<&PsxMaterial2d> for PsxBlendKey {
    fn from(m: &PsxMaterial2d) -> Self {
        Self { blend: drawn_blend(m.blend, m.stp) }
    }
}

impl From<&PsxSprite> for PsxBlendKey {
    fn from(m: &PsxSprite) -> Self {
        Self { blend: drawn_blend(m.blend, m.stp) }
    }
}

fn alpha_mode(blend: PsxBlend) -> AlphaMode2d {
    // Blended surfaces go through the z-sorted transparent pass; the equation
    // is set in `specialize`.
    match blend {
        PsxBlend::Opaque => AlphaMode2d::Mask(ALPHA_TRANSPARENT),
        _ => AlphaMode2d::Blend,
    }
}

impl Material2d for PsxMaterial2d {
    fn fragment_shader() -> ShaderRef {
        MATERIAL2D_SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        alpha_mode(drawn_blend(self.blend, self.stp))
    }

    fn specialize(
        _: &Material2dPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        set_blend(descriptor, key.bind_group_data.blend);
        Ok(())
    }
}

/// PS1 semi-transparency and palettes for a `Sprite`:
/// `(Sprite::from_image(..), SpriteMaterial(psx_sprites.add(PsxSprite { .. })))`.
/// Texels with alpha below 0.25 are transparent. The sprite's `color`
/// modulates in gamma space like the GPU (white = unchanged).
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug, Default)]
#[uniform(21, PsxSpriteUniform)]
#[bind_group_data(PsxBlendKey)]
pub struct PsxSprite {
    /// Palette image (one palette per row); the sprite's image then holds
    /// 8-bit indices (see [`load_indices`]).
    #[texture(20)]
    pub palette: Option<Handle<Image>>,
    pub palette_row: u32,
    pub blend: PsxBlend,
    pub stp: PsxStp,
}

impl PsxSprite {
    pub fn blend(blend: PsxBlend) -> Self {
        Self { blend, ..default() }
    }
    /// The sprite's image (indices) drawn through row `row` of `palette`.
    pub fn indexed(palette: Handle<Image>, row: u32) -> Self {
        Self { palette: Some(palette), palette_row: row, ..default() }
    }
}

#[derive(Clone, Default, ShaderType)]
pub struct PsxSpriteUniform {
    pub flags: u32,
    pub palette_row: u32,
}

impl AsBindGroupShaderType<PsxSpriteUniform> for PsxSprite {
    fn as_bind_group_shader_type(&self, _: &RenderAssets<GpuImage>) -> PsxSpriteUniform {
        PsxSpriteUniform {
            flags: TEXTURED | flag(self.palette.is_some(), PALETTE) | blend_flags(self.blend, self.stp),
            palette_row: self.palette_row,
        }
    }
}

impl MaterialExtension2d for PsxSprite {
    fn fragment_shader() -> Option<ShaderRef> {
        Some(SPRITE_SHADER.into())
    }

    fn alpha_mode(&self) -> Option<AlphaMode2d> {
        Some(alpha_mode(drawn_blend(self.blend, self.stp)))
    }

    fn specialize(
        _: &Material2dPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        set_blend(descriptor, key.bind_group_data.blend);
        Ok(())
    }
}

/// A 15-bit PS1 colour from 5-bit components (0-31), without the STP bit.
pub const fn rgb15(r: u8, g: u8, b: u8) -> u16 {
    (r as u16 & 31) | (g as u16 & 31) << 5 | (b as u16 & 31) << 10
}

/// Palette image from 15-bit PS1 colours (bit 15 = STP), one palette per
/// row, as wide as the longest row. 0000h entries are transparent.
pub fn psx_palette(rows: &[&[u16]]) -> Image {
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
    let mut data = Vec::with_capacity(width * rows.len() * 4);
    for row in rows {
        for i in 0..width {
            data.extend_from_slice(&rgba_of(row.get(i).copied().unwrap_or(0)));
        }
    }
    let mut image = Image::new(
        Extent3d { width: width as u32, height: rows.len().max(1) as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// RGBA8 (sRGB) for a 15-bit colour, alpha as `psx textures` writes it.
pub fn rgba_of(c: u16) -> [u8; 4] {
    let e = |v: u16| ((v & 31) << 3 | (v & 31) >> 2) as u8;
    let alpha = match c {
        0 => 0,
        c if c & 0x8000 != 0 => 128,
        _ => 255,
    };
    [e(c), e(c >> 5), e(c >> 10), alpha]
}

/// An 8-bit index image (`R8Unorm`, nearest-sampled) for a palette material.
pub fn psx_indices(width: u32, height: u32, indices: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        indices,
        TextureFormat::R8Unorm,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Loads an 8-bit grayscale PNG of palette indices (such as
/// `page_*_index.png` from `psx textures`) as raw `R8Unorm` values. A plain
/// `load` would decode it as sRGB colour and scramble the indices.
pub fn load_indices(assets: &AssetServer, path: impl Into<String>) -> Handle<Image> {
    assets
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| {
            s.expand_grayscale = false;
            s.is_srgb = false;
            s.sampler = ImageSampler::nearest();
        })
        .load(path.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_alpha_follows_ps1_rules() {
        assert_eq!(rgba_of(0), [0, 0, 0, 0]);
        assert_eq!(rgba_of(0x8000), [0, 0, 0, 128]);
        assert_eq!(rgba_of(rgb15(31, 0, 16)), [255, 0, 132, 255]);
        let image = psx_palette(&[&[0, 1], &[2]]);
        assert_eq!((image.width(), image.height()), (2, 2));
    }
}

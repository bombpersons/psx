//! Original PlayStation files as ordinary Bevy assets. Extract them with the
//! psx toolkit (`psx assets extract`), drop them under `assets/`, and load
//! them through `AssetServer`:
//!
//! - `.tim` -> `Image`: RGBA8 sRGB, nearest sampling, colour 0x0000
//!   transparent (alpha 0, so [`PsxMaterial::cutout`] and sprites drop it).
//!   The asset is palette 0 (or [`TimSettings::palette`]); every other
//!   palette is a labeled image, `"enemy.tim#Palette1"`.
//! - `.tmd` -> [`PsxModel`]: one `Mesh` per TMD object and material
//!   (`"car.tmd#Object0/Part0"`), with texture pages decoded the way the
//!   GPU would read them from VRAM after uploading the TIMs in
//!   [`TmdSettings::textures`] (default: a `.tim` next to the `.tmd` with the
//!   same name). Spawn it with [`PsxModelRoot`].
//!
//! Axes: PS1 model space is y-down; models are rotated 180 degrees about x so
//! they stand up in Bevy's y-up space, and scaled by
//! [`TmdSettings::units_per_metre`].

use std::collections::BTreeMap;

use bevy::{
    asset::{AssetLoader, LoadContext, RenderAssetUsages, io::Reader},
    image::ImageSampler,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use serde::{Deserialize, Serialize};

use super::formats::tim::{TexPage, Tim, TimAlpha, Vram, clut_from_cba};
use super::formats::tmd::{MODE_TGE, Tmd, TmdObject, TmdPolygon};
use super::{PsxBlend, PsxMaterial, scene::VIEW_UNITS_PER_METRE};

#[derive(Debug)]
pub struct PsxAssetError(String);

impl std::fmt::Display for PsxAssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PsxAssetError {}

/// How TIM colours become alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimTransparency {
    /// 0x0000 transparent, everything else opaque (textures on opaque polygons, sprites).
    #[default]
    Cutout,
    /// Also gives colours with the STP bit alpha 0.5 (texels on semi-transparent polygons).
    SemiStp,
    /// Every texel opaque.
    Opaque,
}

impl From<TimTransparency> for TimAlpha {
    fn from(t: TimTransparency) -> TimAlpha {
        match t {
            TimTransparency::Cutout => TimAlpha::Cutout,
            TimTransparency::SemiStp => TimAlpha::SemiStp,
            TimTransparency::Opaque => TimAlpha::Opaque,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TimSettings {
    /// Palette (CLUT row) used for the main image of 4/8-bit TIMs.
    pub palette: usize,
    pub transparency: TimTransparency,
}

/// An sRGB RGBA8 image with nearest sampling, as PS1 texels were drawn.
pub fn rgba_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    image
}

async fn read_all(reader: &mut dyn Reader) -> Result<Vec<u8>, PsxAssetError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await.map_err(|e| PsxAssetError(e.to_string()))?;
    Ok(bytes)
}

#[derive(Default, TypePath)]
pub struct TimLoader;

impl AssetLoader for TimLoader {
    type Asset = Image;
    type Settings = TimSettings;
    type Error = PsxAssetError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &TimSettings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Image, PsxAssetError> {
        let bytes = read_all(reader).await?;
        let tim = Tim::parse(&bytes).map_err(|e| PsxAssetError(format!("{}: {e}", load_context.path())))?;
        let alpha = TimAlpha::from(settings.transparency);
        for p in 1..tim.palette_count() {
            load_context
                .add_labeled_asset(format!("Palette{p}"), rgba_image(tim.width(), tim.height(), tim.to_rgba(p, alpha)));
        }
        Ok(rgba_image(tim.width(), tim.height(), tim.to_rgba(settings.palette, alpha)))
    }

    fn extensions(&self) -> &[&str] {
        &["tim"]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TmdSettings {
    /// Files (paths relative to the `.tmd`) whose TIMs are uploaded to a
    /// virtual VRAM in order, later ones overwriting earlier ones: a `.tim`,
    /// or any file with TIMs inside (e.g. the game archive the model came from). Texture pages and CLUTs are
    /// read from it at the positions the model's TSB/CBA fields name. Empty:
    /// use `<model>.tim` if it exists.
    pub textures: Vec<String>,
    /// TMD units per Bevy unit.
    pub units_per_metre: f32,
    /// Draw polygons without the TMD double-sided flag front faces only
    /// (screen-clockwise on the PS1, like `GsSortObject` with back-face
    /// clipping). Default: everything double-sided, since culling was a
    /// per-game choice.
    pub cull_back_faces: bool,
}

impl Default for TmdSettings {
    fn default() -> Self {
        Self { textures: Vec::new(), units_per_metre: VIEW_UNITS_PER_METRE, cull_back_faces: false }
    }
}

/// One mesh with one material.
#[derive(Clone, Debug)]
pub struct PsxModelPart {
    pub mesh: Handle<Mesh>,
    /// Decoded texture page (256x256 texels), when the model is textured and
    /// a supplied TIM covered that VRAM.
    pub texture: Option<Handle<Image>>,
    pub blend: PsxBlend,
    /// Has normals and expects scene lights (the TMD's light-source calculation flag).
    pub lit: bool,
    pub double_sided: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PsxModelObject {
    pub parts: Vec<PsxModelPart>,
}

/// A loaded `.tmd`.
#[derive(Asset, TypePath, Debug)]
pub struct PsxModel {
    /// TMD objects in file order. Games position them (often as body parts).
    pub objects: Vec<PsxModelObject>,
    /// Bevy-space bounding box `[min, max]` of every vertex used.
    pub bounds: [Vec3; 2],
    /// Textured polygons whose page or CLUT no supplied TIM covered (drawn untextured).
    pub untextured: usize,
}

/// Spawns a [`PsxModel`]'s objects as children (with [`PsxModelObjectIndex`])
/// and their parts as `Mesh3d` + [`PsxMaterial`] grandchildren once it loads.
#[derive(Component, Clone, Debug)]
pub struct PsxModelRoot(pub Handle<PsxModel>);

/// Which TMD object a spawned child is.
#[derive(Component, Clone, Copy, Debug)]
pub struct PsxModelObjectIndex(pub usize);

#[derive(Component)]
struct ModelSpawned;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TexKey {
    page: TexPage,
    clut: (u16, u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PartKey {
    tex: Option<TexKey>,
    /// 0: opaque, 1-4: semi-transparency ABR + 1.
    blend: u8,
    lit: bool,
    double_sided: bool,
}

fn tex_key(p: &TmdPolygon) -> Option<TexKey> {
    if !p.textured() {
        return None;
    }
    let page = TexPage::from_tsb(p.tsb);
    let clut = if page.bits == 16 { (0, 0) } else { clut_from_cba(p.cba) };
    Some(TexKey { page: TexPage { abr: 0, ..page }, clut })
}

#[derive(Default)]
struct PartData {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
}

fn add_polygon(data: &mut PartData, obj: &TmdObject, p: &TmdPolygon, scale: f32, textured: bool, flip: bool) {
    for tri in p.triangles() {
        // PS1 front faces are clockwise on screen; after the 180-degree turn
        // about x that is clockwise in Bevy too, so swap to counter-clockwise.
        let order = if flip { [tri[0], tri[2], tri[1]] } else { *tri };
        for i in order {
            let v = obj.vertices[p.vertices[i] as usize];
            data.positions.push([v[0] as f32 * scale, -(v[1] as f32) * scale, -(v[2] as f32) * scale]);
            let n = p
                .normal(i)
                .and_then(|n| obj.normals.get(n as usize))
                .map_or(Vec3::Y, |n| Vec3::new(n[0] as f32, -(n[1] as f32), -(n[2] as f32)).normalize_or(Vec3::Y));
            data.normals.push(n.to_array());
            data.uvs.push(p.uvs.get(i).map_or([0.0; 2], |t| [t[0] as f32 / 256.0, t[1] as f32 / 256.0]));
            // Texture modulation: 128 is neutral (up to 2x); flat colours are /255.
            let c = p.color(i);
            let rgb = if p.textured() {
                if p.colors.is_empty() || p.mode & MODE_TGE != 0 { [1.0; 3] } else { c.map(|x| x as f32 / 128.0) }
            } else {
                c.map(|x| x as f32 / 255.0)
            };
            // A textured polygon drawn without its texture shows grey, not white.
            let rgb = if p.textured() && !textured { rgb.map(|x| x * 0.5) } else { rgb };
            data.colors.push(Color::srgb(rgb[0], rgb[1], rgb[2]).to_linear().to_f32_array());
        }
    }
}

#[derive(Default, TypePath)]
pub struct TmdLoader;

impl AssetLoader for TmdLoader {
    type Asset = PsxModel;
    type Settings = TmdSettings;
    type Error = PsxAssetError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &TmdSettings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<PsxModel, PsxAssetError> {
        let bytes = read_all(reader).await?;
        let tmd = Tmd::parse(&bytes).map_err(|e| PsxAssetError(format!("{}: {e}", load_context.path())))?;

        let mut vram = Vram::default();
        let explicit = !settings.textures.is_empty();
        let names = if explicit {
            settings.textures.clone()
        } else {
            let file = load_context.path().path().file_stem().map(|s| format!("{}.tim", s.to_string_lossy()));
            file.into_iter().collect()
        };
        for name in names {
            let path = load_context.path().resolve_embed_str(&name).map_err(|e| PsxAssetError(e.to_string()))?;
            match load_context.read_asset_bytes(path.clone()).await {
                Ok(b) => {
                    let tims = Tim::find_all(&b);
                    if tims.is_empty() {
                        return Err(PsxAssetError(format!("{path}: no TIM found")));
                    }
                    for (_, t) in &tims {
                        vram.load_tim(t);
                    }
                }
                Err(e) if explicit => return Err(PsxAssetError(format!("{path}: {e}"))),
                Err(_) => {}
            }
        }

        let scale = 1.0 / settings.units_per_metre.max(f32::EPSILON);
        let mut pages: BTreeMap<TexKey, Option<Handle<Image>>> = BTreeMap::new();
        let mut model = PsxModel { objects: Vec::new(), bounds: [Vec3::MAX, Vec3::MIN], untextured: 0 };
        for (k, obj) in tmd.objects.iter().enumerate() {
            let mut parts: BTreeMap<PartKey, PartData> = BTreeMap::new();
            for p in &obj.polygons {
                let tex = tex_key(p).filter(|key| {
                    pages
                        .entry(*key)
                        .or_insert_with(|| {
                            let (rgba, covered) = vram.decode_page(key.page, key.clut, TimAlpha::Cutout);
                            (covered > 0.0).then(|| {
                                let label = format!(
                                    "Page{}_{}_{}b_Clut{}_{}",
                                    key.page.x, key.page.y, key.page.bits, key.clut.0, key.clut.1
                                );
                                load_context.add_labeled_asset(label, rgba_image(256, 256, rgba))
                            })
                        })
                        .is_some()
                });
                if p.textured() && tex.is_none() {
                    model.untextured += 1;
                }
                let blend = if p.semi_transparent() {
                    1 + if p.textured() { TexPage::from_tsb(p.tsb).abr } else { 0 }
                } else {
                    0
                };
                let double_sided = !settings.cull_back_faces || p.double_sided();
                let key = PartKey { tex, blend, lit: p.lit(), double_sided };
                add_polygon(parts.entry(key).or_default(), obj, p, scale, tex.is_some(), !double_sided);
            }
            let mut object = PsxModelObject::default();
            for (j, (key, data)) in parts.into_iter().enumerate() {
                for p in &data.positions {
                    model.bounds[0] = model.bounds[0].min(Vec3::from_array(*p));
                    model.bounds[1] = model.bounds[1].max(Vec3::from_array(*p));
                }
                let indices = (0..data.positions.len() as u32).collect();
                let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, data.positions)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, data.uvs)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, data.colors)
                    .with_inserted_indices(Indices::U32(indices));
                if key.lit {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals);
                }
                object.parts.push(PsxModelPart {
                    mesh: load_context.add_labeled_asset(format!("Object{k}/Part{j}"), mesh),
                    texture: key.tex.and_then(|t| pages[&t].clone()),
                    blend: match key.blend {
                        0 => PsxBlend::Opaque,
                        1 => PsxBlend::Half,
                        2 => PsxBlend::Add,
                        3 => PsxBlend::Subtract,
                        _ => PsxBlend::AddQuarter,
                    },
                    lit: key.lit,
                    double_sided: key.double_sided,
                });
            }
            model.objects.push(object);
        }
        if model.bounds[0].x > model.bounds[1].x {
            model.bounds = [Vec3::ZERO; 2];
        }
        Ok(model)
    }

    fn extensions(&self) -> &[&str] {
        &["tmd"]
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_asset::<PsxModel>()
        .init_asset_loader::<TimLoader>()
        .init_asset_loader::<TmdLoader>()
        .add_systems(Update, spawn_models);
}

fn spawn_models(
    mut commands: Commands,
    roots: Query<(Entity, &PsxModelRoot), Without<ModelSpawned>>,
    models: Res<Assets<PsxModel>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for (root, PsxModelRoot(handle)) in &roots {
        let Some(model) = models.get(handle) else { continue };
        commands.entity(root).insert(ModelSpawned);
        for (k, object) in model.objects.iter().enumerate() {
            let child = commands
                .spawn((
                    Name::new(format!("Object{k}")),
                    PsxModelObjectIndex(k),
                    Transform::default(),
                    Visibility::default(),
                    ChildOf(root),
                ))
                .id();
            for part in &object.parts {
                let material = PsxMaterial {
                    texture: part.texture.clone(),
                    lit: part.lit,
                    double_sided: part.double_sided,
                    blend: part.blend,
                    ..default()
                };
                commands.spawn((
                    Mesh3d(part.mesh.clone()),
                    MeshMaterial3d(materials.add(material)),
                    Transform::default(),
                    ChildOf(child),
                ));
            }
        }
    }
}

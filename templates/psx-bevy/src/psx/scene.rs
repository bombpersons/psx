//! Captured 2D frames as ordinary Bevy entities.
//!
//! The psx toolkit's `psx run --gpu-frame N` + `psx textures` turn one frame
//! of an original game into `scene.psxscene` (JSON) plus decoded texture page
//! PNGs. Loading that file and adding [`PsxSceneRoot`] spawns one child per
//! element, in the game's draw order:
//!
//! - rects and axis-aligned quads become `Sprite`s (`Sprite::rect` selects the
//!   texels, flips carried over);
//! - polygons the capture lifted back to 3D (GTE view space) become `Mesh3d`
//!   with [`PsxMaterial`] under a child group in Bevy space, seen by a
//!   [`PsxSceneCamera`] given the game's field of view;
//! - other polygons become `Mesh2d` triangles with UVs and vertex colours
//!   ([`PsxMaterial2d`]).
//!
//! Every child has a [`PsxSceneElement`] naming the GPU command it came from,
//! so a reimplementation can start from a pixel-faithful reference layout,
//! then replace pieces with real game entities while keeping the evidence
//! trail. Semi-transparent elements use the game's blend equation; in format
//! 2 scenes only their STP texels blend (image alpha ≈ 0.5), as on the GPU.
//! Semi-transparent sprites carry `SpriteMaterial<PsxSprite>`. A `Subtract`
//! element with STP texels spawns twice: the subtracting texels and, with
//! [`PsxStp::OpaqueOnly`], the opaque ones.
//!
//! The PS1 had no depth buffer: it painted polygons in command order. 3D
//! elements are depth-tested by default, which is right for exploring the
//! geometry from new angles; [`PsxSceneDepth::Painter`] instead draws them in
//! capture order without depth, reproducing the captured frame exactly.

use bevy::{
    asset::{AssetLoader, LoadContext, RenderAssetUsages, io::Reader},
    camera::visibility::RenderLayers,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    sprite::SpriteAlphaMode,
};
use serde::Deserialize;

use super::{PsxBlend, PsxMaterial, PsxMaterial2d, PsxSprite, PsxStp, material::ALPHA_TRANSPARENT};

/// In scenes with 3D geometry, 2D elements drawn before the first 3D element
/// (clears, skies) go on this layer, for a camera behind the 3D one.
pub const SCENE_BACKGROUND_LAYER: usize = 30;

/// PS1 view-space units per Bevy unit for lifted 3D geometry.
pub const VIEW_UNITS_PER_METRE: f32 = 64.0;

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct SceneVertex3d {
    /// GTE view space: x right, y down, z forward (into the screen).
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub u: f32,
    pub v: f32,
    pub color: [f32; 3],
}

/// The game's perspective: `sx = ofx + h·x/z`, `sy = ofy + h·y/z` (screen pixels).
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct SceneProjection {
    pub h: f32,
    pub ofx: f32,
    pub ofy: f32,
}

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct SceneVertex {
    pub x: f32,
    pub y: f32,
    pub u: f32,
    pub v: f32,
    pub color: [f32; 3],
}

/// One drawn element, in screen pixels (origin top-left, y down) as captured.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SceneElement {
    Sprite {
        image: Option<usize>,
        /// Screen `[x, y, w, h]`.
        rect: [f32; 4],
        /// Texel `[u, v, w, h]` within the image.
        uv: Option<[f32; 4]>,
        #[serde(default)]
        flip_x: bool,
        #[serde(default)]
        flip_y: bool,
        /// Texture modulation (1.0 = unchanged) or flat colour (untextured).
        color: [f32; 3],
        #[serde(default)]
        semi: bool,
        /// Semi-transparency equation 0-3 when `semi`.
        #[serde(default)]
        blend: Option<u8>,
        command: u32,
    },
    Mesh {
        image: Option<usize>,
        vertices: Vec<SceneVertex>,
        triangles: Vec<[u32; 3]>,
        #[serde(default)]
        semi: bool,
        #[serde(default)]
        blend: Option<u8>,
        /// The GPU dithers it (format 2; earlier scenes: assumed).
        #[serde(default = "yes")]
        dither: bool,
        command: u32,
    },
    Mesh3d {
        image: Option<usize>,
        vertices: Vec<SceneVertex3d>,
        triangles: Vec<[u32; 3]>,
        #[serde(default)]
        semi: bool,
        #[serde(default)]
        blend: Option<u8>,
        #[serde(default = "yes")]
        dither: bool,
        command: u32,
    },
}

fn yes() -> bool {
    true
}

impl SceneElement {
    /// The GPU's semi-transparency equation for this element.
    pub fn blend(&self) -> PsxBlend {
        let (semi, blend) = match self {
            SceneElement::Sprite { semi, blend, .. }
            | SceneElement::Mesh { semi, blend, .. }
            | SceneElement::Mesh3d { semi, blend, .. } => (*semi, *blend),
        };
        match (semi, blend.unwrap_or(0)) {
            (false, _) => PsxBlend::Opaque,
            (true, 0) => PsxBlend::Half,
            (true, 1) => PsxBlend::Add,
            (true, 2) => PsxBlend::Subtract,
            (true, _) => PsxBlend::AddQuarter,
        }
    }
}

/// Oldest and newest `.psxscene` formats this loader reads. Format 2 marks
/// STP texels with image alpha ≈ 0.5; format 1 images had only 0 and 1.
pub const SCENE_FORMATS: std::ops::RangeInclusive<u32> = 1..=2;

#[derive(Deserialize)]
struct SceneFile {
    format: u32,
    screen: [f32; 2],
    #[serde(default)]
    projection: Option<SceneProjection>,
    images: Vec<String>,
    elements: Vec<SceneElement>,
}

/// A loaded `.psxscene`.
#[derive(Asset, TypePath, Debug)]
pub struct PsxScene {
    /// Size of the captured display, in pixels.
    pub screen: Vec2,
    pub projection: Option<SceneProjection>,
    /// Texture pages, loaded relative to the scene file.
    pub images: Vec<Handle<Image>>,
    pub elements: Vec<SceneElement>,
    /// Images mark STP texels (format 2+), so semi-transparency is per texel.
    pub texel_stp: bool,
}

/// Spawns the scene's elements as children once it has loaded. The scene's
/// screen centre sits at this entity's origin.
#[derive(Component, Clone, Debug)]
#[require(PsxSceneDepth)]
pub struct PsxSceneRoot(pub Handle<PsxScene>);

/// How a [`PsxSceneRoot`]'s lifted 3D elements are ordered. Can be changed at
/// any time.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
#[reflect(Component)]
pub enum PsxSceneDepth {
    /// Bevy's depth buffer decides visibility: geometry looks right from any
    /// camera, but polygons the game drew first and let later ones paint over
    /// (backdrops placed near the camera) can show in front.
    #[default]
    Tested,
    /// Painter's order, as the PS1 drew: each element draws over the earlier
    /// ones, without depth (`PsxMaterial::draw_order` = element index).
    /// Matches the captured frame from the game's camera.
    Painter,
}

/// Which captured element (and GPU command, `gpu_frame.json` index) an entity came from.
#[derive(Component, Clone, Copy, Debug)]
pub struct PsxSceneElement {
    pub element: usize,
    pub command: u32,
}

#[derive(Component)]
struct Spawned;

/// A `Camera3d` given the loaded scene's field of view (at the origin, looking
/// down -Z, which is the game's camera after the view transform).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PsxSceneCamera;

impl PsxScene {
    /// The game's vertical field of view, when the scene has 3D geometry.
    pub fn fov_y(&self) -> Option<f32> {
        self.projection.map(|p| 2.0 * ((self.screen.y / 2.0) / p.h.max(1.0)).atan())
    }

    /// Horizontal scale that shows the game's pixels as on a 4:3 TV.
    pub fn pixel_aspect(&self) -> f32 {
        (4.0 / 3.0) / (self.screen.x / self.screen.y)
    }

    /// Transform from GTE view space into Bevy space: y and z flipped, scaled
    /// to metres, and x scaled by [`Self::pixel_aspect`].
    pub fn view_transform(&self) -> Transform {
        let s = 1.0 / VIEW_UNITS_PER_METRE;
        Transform::from_scale(Vec3::new(s * self.pixel_aspect(), -s, -s))
    }
}

#[derive(Debug)]
pub struct SceneLoadError(String);

impl std::fmt::Display for SceneLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "psxscene: {}", self.0)
    }
}

impl std::error::Error for SceneLoadError {}

#[derive(Default, TypePath)]
pub struct PsxSceneLoader;

impl AssetLoader for PsxSceneLoader {
    type Asset = PsxScene;
    type Settings = ();
    type Error = SceneLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<PsxScene, SceneLoadError> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.map_err(|e| SceneLoadError(e.to_string()))?;
        let file: SceneFile = serde_json::from_slice(&bytes).map_err(|e| SceneLoadError(e.to_string()))?;
        if !SCENE_FORMATS.contains(&file.format) {
            return Err(SceneLoadError(format!(
                "unsupported format {} (this template reads {}-{}; update src/psx from a newer template)",
                file.format,
                SCENE_FORMATS.start(),
                SCENE_FORMATS.end()
            )));
        }
        let mut images = Vec::new();
        for name in &file.images {
            let path = load_context.path().resolve_embed_str(name).map_err(|e| SceneLoadError(e.to_string()))?;
            images.push(load_context.load(path));
        }
        Ok(PsxScene {
            screen: Vec2::from(file.screen),
            projection: file.projection,
            images,
            elements: file.elements,
            texel_stp: file.format >= 2,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["psxscene"]
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_asset::<PsxScene>()
        .init_asset_loader::<PsxSceneLoader>()
        .register_type::<PsxSceneDepth>()
        .add_systems(Update, ((spawn_scenes, apply_scene_depth).chain(), fit_scene_cameras));
}

/// Spawned roots whose depth mode is new or changed.
type DepthChanged = (With<Spawned>, Or<(Changed<PsxSceneDepth>, Added<Spawned>)>);

/// Sets each lifted element's [`PsxMaterial::draw_order`] from its root's
/// [`PsxSceneDepth`], when the scene spawns or the mode changes.
fn apply_scene_depth(
    roots: Query<(Entity, &PsxSceneDepth), DepthChanged>,
    children: Query<&Children>,
    elements: Query<(&PsxSceneElement, &MeshMaterial3d<PsxMaterial>)>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for (root, depth) in &roots {
        for entity in children.iter_descendants(root) {
            let Ok((element, material)) = elements.get(entity) else { continue };
            let order = (*depth == PsxSceneDepth::Painter).then_some(element.element as u32);
            if materials.get(&material.0).is_some_and(|m| m.draw_order != order)
                && let Some(mut m) = materials.get_mut(&material.0)
            {
                m.draw_order = order;
            }
        }
    }
}

fn fit_scene_cameras(
    roots: Query<&PsxSceneRoot>,
    scenes: Res<Assets<PsxScene>>,
    mut cameras: Query<&mut Projection, With<PsxSceneCamera>>,
) {
    let Some(fov) = roots.iter().find_map(|r| scenes.get(&r.0).and_then(PsxScene::fov_y)) else { return };
    for mut projection in &mut cameras {
        if let Projection::Perspective(p) = &mut *projection
            && p.fov != fov
        {
            p.fov = fov;
            p.far = 2000.0;
        }
    }
}

/// Captured screen position (y down, top-left origin) to Bevy 2D (y up, centred).
fn to_world(screen: Vec2, x: f32, y: f32) -> Vec2 {
    Vec2::new(x - screen.x / 2.0, screen.y / 2.0 - y)
}

/// The draws an element needs: one, or for `Subtract` with STP texels, the
/// subtracting texels plus the opaque ones (one draw cannot do both).
fn stp_passes(blend: PsxBlend, stp: PsxStp) -> &'static [PsxStp] {
    match (blend, stp) {
        (PsxBlend::Opaque, _) => &[PsxStp::All],
        (PsxBlend::Subtract, PsxStp::Texel) => &[PsxStp::Texel, PsxStp::OpaqueOnly],
        (_, PsxStp::Texel) => &[PsxStp::Texel],
        _ => &[PsxStp::All],
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_scenes(
    mut commands: Commands,
    roots: Query<(Entity, &PsxSceneRoot), Without<Spawned>>,
    scenes: Res<Assets<PsxScene>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial2d>>,
    mut psx_materials: ResMut<Assets<PsxMaterial>>,
    mut psx_sprites: ResMut<Assets<PsxSprite>>,
) {
    for (root, PsxSceneRoot(handle)) in &roots {
        let Some(scene) = scenes.get(handle) else { continue };
        commands.entity(root).insert(Spawned);
        // Lifted 3D geometry lives in a child group carrying the view transform.
        let group3d = commands.spawn((scene.view_transform(), Visibility::default(), ChildOf(root))).id();
        let first3d = scene.elements.iter().position(|e| matches!(e, SceneElement::Mesh3d { .. }));
        let background = |i: usize| first3d.is_some_and(|f| i < f);
        // With 3D geometry, 2D elements take the same TV aspect so they line up.
        let sx = if first3d.is_some() { scene.pixel_aspect() } else { 1.0 };
        let place = |x: f32, y: f32| {
            let p = to_world(scene.screen, x, y);
            Vec2::new(p.x * sx, p.y)
        };
        // Per-texel STP needs a texture whose alpha marks it (format 2);
        // untextured semi-transparent elements blend whole.
        let stp = |image: &Option<usize>| if scene.texel_stp && image.is_some() { PsxStp::Texel } else { PsxStp::All };
        for (i, element) in scene.elements.iter().enumerate() {
            // Later commands draw on top.
            let z = i as f32 * 0.01;
            let tag = |command: &u32| PsxSceneElement { element: i, command: *command };
            let layer =
                if background(i) { RenderLayers::layer(SCENE_BACKGROUND_LAYER) } else { RenderLayers::default() };
            let blend = element.blend();
            match element {
                SceneElement::Sprite { image, rect, uv, flip_x, flip_y, color, command, .. } => {
                    let [x, y, w, h] = *rect;
                    let tint = Color::srgb(color[0], color[1], color[2]);
                    let mut sprite = match image {
                        Some(i) => Sprite {
                            image: scene.images[*i].clone(),
                            rect: uv.map(|[u, v, uw, vh]| Rect::new(u, v, u + uw, v + vh)),
                            color: tint,
                            // Texel 0000h is transparent; STP texels of an opaque sprite are opaque.
                            alpha_mode: SpriteAlphaMode::Mask(ALPHA_TRANSPARENT),
                            ..default()
                        },
                        None => Sprite::from_color(tint, Vec2::ONE),
                    };
                    sprite.custom_size = Some(Vec2::new(w * sx, h));
                    sprite.flip_x = *flip_x;
                    sprite.flip_y = *flip_y;
                    let transform = Transform::from_translation(place(x + w / 2.0, y + h / 2.0).extend(z));
                    if blend == PsxBlend::Opaque {
                        commands.spawn((sprite, transform, tag(command), layer, ChildOf(root)));
                        continue;
                    }
                    for &pass in stp_passes(blend, stp(image)) {
                        let material = SpriteMaterial(psx_sprites.add(PsxSprite { blend, stp: pass, ..default() }));
                        commands.spawn((
                            sprite.clone(),
                            material,
                            transform,
                            tag(command),
                            layer.clone(),
                            ChildOf(root),
                        ));
                    }
                }
                SceneElement::Mesh { image, vertices, triangles, dither, command, .. } => {
                    let positions: Vec<[f32; 3]> =
                        vertices.iter().map(|v| place(v.x, v.y).extend(0.0).to_array()).collect();
                    let uvs: Vec<[f32; 2]> = vertices.iter().map(|v| [v.u / 256.0, v.v / 256.0]).collect();
                    let colors: Vec<[f32; 4]> = vertices
                        .iter()
                        .map(|v| Color::srgb(v.color[0], v.color[1], v.color[2]).to_linear().to_f32_array())
                        .collect();
                    // Counter-clockwise in Bevy's y-up space, whatever the capture's winding.
                    let mut indices = Vec::new();
                    for t in triangles {
                        let [a, b, c] = t.map(|k| Vec2::from_slice(&positions[k as usize][..2]));
                        let ccw = (b - a).perp_dot(c - a) >= 0.0;
                        indices.extend(if ccw { [t[0], t[1], t[2]] } else { [t[0], t[2], t[1]] });
                    }
                    let mesh = meshes.add(
                        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
                            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
                            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
                            .with_inserted_indices(Indices::U32(indices)),
                    );
                    for &pass in stp_passes(blend, stp(image)) {
                        let material = PsxMaterial2d {
                            texture: image.map(|i| scene.images[i].clone()),
                            blend,
                            stp: pass,
                            dither: *dither,
                            ..default()
                        };
                        commands.spawn((
                            Mesh2d(mesh.clone()),
                            MeshMaterial2d(materials.add(material)),
                            Transform::from_xyz(0.0, 0.0, z),
                            tag(command),
                            layer.clone(),
                            ChildOf(root),
                        ));
                    }
                }
                SceneElement::Mesh3d { image, vertices, triangles, dither, command, .. } => {
                    let positions: Vec<[f32; 3]> = vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
                    let uvs: Vec<[f32; 2]> = vertices.iter().map(|v| [v.u / 256.0, v.v / 256.0]).collect();
                    let colors: Vec<[f32; 4]> = vertices
                        .iter()
                        .map(|v| Color::srgb(v.color[0], v.color[1], v.color[2]).to_linear().to_f32_array())
                        .collect();
                    let indices: Vec<u32> = triangles.iter().flatten().copied().collect();
                    let mesh = meshes.add(
                        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
                            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
                            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
                            .with_inserted_indices(Indices::U32(indices)),
                    );
                    for &pass in stp_passes(blend, stp(image)) {
                        let material = PsxMaterial {
                            texture: image.map(|i| scene.images[i].clone()),
                            // Colours were lit by the game already; the PS1 drew both faces.
                            lit: false,
                            fog: false,
                            double_sided: true,
                            blend,
                            stp: pass,
                            dither: *dither,
                            ..default()
                        };
                        commands.spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(psx_materials.add(material)),
                            Transform::default(),
                            tag(command),
                            ChildOf(group3d),
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_format_one_and_two_elements() {
        let json = r#"[
            {"kind": "sprite", "image": 0, "rect": [0, 0, 8, 8], "uv": [0, 0, 8, 8], "color": [1, 1, 1], "semi": true, "blend": 2, "command": 4},
            {"kind": "mesh", "image": null, "vertices": [], "triangles": [], "semi": true, "command": 5},
            {"kind": "mesh3d", "image": 1, "vertices": [], "triangles": [], "semi": false, "blend": null, "dither": false, "command": 6}
        ]"#;
        let elements: Vec<SceneElement> = serde_json::from_str(json).unwrap();
        assert_eq!(elements[0].blend(), PsxBlend::Subtract);
        assert_eq!(elements[1].blend(), PsxBlend::Half, "format 1 meshes had no blend: B/2+F/2");
        assert!(matches!(elements[1], SceneElement::Mesh { dither: true, .. }), "dither defaults on");
        assert!(matches!(elements[2], SceneElement::Mesh3d { dither: false, .. }));
        assert_eq!(elements[2].blend(), PsxBlend::Opaque);
        assert_eq!(stp_passes(PsxBlend::Subtract, PsxStp::Texel), &[PsxStp::Texel, PsxStp::OpaqueOnly]);
        assert_eq!(stp_passes(PsxBlend::Half, PsxStp::All), &[PsxStp::All]);
    }
}

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
//! - other polygons become `Mesh2d` triangles with UVs and vertex colours.
//!
//! Every child has a [`PsxSceneElement`] naming the GPU command it came from,
//! so a reimplementation can start from a pixel-faithful reference layout,
//! then replace pieces with real game entities while keeping the evidence
//! trail. Semi-transparency is approximated with 50% alpha blending.
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
    sprite_render::AlphaMode2d,
};
use serde::Deserialize;

use super::{PsxBlend, PsxMaterial};

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
#[allow(dead_code)] // data for game code
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
        command: u32,
    },
    Mesh {
        image: Option<usize>,
        vertices: Vec<SceneVertex>,
        triangles: Vec<[u32; 3]>,
        #[serde(default)]
        semi: bool,
        command: u32,
    },
    Mesh3d {
        image: Option<usize>,
        vertices: Vec<SceneVertex3d>,
        triangles: Vec<[u32; 3]>,
        #[serde(default)]
        semi: bool,
        /// Semi-transparency equation 0-3 when `semi`.
        #[serde(default)]
        blend: Option<u8>,
        command: u32,
    },
}

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
#[allow(dead_code)] // read by game code
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

    async fn load(&self, reader: &mut dyn Reader, _: &(), load_context: &mut LoadContext<'_>) -> Result<PsxScene, SceneLoadError> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.map_err(|e| SceneLoadError(e.to_string()))?;
        let file: SceneFile = serde_json::from_slice(&bytes).map_err(|e| SceneLoadError(e.to_string()))?;
        if file.format != 1 {
            return Err(SceneLoadError(format!("unsupported format {}", file.format)));
        }
        let mut images = Vec::new();
        for name in &file.images {
            let path = load_context.path().resolve_embed_str(name).map_err(|e| SceneLoadError(e.to_string()))?;
            images.push(load_context.load(path));
        }
        Ok(PsxScene { screen: Vec2::from(file.screen), projection: file.projection, images, elements: file.elements })
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

fn spawn_scenes(
    mut commands: Commands,
    roots: Query<(Entity, &PsxSceneRoot), Without<Spawned>>,
    scenes: Res<Assets<PsxScene>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut psx_materials: ResMut<Assets<PsxMaterial>>,
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
        for (i, element) in scene.elements.iter().enumerate() {
            // Later commands draw on top.
            let z = i as f32 * 0.01;
            match element {
                SceneElement::Sprite { image, rect, uv, flip_x, flip_y, color, semi, command } => {
                    let [x, y, w, h] = *rect;
                    let alpha = if *semi { 0.5 } else { 1.0 };
                    let mut sprite = match image {
                        Some(i) => Sprite {
                            image: scene.images[*i].clone(),
                            rect: uv.map(|[u, v, uw, vh]| Rect::new(u, v, u + uw, v + vh)),
                            color: Color::srgba(color[0], color[1], color[2], alpha),
                            ..default()
                        },
                        None => Sprite::from_color(Color::srgba(color[0], color[1], color[2], alpha), Vec2::ONE),
                    };
                    sprite.custom_size = Some(Vec2::new(w * sx, h));
                    sprite.flip_x = *flip_x;
                    sprite.flip_y = *flip_y;
                    let centre = place(x + w / 2.0, y + h / 2.0);
                    let id = commands
                        .spawn((
                            sprite,
                            Transform::from_translation(centre.extend(z)),
                            PsxSceneElement { element: i, command: *command },
                            ChildOf(root),
                        ))
                        .id();
                    if background(i) {
                        commands.entity(id).insert(RenderLayers::layer(SCENE_BACKGROUND_LAYER));
                    }
                }
                SceneElement::Mesh { image, vertices, triangles, semi, command } => {
                    let positions: Vec<[f32; 3]> =
                        vertices.iter().map(|v| place(v.x, v.y).extend(0.0).to_array()).collect();
                    let uvs: Vec<[f32; 2]> = vertices.iter().map(|v| [v.u / 256.0, v.v / 256.0]).collect();
                    let alpha = if *semi { 0.5 } else { 1.0 };
                    let colors: Vec<[f32; 4]> = vertices
                        .iter()
                        .map(|v| Color::srgba(v.color[0], v.color[1], v.color[2], alpha).to_linear().to_f32_array())
                        .collect();
                    // Counter-clockwise in Bevy's y-up space, whatever the capture's winding.
                    let mut indices = Vec::new();
                    for t in triangles {
                        let [a, b, c] = t.map(|k| Vec2::from_slice(&positions[k as usize][..2]));
                        let ccw = (b - a).perp_dot(c - a) >= 0.0;
                        indices.extend(if ccw { [t[0], t[1], t[2]] } else { [t[0], t[2], t[1]] });
                    }
                    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
                        .with_inserted_indices(Indices::U32(indices));
                    let material = ColorMaterial {
                        texture: image.map(|i| scene.images[i].clone()),
                        // Transparent texels (0000h) are cut out; semi-transparent ones blend.
                        alpha_mode: if *semi {
                            AlphaMode2d::Blend
                        } else if image.is_some() {
                            AlphaMode2d::Mask(0.5)
                        } else {
                            AlphaMode2d::Opaque
                        },
                        ..default()
                    };
                    let id = commands
                        .spawn((
                            Mesh2d(meshes.add(mesh)),
                            MeshMaterial2d(materials.add(material)),
                            Transform::from_xyz(0.0, 0.0, z),
                            PsxSceneElement { element: i, command: *command },
                            ChildOf(root),
                        ))
                        .id();
                    if background(i) {
                        commands.entity(id).insert(RenderLayers::layer(SCENE_BACKGROUND_LAYER));
                    }
                }
                SceneElement::Mesh3d { image, vertices, triangles, semi, blend, command } => {
                    let positions: Vec<[f32; 3]> = vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
                    let uvs: Vec<[f32; 2]> = vertices.iter().map(|v| [v.u / 256.0, v.v / 256.0]).collect();
                    let colors: Vec<[f32; 4]> = vertices
                        .iter()
                        .map(|v| Color::srgb(v.color[0], v.color[1], v.color[2]).to_linear().to_f32_array())
                        .collect();
                    let indices: Vec<u32> = triangles.iter().flatten().copied().collect();
                    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
                        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
                        .with_inserted_indices(Indices::U32(indices));
                    let material = PsxMaterial {
                        texture: image.map(|i| scene.images[i].clone()),
                        // Colours were lit by the game already; the PS1 drew both faces.
                        lit: false,
                        fog: false,
                        double_sided: true,
                        blend: match (*semi, blend.unwrap_or(0)) {
                            (false, _) => PsxBlend::Opaque,
                            (true, 0) => PsxBlend::Half,
                            (true, 1) => PsxBlend::Add,
                            (true, 2) => PsxBlend::Subtract,
                            (true, _) => PsxBlend::AddQuarter,
                        },
                        ..default()
                    };
                    commands.spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(psx_materials.add(material)),
                        Transform::default(),
                        PsxSceneElement { element: i, command: *command },
                        ChildOf(group3d),
                    ));
                }
            }
        }
    }
}

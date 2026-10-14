//! Captured 2D frames as ordinary Bevy entities.
//!
//! The psx toolkit's `psx run --gpu-frame N` + `psx textures` turn one frame
//! of an original game into `scene.psxscene` (JSON) plus decoded texture page
//! PNGs. Loading that file and adding [`PsxSceneRoot`] spawns one child per
//! element, in the game's draw order:
//!
//! - rects and axis-aligned quads become `Sprite`s (`Sprite::rect` selects the
//!   texels, flips carried over);
//! - other polygons become `Mesh2d` triangles with UVs and vertex colours.
//!
//! Every child has a [`PsxSceneElement`] naming the GPU command it came from,
//! so a reimplementation can start from a pixel-faithful reference layout,
//! then replace pieces with real game entities while keeping the evidence
//! trail. Semi-transparency is approximated with 50% alpha blending.

use bevy::{
    asset::{AssetLoader, LoadContext, RenderAssetUsages, io::Reader},
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    sprite_render::AlphaMode2d,
};
use serde::Deserialize;

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
}

#[derive(Deserialize)]
struct SceneFile {
    format: u32,
    screen: [f32; 2],
    images: Vec<String>,
    elements: Vec<SceneElement>,
}

/// A loaded `.psxscene`.
#[derive(Asset, TypePath, Debug)]
pub struct PsxScene {
    /// Size of the captured display, in pixels.
    pub screen: Vec2,
    /// Texture pages, loaded relative to the scene file.
    pub images: Vec<Handle<Image>>,
    pub elements: Vec<SceneElement>,
}

/// Spawns the scene's elements as children once it has loaded. The scene's
/// screen centre sits at this entity's origin.
#[derive(Component, Clone, Debug)]
pub struct PsxSceneRoot(pub Handle<PsxScene>);

/// Which captured element (and GPU command, `gpu_frame.json` index) an entity came from.
#[allow(dead_code)] // read by game code
#[derive(Component, Clone, Copy, Debug)]
pub struct PsxSceneElement {
    pub element: usize,
    pub command: u32,
}

#[derive(Component)]
struct Spawned;

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
        Ok(PsxScene { screen: Vec2::from(file.screen), images, elements: file.elements })
    }

    fn extensions(&self) -> &[&str] {
        &["psxscene"]
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_asset::<PsxScene>().init_asset_loader::<PsxSceneLoader>().add_systems(Update, spawn_scenes);
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
) {
    for (root, PsxSceneRoot(handle)) in &roots {
        let Some(scene) = scenes.get(handle) else { continue };
        commands.entity(root).insert(Spawned);
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
                    sprite.custom_size = Some(Vec2::new(w, h));
                    sprite.flip_x = *flip_x;
                    sprite.flip_y = *flip_y;
                    let centre = to_world(scene.screen, x + w / 2.0, y + h / 2.0);
                    commands.spawn((
                        sprite,
                        Transform::from_translation(centre.extend(z)),
                        PsxSceneElement { element: i, command: *command },
                        ChildOf(root),
                    ));
                }
                SceneElement::Mesh { image, vertices, triangles, semi, command } => {
                    let positions: Vec<[f32; 3]> =
                        vertices.iter().map(|v| to_world(scene.screen, v.x, v.y).extend(0.0).to_array()).collect();
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
                    commands.spawn((
                        Mesh2d(meshes.add(mesh)),
                        MeshMaterial2d(materials.add(material)),
                        Transform::from_xyz(0.0, 0.0, z),
                        PsxSceneElement { element: i, command: *command },
                        ChildOf(root),
                    ));
                }
            }
        }
    }
}

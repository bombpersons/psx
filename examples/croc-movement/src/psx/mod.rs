//! PlayStation look for ordinary Bevy scenes.
//!
//! - Add [`PsxView`] to any `Camera3d`/`Camera2d` and it renders into the
//!   low-resolution [`PsxScreen`] target, shown letterboxed in the window.
//! - Use [`PsxMaterial`] on `Mesh3d` entities for PS1 shading. Lights are normal
//!   `DirectionalLight`/`AmbientLight`; fog is the camera's `DistanceFog`.
//! - 2D is plain Bevy `Sprite`s seen by a `Camera2d` with `PsxView`. Add
//!   `SpriteMaterial<PsxSprite>` for PS1 blend modes or CLUT palettes, and
//!   use [`PsxMaterial2d`] on `Mesh2d` polygons.
//! - [`PsxLook`] toggles the effects globally. With `quantize`, the whole
//!   screen (sprites included) is reduced to 15-bit colour after drawing.
//! - [`PsxSceneRoot`] spawns a captured frame (`.psxscene` from the psx
//!   toolkit) as ordinary sprites and meshes, for reference layouts.
//! - Original `.tim` textures load as `Image`s and `.tmd` models as
//!   [`PsxModel`]s (spawned with [`PsxModelRoot`]); see `assets.rs`.
mod assets;
pub mod formats;
mod material;
mod material2d;
mod scene;
mod shaders;

pub use assets::{
    PsxModel, PsxModelObject, PsxModelObjectIndex, PsxModelPart, PsxModelRoot, TimLoader, TimSettings, TimTransparency,
    TmdLoader, TmdSettings, rgba_image,
};

pub use material::{PsxBlend, PsxMaterial, PsxStp};
pub use material2d::{PsxMaterial2d, PsxSprite, load_indices, psx_indices, psx_palette, rgb15};
pub use scene::{
    PsxScene, PsxSceneCamera, PsxSceneDepth, PsxSceneElement, PsxSceneRoot, SCENE_BACKGROUND_LAYER, SceneElement,
};

use bevy::{
    camera::{CompositingSpace, RenderTarget, visibility::RenderLayers},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    mesh::MeshVertexBufferLayoutRef,
    pbr::MaterialPlugin,
    prelude::*,
    render::{
        render_resource::{
            AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, TextureFormat, TextureUsages,
        },
        view::Msaa,
    },
    shader::ShaderRef,
    sprite_render::{Material2d, Material2dKey, Material2dPipeline, Material2dPlugin},
};

/// Window-only layer used to show the screen texture; keep game entities off it.
pub const DISPLAY_LAYER: usize = 31;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Reflect)]
pub enum Region {
    /// 60 Hz.
    #[default]
    Ntsc,
    /// 50 Hz.
    Pal,
}

impl Region {
    pub fn hz(self) -> f64 {
        match self {
            Region::Ntsc => 60.0,
            Region::Pal => 50.0,
        }
    }
}

/// Global look switches, applied to every [`PsxMaterial`] and [`PsxMaterial2d`]
/// and to the final screen pass.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Reflect)]
#[reflect(Resource)]
pub struct PsxLook {
    /// Snap vertices to whole screen pixels (polygon "wobble").
    pub vertex_snap: bool,
    /// The GPU's 4×4 ordered dither on shaded surfaces ([`PsxMaterial`],
    /// [`PsxMaterial2d`]) before they are cut to 15 bits. Sprites are not
    /// dithered, as on the PS1.
    pub dither: bool,
    /// 15-bit framebuffer: after every `PsxView` camera has drawn, the screen
    /// is reduced to 5 bits per channel, so plain `Sprite`s and other stock
    /// Bevy 2D get the PS1 colour depth too. One cheap full-screen pass.
    pub quantize: bool,
}

impl Default for PsxLook {
    fn default() -> Self {
        Self { vertex_snap: true, dither: true, quantize: true }
    }
}

pub const MAX_LIGHTS: usize = 8;

/// Scene lighting gathered from Bevy lights for per-vertex (Gouraud) shading.
/// Intensity convention: 1000 lux of `DirectionalLight::illuminance`, or a
/// `GlobalAmbientLight::brightness` of 1000, is full strength (1.0).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct PsxLighting {
    /// Linear RGB × intensity.
    pub ambient: Vec3,
    /// World-space directions *towards* each light.
    pub directions: [Vec3; MAX_LIGHTS],
    pub colors: [Vec3; MAX_LIGHTS],
    pub count: u32,
}

/// The game's framebuffer. Change size/region before `PsxPlugin` is added.
#[derive(Resource, Clone, Debug)]
pub struct PsxScreen {
    pub width: u32,
    pub height: u32,
    pub region: Region,
    /// The finished frame: shown in the window and captured by screenshots.
    pub image: Handle<Image>,
    /// What `PsxView` cameras draw into; the screen pass copies it to
    /// `image` (15-bit when [`PsxLook::quantize`]).
    pub canvas: Handle<Image>,
}

impl Default for PsxScreen {
    fn default() -> Self {
        Self { width: 320, height: 240, region: Region::Ntsc, image: default(), canvas: default() }
    }
}

/// Marker: this camera renders into the PS1 screen target. Without 3D
/// cameras, they composite in sRGB so blending matches the GPU's.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PsxView;

/// Layer of the internal screen pass (canvas → screen image).
pub const SCREEN_PASS_LAYER: usize = 29;

/// Order of the screen pass camera: give `PsxView` cameras lower orders.
pub const SCREEN_PASS_ORDER: isize = 999;

#[derive(Component)]
struct DisplayQuad;

#[derive(Default)]
pub struct PsxPlugin {
    pub screen: PsxScreen,
}

impl Plugin for PsxPlugin {
    fn build(&self, app: &mut App) {
        let mut screen = self.screen.clone();
        let mut images = app.world_mut().resource_mut::<Assets<Image>>();
        let target = || Image::new_target_texture(screen.width, screen.height, TextureFormat::Rgba8UnormSrgb, None);
        let mut image = target();
        image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let mut canvas = target();
        canvas.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let (image, canvas) = (images.add(image), images.add(canvas));
        (screen.image, screen.canvas) = (image, canvas);

        scene::plugin(app);
        assets::plugin(app);
        shaders::plugin(app);
        app.add_plugins((
            MaterialPlugin::<PsxMaterial>::default(),
            Material2dPlugin::<PsxMaterial2d>::default(),
            Material2dPlugin::<ScreenPassMaterial>::default(),
            SpriteMaterialPlugin::<PsxSprite>::default(),
        ))
        .register_type::<PsxLook>()
        .init_resource::<PsxLook>()
        .init_resource::<PsxLighting>()
        .insert_resource(screen)
        .add_observer(attach_view)
        .add_systems(Startup, (spawn_display, spawn_screen_pass))
        .add_systems(Update, (fit_display, sync_compositing))
        .add_systems(PostUpdate, (gather_lighting, apply_look).chain().after(TransformSystems::Propagate));
    }
}

fn attach_view(add: On<Add<PsxView>>, mut commands: Commands, screen: Res<PsxScreen>) {
    commands.entity(add.entity).insert((
        RenderTarget::Image(screen.canvas.clone().into()),
        Msaa::Off,
        Tonemapping::None,
        DebandDither::Disabled,
    ));
}

/// 2D-only screens composite in sRGB, so blending works on gamma-encoded
/// colour like the GPU's (B/2+F/2 of the stored values). Bevy's 3D path
/// writes linear light, and all `PsxView` cameras share one main texture,
/// so while any of them is a `Camera3d` they all stay linear.
fn sync_compositing(
    mut commands: Commands,
    views: Query<(Entity, Has<Camera3d>, Has<CompositingSpace>), With<PsxView>>,
) {
    let srgb = !views.iter().any(|(_, is_3d, _)| is_3d);
    for (entity, _, has_space) in &views {
        if srgb && !has_space {
            commands.entity(entity).insert(CompositingSpace::Srgb);
        } else if !srgb && has_space {
            commands.entity(entity).remove::<CompositingSpace>();
        }
    }
}

/// Copies the canvas into the screen image, optionally reduced to 15 bits.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
#[bind_group_data(ScreenPassKey)]
struct ScreenPassMaterial {
    #[texture(0)]
    canvas: Handle<Image>,
    quantize: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ScreenPassKey {
    quantize: bool,
}

impl From<&ScreenPassMaterial> for ScreenPassKey {
    fn from(m: &ScreenPassMaterial) -> Self {
        Self { quantize: m.quantize }
    }
}

impl Material2d for ScreenPassMaterial {
    fn fragment_shader() -> ShaderRef {
        shaders::SCREEN.into()
    }

    fn specialize(
        _: &Material2dPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: Material2dKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if key.bind_group_data.quantize
            && let Some(fragment) = descriptor.fragment.as_mut()
        {
            fragment.shader_defs.push("QUANTIZE".into());
        }
        Ok(())
    }
}

fn spawn_screen_pass(
    mut commands: Commands,
    screen: Res<PsxScreen>,
    look: Res<PsxLook>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ScreenPassMaterial>>,
) {
    commands.spawn((
        Camera2d,
        Camera { order: SCREEN_PASS_ORDER, ..default() },
        RenderTarget::Image(screen.image.clone().into()),
        Msaa::Off,
        Tonemapping::None,
        DebandDither::Disabled,
        RenderLayers::layer(SCREEN_PASS_LAYER),
    ));
    commands.spawn((
        Mesh2d(meshes.add(Rectangle::new(screen.width as f32, screen.height as f32))),
        MeshMaterial2d(materials.add(ScreenPassMaterial { canvas: screen.canvas.clone(), quantize: look.quantize })),
        RenderLayers::layer(SCREEN_PASS_LAYER),
    ));
}

fn spawn_display(mut commands: Commands, screen: Res<PsxScreen>) {
    commands.spawn((
        Camera2d,
        Msaa::Off,
        Camera { order: 1000, clear_color: ClearColorConfig::Custom(Color::BLACK), ..default() },
        RenderLayers::layer(DISPLAY_LAYER),
    ));
    commands.spawn((
        Sprite::from_image(screen.image.clone()),
        Transform::default(),
        DisplayQuad,
        RenderLayers::layer(DISPLAY_LAYER),
    ));
}

/// Largest scale that fits the window without stretching or cropping.
pub fn fit_scale(window: Vec2, screen: Vec2) -> f32 {
    (window.x / screen.x).min(window.y / screen.y).max(0.0)
}

fn fit_display(windows: Query<&Window>, screen: Res<PsxScreen>, mut display: Query<&mut Transform, With<DisplayQuad>>) {
    let Ok(window) = windows.single() else { return };
    let scale = fit_scale(window.size(), Vec2::new(screen.width as f32, screen.height as f32));
    for mut transform in &mut display {
        transform.scale = Vec3::splat(scale);
    }
}

fn gather_lighting(
    ambient: Option<Res<GlobalAmbientLight>>,
    lights: Query<(&DirectionalLight, &GlobalTransform)>,
    mut lighting: ResMut<PsxLighting>,
) {
    let mut next = PsxLighting::default();
    if let Some(a) = ambient {
        next.ambient = a.color.to_linear().to_vec3() * a.brightness / 1000.0;
    }
    for (light, transform) in lights.iter().take(MAX_LIGHTS) {
        let i = next.count as usize;
        next.directions[i] = transform.back().as_vec3();
        next.colors[i] = light.color.to_linear().to_vec3() * light.illuminance / 1000.0;
        next.count += 1;
    }
    lighting.set_if_neq(next);
}

/// Copy global look and lighting into materials that differ (new or changed).
fn apply_look(
    look: Res<PsxLook>,
    lighting: Res<PsxLighting>,
    mut materials: ResMut<Assets<PsxMaterial>>,
    mut materials2d: ResMut<Assets<PsxMaterial2d>>,
    mut screen_pass: ResMut<Assets<ScreenPassMaterial>>,
) {
    let stale: Vec<_> =
        materials.iter().filter(|(_, m)| m.look != *look || m.lighting != *lighting).map(|(id, _)| id).collect();
    for id in stale {
        if let Some(mut m) = materials.get_mut(id) {
            m.look = *look;
            m.lighting = *lighting;
        }
    }
    let stale: Vec<_> = materials2d.iter().filter(|(_, m)| m.look != *look).map(|(id, _)| id).collect();
    for id in stale {
        if let Some(mut m) = materials2d.get_mut(id) {
            m.look = *look;
        }
    }
    let stale: Vec<_> = screen_pass.iter().filter(|(_, m)| m.quantize != look.quantize).map(|(id, _)| id).collect();
    for id in stale {
        if let Some(mut m) = screen_pass.get_mut(id) {
            m.quantize = look.quantize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_fits_without_stretching() {
        let screen = Vec2::new(320., 240.);
        for window in [Vec2::new(1600., 900.), Vec2::new(300., 900.), Vec2::new(160., 120.)] {
            let fitted = screen * fit_scale(window, screen);
            assert!(fitted.x <= window.x + 0.001 && fitted.y <= window.y + 0.001);
            assert!((fitted.x / fitted.y - 320. / 240.).abs() < 0.0001);
        }
    }
}

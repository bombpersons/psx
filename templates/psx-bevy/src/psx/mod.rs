//! PlayStation look for ordinary Bevy scenes.
//!
//! - Add [`PsxView`] to any `Camera3d`/`Camera2d` and it renders into the
//!   low-resolution [`PsxScreen`] target, shown letterboxed in the window.
//! - Use [`PsxMaterial`] on `Mesh3d` entities for PS1 shading. Lights are normal
//!   `DirectionalLight`/`AmbientLight`; fog is the camera's `DistanceFog`.
//! - 2D is plain Bevy `Sprite`s seen by a `Camera2d` with `PsxView`.
//! - [`PsxLook`] toggles the effects globally.
mod material;

pub use material::{PsxBlend, PsxMaterial};

use bevy::{
    asset::embedded_asset,
    camera::{RenderTarget, visibility::RenderLayers},
    core_pipeline::tonemapping::{DebandDither, Tonemapping},
    pbr::MaterialPlugin,
    prelude::*,
    render::{
        render_resource::{TextureFormat, TextureUsages},
        view::Msaa,
    },
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

/// Global look switches, applied to every [`PsxMaterial`].
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Reflect)]
#[reflect(Resource)]
pub struct PsxLook {
    /// Snap vertices to whole screen pixels (polygon "wobble").
    pub vertex_snap: bool,
    /// 4×4 ordered dither before 15-bit quantisation.
    pub dither: bool,
}

impl Default for PsxLook {
    fn default() -> Self {
        Self { vertex_snap: true, dither: true }
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
    pub image: Handle<Image>,
}

impl Default for PsxScreen {
    fn default() -> Self {
        Self { width: 320, height: 240, region: Region::Ntsc, image: default() }
    }
}

/// Marker: this camera renders into the PS1 screen target.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PsxView;

#[derive(Component)]
struct DisplayQuad;

#[derive(Default)]
pub struct PsxPlugin {
    pub screen: PsxScreen,
}

impl Plugin for PsxPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "shaders/psx_material.wesl");
        let mut screen = self.screen.clone();
        let mut image =
            Image::new_target_texture(screen.width, screen.height, TextureFormat::Rgba8UnormSrgb, None);
        image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        screen.image = app.world_mut().resource_mut::<Assets<Image>>().add(image);

        app.add_plugins(MaterialPlugin::<PsxMaterial>::default())
            .register_type::<PsxLook>()
            .init_resource::<PsxLook>()
            .init_resource::<PsxLighting>()
            .insert_resource(screen)
            .add_observer(attach_view)
            .add_systems(Startup, spawn_display)
            .add_systems(Update, fit_display)
            .add_systems(PostUpdate, (gather_lighting, apply_look).chain().after(TransformSystems::Propagate));
    }
}

fn attach_view(add: On<Add<PsxView>>, mut commands: Commands, screen: Res<PsxScreen>) {
    commands.entity(add.entity).insert((
        RenderTarget::Image(screen.image.clone().into()),
        Msaa::Off,
        Tonemapping::None,
        DebandDither::Disabled,
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

fn fit_display(
    windows: Query<&Window>,
    screen: Res<PsxScreen>,
    mut display: Query<&mut Transform, With<DisplayQuad>>,
) {
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
fn apply_look(look: Res<PsxLook>, lighting: Res<PsxLighting>, mut materials: ResMut<Assets<PsxMaterial>>) {
    let stale: Vec<_> = materials
        .iter()
        .filter(|(_, m)| m.look != *look || m.lighting != *lighting)
        .map(|(id, _)| id)
        .collect();
    for id in stale {
        if let Some(mut m) = materials.get_mut(id) {
            m.look = *look;
            m.lighting = *lighting;
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

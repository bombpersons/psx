//! PlayStation look for ordinary Bevy scenes.
//!
//! - Add [`PsxView`] to any `Camera3d`/`Camera2d` and it renders into the
//!   low-resolution [`PsxScreen`] target, shown letterboxed in the window.
//! - Use [`PsxMaterial`] on `Mesh3d` entities for PS1 shading. Lights are normal
//!   `DirectionalLight`/`AmbientLight`; use camera `DistanceFog` or material
//!   [`PsxDepthCue`] for reciprocal per-vertex cueing.
//! - 2D is plain Bevy `Sprite`s seen by a `Camera2d` with `PsxView`. Add
//!   `SpriteMaterial<PsxSprite>` for PS1 blend modes or CLUT palettes, and
//!   use [`PsxMaterial2d`] on `Mesh2d` polygons.
//! - Every `PsxView` camera draws gamma-encoded colour, so [`PsxBlend`]
//!   modes blend the stored values like the GPU, in 2D and 3D.
//! - [`PsxLook`] toggles the effects globally. With `quantize`, the whole
//!   screen (sprites included) is reduced to 15-bit colour after drawing.
//! - [`PsxSceneRoot`] spawns a captured frame (`.psxscene` from the psx
//!   toolkit) as ordinary sprites and meshes, for reference layouts.
//! - Original `.tim` textures load as `Image`s and `.tmd` models as
//!   [`PsxModel`]s (spawned with [`PsxModelRoot`]); see `assets.rs`.
mod assets;
pub mod font;
pub mod formats;
mod material;
mod material2d;
mod scene;
mod shaders;

pub use assets::{
    PsxModel, PsxModelObject, PsxModelObjectIndex, PsxModelPart, PsxModelRoot, TimLoader, TimSettings, TimTransparency,
    TmdLoader, TmdSettings, rgba_image,
};

pub use material::{PsxBlend, PsxDepthBias, PsxDepthCue, PsxMaterial, PsxStp};
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
        Render, RenderApp,
        extract_component::{ExtractComponent, ExtractComponentPlugin},
        render_resource::{
            AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError, TextureFormat, TextureUsages,
        },
        view::{Msaa, ResolveCompositingSpaces, ResolvedCompositingSpace, resolve_composition_spaces},
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
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq, Reflect, serde::Serialize, serde::Deserialize)]
#[reflect(Resource)]
#[serde(deny_unknown_fields)]
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

/// The game's framebuffer. The region is fixed when `PsxPlugin` is added;
/// the size may change at any time ([`PsxScreen::set_size`]): the screen
/// images, the screen pass, every `PsxView` camera (3D aspect, 2D pixel
/// scale) and the window display follow before that frame renders.
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

impl PsxScreen {
    pub fn size(&self) -> UVec2 {
        UVec2::new(self.width, self.height)
    }
    /// Resize the framebuffer (at least 1×1). Content laid out from the old
    /// size (HUD positions in screen pixels) is the game's to move.
    pub fn set_size(&mut self, size: UVec2) {
        let size = size.max(UVec2::ONE);
        (self.width, self.height) = (size.x, size.y);
    }
}

/// How the screen's shape relates to the window.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Reflect, serde::Serialize, serde::Deserialize)]
#[reflect(Resource)]
#[serde(rename_all = "lowercase")]
pub enum PsxAspect {
    /// The screen keeps its size and is letterboxed in the window.
    #[default]
    Fixed,
    /// The screen keeps its height and its width follows the window's aspect
    /// ratio (rounded to an even number of pixels), so any window shape is
    /// filled. Widening shows more of the scene at the sides.
    Window,
}

/// The width that gives a screen `height` pixels high the shape of `window`.
pub fn width_for_aspect(height: u32, window: Vec2) -> u32 {
    let width = (height as f32 * window.x / window.y.max(1.0) / 2.0).round() as u32 * 2;
    width.clamp(2, 4096)
}

/// Keeps a 2D entity at a point of the screen as its size changes, for HUDs:
/// `at` is -1..=1 per axis (`(0, -1)` bottom centre, `(1, 1)` top right),
/// scaled by half the screen size, plus `offset` in pixels. Sets the
/// `Transform`'s x and y; z, rotation and scale stay the game's.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Reflect)]
#[reflect(Component)]
#[require(Transform)]
pub struct PsxAnchor {
    pub at: Vec2,
    pub offset: Vec2,
}

impl PsxAnchor {
    pub fn new(at: Vec2, offset: Vec2) -> Self {
        Self { at, offset }
    }
    /// The position for a screen of `size` pixels, origin at its centre.
    pub fn position(&self, size: UVec2) -> Vec2 {
        self.at * size.as_vec2() / 2.0 + self.offset
    }
}

/// Marker: this camera renders into the PS1 screen target. All `PsxView`
/// cameras, 2D and 3D, draw and blend gamma-encoded colour like the GPU
/// (see `encode_psx_views`).
#[derive(Component, ExtractComponent, Clone, Copy, Debug, Default)]
#[extract_app(RenderApp)]
pub struct PsxView;

/// Layer of the internal screen pass (canvas → screen image).
pub const SCREEN_PASS_LAYER: usize = 29;

/// Order of the screen pass camera: give `PsxView` cameras lower orders.
pub const SCREEN_PASS_ORDER: isize = 999;

#[derive(Component)]
struct DisplayQuad;

#[derive(Component)]
struct ScreenPassQuad;

#[derive(Default)]
pub struct PsxPlugin {
    pub screen: PsxScreen,
}

impl Plugin for PsxPlugin {
    fn build(&self, app: &mut App) {
        let mut screen = self.screen.clone();
        let mut images = app.world_mut().resource_mut::<Assets<Image>>();
        let size = screen.size().max(UVec2::ONE);
        (screen.image, screen.canvas) = (images.add(screen_target(size)), images.add(screen_target(size)));

        scene::plugin(app);
        assets::plugin(app);
        shaders::plugin(app);
        app.add_plugins((
            MaterialPlugin::<PsxMaterial>::default(),
            Material2dPlugin::<PsxMaterial2d>::default(),
            Material2dPlugin::<ScreenPassMaterial>::default(),
            SpriteMaterialPlugin::<PsxSprite>::default(),
            ExtractComponentPlugin::<PsxView>::default(),
        ))
        .register_type::<PsxLook>()
        .init_resource::<PsxLook>()
        .init_resource::<PsxLighting>()
        .init_resource::<PsxAspect>()
        .insert_resource(screen)
        .add_observer(attach_view)
        .add_systems(Startup, (spawn_display, spawn_screen_pass))
        .add_systems(Update, (match_window_aspect, fit_display).chain())
        .add_systems(PostUpdate, resize_screen.before(bevy::camera::CameraUpdateSystems))
        .add_systems(PostUpdate, place_anchored.before(TransformSystems::Propagate))
        .add_systems(PostUpdate, (gather_lighting, apply_look).chain().after(TransformSystems::Propagate));
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(
                Render,
                (
                    hide_psx_requests.before(resolve_composition_spaces),
                    encode_psx_views.after(resolve_composition_spaces),
                )
                    .in_set(ResolveCompositingSpaces),
            );
        }
    }
}

fn attach_view(add: On<Add<PsxView>>, mut commands: Commands, screen: Res<PsxScreen>) {
    commands.entity(add.entity).insert((
        RenderTarget::Image(screen.canvas.clone().into()),
        Msaa::Off,
        Tonemapping::None,
        DebandDither::Disabled,
        // Gives the camera an `Rgba8Unorm` main texture that stores encoded
        // values; `encode_psx_views` makes it stick for 3D cameras too.
        CompositingSpace::Srgb,
    ));
}

/// The PS1 GPU blended the 15-bit values in VRAM, which are gamma-encoded:
/// B/2+F/2, B+F, B−F and B+F/4 act on those numbers. So `PsxView` cameras
/// draw into a main texture holding encoded colour (Bevy's
/// `CompositingSpace::Srgb`), and the hardware blend of every PSX material
/// works on encoded values, exactly like the GPU's.
///
/// Bevy grants `Srgb` only to stacks of `Camera2d`s, because its own 3D
/// materials write linear light. [`PsxMaterial`] writes encoded colour into
/// such a target (it checks the target format), so the PSX views keep
/// `Srgb` with 3D cameras too: their request is set aside while Bevy
/// resolves the other cameras ([`hide_psx_requests`]) and applied after.
/// Bevy's 2D shaders then encode (`SRGB_OUTPUT`), clear colours are encoded,
/// and the final copy into the sRGB canvas decodes once.
/// `StandardMaterial` under a `PsxView` would look too dark: use `PsxMaterial`.
fn encode_psx_views(mut views: Query<&mut ResolvedCompositingSpace, With<PsxView>>) {
    for mut space in &mut views {
        space.0 = Some(CompositingSpace::Srgb);
    }
}

/// See [`encode_psx_views`]: keeps Bevy's resolver from overriding (and
/// warning about) the PSX views' request on stacks with a `Camera3d`.
fn hide_psx_requests(mut views: Query<&mut ResolvedCompositingSpace, With<PsxView>>) {
    for mut space in &mut views {
        space.0 = None;
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
        ScreenPassQuad,
    ));
}

/// A screen-sized render target: the screen image or the canvas.
fn screen_target(size: UVec2) -> Image {
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    image
}

/// Follows [`PsxScreen::set_size`] with new screen images and points
/// everything that drew into or from the old ones at them: `PsxView` cameras,
/// the screen pass and the window display. Runs before cameras read their
/// target sizes, so projections (3D aspect, 2D one unit per pixel) match the
/// new size on the same frame.
///
/// New images rather than resized ones: renderer caches (material and
/// sprite bind groups) are keyed by image id, and a resized image could
/// leave one drawing from the old texture, which froze the window display.
/// Game code that keeps a clone of `PsxScreen::image` must read it again.
#[allow(clippy::too_many_arguments)]
fn resize_screen(
    mut screen: ResMut<PsxScreen>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut screen_pass: ResMut<Assets<ScreenPassMaterial>>,
    quads: Query<(&Mesh2d, &MeshMaterial2d<ScreenPassMaterial>), With<ScreenPassQuad>>,
    mut targets: Query<(&mut RenderTarget, Option<&mut Projection>)>,
    mut display: Query<&mut Sprite, With<DisplayQuad>>,
) {
    let size = screen.size().max(UVec2::ONE);
    if !screen.is_changed() || images.get(&screen.image).is_none_or(|image| image.size() == size) {
        return;
    }
    debug!("PsxScreen resized to {}x{}", size.x, size.y);
    let (old_image, old_canvas) = (screen.image.clone(), screen.canvas.clone());
    let (image, canvas) = (images.add(screen_target(size)), images.add(screen_target(size)));
    for (mut target, projection) in &mut targets {
        let RenderTarget::Image(target) = &mut *target else { continue };
        if target.handle == old_canvas {
            target.handle = canvas.clone();
        } else if target.handle == old_image {
            target.handle = image.clone();
        } else {
            continue;
        }
        // Bevy recomputes a camera's target size when its projection changes
        // or an image event arrives, not when `RenderTarget` changes. Without
        // this, a camera whose projection changes this frame for another
        // reason picks up the new size a frame before the others drawing to
        // the same image, and their shared depth texture no longer matches.
        if let Some(mut projection) = projection {
            projection.set_changed();
        }
    }
    for (quad, material) in &quads {
        if let Some(mut mesh) = meshes.get_mut(&quad.0) {
            *mesh = Rectangle::new(size.x as f32, size.y as f32).into();
        }
        if let Some(mut material) = screen_pass.get_mut(&material.0) {
            material.canvas = canvas.clone();
        }
    }
    for mut sprite in &mut display {
        sprite.image = image.clone();
    }
    (screen.image, screen.canvas) = (image, canvas);
}

fn spawn_display(
    mut commands: Commands,
    screen: Res<PsxScreen>,
    windows: Query<(), With<bevy::window::PrimaryWindow>>,
) {
    if windows.is_empty() {
        return;
    }
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

fn place_anchored(screen: Res<PsxScreen>, mut anchored: Query<(Ref<PsxAnchor>, &mut Transform)>) {
    for (anchor, mut transform) in &mut anchored {
        if screen.is_changed() || anchor.is_changed() {
            let at = anchor.position(screen.size());
            (transform.translation.x, transform.translation.y) = (at.x, at.y);
        }
    }
}

/// [`PsxAspect::Window`]: follow the window's shape. A minimised window
/// (zero size) leaves the screen as it is.
fn match_window_aspect(aspect: Res<PsxAspect>, windows: Query<&Window>, mut screen: ResMut<PsxScreen>) {
    let Ok(window) = windows.single() else { return };
    if *aspect != PsxAspect::Window || window.width() < 1.0 || window.height() < 1.0 {
        return;
    }
    let height = screen.height;
    let width = width_for_aspect(height, window.size());
    if width != screen.width {
        debug!("Window {}x{}: screen width {} -> {width}", window.width(), window.height(), screen.width);
        screen.set_size(UVec2::new(width, height));
    }
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
    fn window_aspect_keeps_height_and_fills_the_window() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(PsxScreen::default())
            .insert_resource(PsxAspect::Window)
            .add_systems(Update, match_window_aspect);
        let window = app.world_mut().spawn(Window { resolution: (1600, 900).into(), ..default() }).id();
        app.update();
        assert_eq!(app.world().resource::<PsxScreen>().size(), UVec2::new(426, 240));
        app.world_mut().get_mut::<Window>(window).unwrap().resolution.set(1000.0, 1000.0);
        app.update();
        assert_eq!(app.world().resource::<PsxScreen>().size(), UVec2::new(240, 240));
        *app.world_mut().resource_mut::<PsxAspect>() = PsxAspect::Fixed;
        app.world_mut().get_mut::<Window>(window).unwrap().resolution.set(1600.0, 900.0);
        app.update();
        assert_eq!(app.world().resource::<PsxScreen>().size(), UVec2::new(240, 240), "Fixed leaves the size alone");
        assert_eq!(width_for_aspect(240, Vec2::new(4000.0, 10.0)), 4096);
    }

    #[test]
    fn anchors_follow_the_screen_edges() {
        let bottom_left = PsxAnchor::new(Vec2::new(-1.0, -1.0), Vec2::new(8.0, 8.0));
        assert_eq!(bottom_left.position(UVec2::new(320, 240)), Vec2::new(-152.0, -112.0));
        assert_eq!(bottom_left.position(UVec2::new(426, 240)), Vec2::new(-205.0, -112.0));
        assert_eq!(PsxAnchor::default().position(UVec2::new(640, 480)), Vec2::ZERO);
    }

    #[test]
    fn screen_targets_follow_set_size() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<Mesh>()
            .init_asset::<ScreenPassMaterial>()
            .add_systems(Update, resize_screen);
        let new = || Image::new_target_texture(320, 240, TextureFormat::Rgba8UnormSrgb, None);
        let (image, canvas) = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            (images.add(new()), images.add(new()))
        };
        let quad = app.world_mut().resource_mut::<Assets<Mesh>>().add(Rectangle::new(320.0, 240.0));
        let pass = ScreenPassMaterial { canvas: canvas.clone(), quantize: false };
        let pass = app.world_mut().resource_mut::<Assets<ScreenPassMaterial>>().add(pass);
        app.world_mut().spawn((Mesh2d(quad.clone()), MeshMaterial2d(pass), ScreenPassQuad));
        app.insert_resource(PsxScreen { image: image.clone(), canvas: canvas.clone(), ..default() });
        app.update();
        app.world_mut().resource_mut::<PsxScreen>().set_size(UVec2::new(426, 240));
        let target = app.world_mut().spawn((RenderTarget::Image(canvas.clone().into()), Projection::default())).id();
        let other =
            app.world_mut().spawn((RenderTarget::Image(Handle::<Image>::default().into()), Projection::default())).id();
        let display = app.world_mut().spawn((Sprite::from_image(image.clone()), DisplayQuad)).id();
        app.update();
        let screen = app.world().resource::<PsxScreen>().clone();
        assert!(screen.image != image && screen.canvas != canvas, "new images, so no renderer cache keeps the old");
        let images = app.world().resource::<Assets<Image>>();
        for handle in [&screen.image, &screen.canvas] {
            assert_eq!(images.get(handle).unwrap().size(), UVec2::new(426, 240));
        }
        let RenderTarget::Image(retargeted) = app.world().get::<RenderTarget>(target).unwrap() else { panic!() };
        assert_eq!(retargeted.handle, screen.canvas, "PsxView cameras follow the canvas");
        let changed = |e: Entity| app.world().entity(e).get_ref::<Projection>().unwrap().last_changed().get();
        assert!(changed(target) > changed(other), "retargeted cameras recompute their size together");
        let pass = app.world().resource::<Assets<ScreenPassMaterial>>().iter().next().unwrap().1.canvas.clone();
        assert_eq!(pass, screen.canvas);
        let shown = &app.world().get::<Sprite>(display).unwrap().image;
        assert_eq!(*shown, screen.image, "the window shows the new screen image");
        let mesh = app.world().resource::<Assets<Mesh>>().get(&quad).unwrap();
        let x = mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap().iter().map(|p| p[0]);
        assert_eq!(x.fold(0.0f32, f32::max), 213.0);
        app.world_mut().resource_mut::<PsxScreen>().set_size(UVec2::ZERO);
        assert_eq!(app.world().resource::<PsxScreen>().size(), UVec2::ONE);
    }

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

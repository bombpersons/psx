//! Example game: replace this module with your game's own plugins (and
//! `crate::GamePlugins`' list of them).
//!
//! Shows the intended shape:
//! - one plugin per feature, each usable on its own: [`player::PlayerPlugin`]
//!   and [`SpinnerPlugin`] are rules (they run headless), [`DemoPlugin`] is
//!   the scene, HUD and presentation. A mod can drop `DemoPlugin` and keep
//!   the rest (see `mods/example`);
//! - level content is data (`assets/demo/level.ron`, the engine's format),
//!   spawned through the engine's object type registry;
//! - each object type is code registered by id: the player's controller
//!   ([`player`], no rendering, tested headless in `tests/player.rs`) and a
//!   spinner; presentation dresses spawned objects separately;
//! - a trigger in the level runs a registered action (flash the gem);
//! - ordinary Bevy entities with `PsxMaterial` meshes, normal lights and fog,
//!   and a 2D HUD of sprites over 3D.
pub mod player;

use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    mesh::{Indices, PrimitiveTopology},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use engine::config::load_ron;
use engine::controller::Button;
use engine::prelude::*;
use engine::psx::{PsxAnchor, PsxBlend, PsxLook, PsxMaterial, PsxMaterial2d, PsxSprite, PsxStp, PsxView};
use engine::psx::{font, psx_indices, psx_palette, rgb15};
use crate::disc::GameData;
use player::Player;

/// The spinner object type and anything with [`Spin`]: rules only.
pub struct SpinnerPlugin;

impl Plugin for SpinnerPlugin {
    fn build(&self, app: &mut App) {
        app.register_object_type(SPINNER_TYPE, ObjectTypeInfo::new("Spinner", spawn_spinner))
            .add_systems(FixedUpdate, spin.in_set(EngineSet::Objects));
    }
}

/// The spinner's spawn hook: `params: [speed in mrad/s, colour 0|1]`.
pub fn spawn_spinner(entity: &mut EntityWorldMut, placement: &Placement) {
    let param = |i: usize| placement.params.get(i).copied().unwrap_or(0);
    entity.insert((Spin(param(0) as f32 / 1000.0), Spinner { colour: param(1) }));
}

/// The demo's scene: cameras, floor, the level, the HUD and the look of the
/// player and spinners. Needs the renderer. The glow panel turns only with
/// [`SpinnerPlugin`], which runs everything with [`Spin`].
pub struct DemoPlugin;

impl Plugin for DemoPlugin {
    fn build(&self, app: &mut App) {
        app.register_action(FLASH_ACTION, "flash", flash)
            .init_resource::<Flash>()
            // World setup waits for the game's data (imported from the disc).
            .add_systems(OnEnter(GameData::Ready), (setup, setup_2d, show_disc))
            .add_systems(FixedUpdate, (toggle_look, cycle_palette, flash_gem).in_set(GameSystems::Game))
            .add_systems(FixedUpdate, update_hud.in_set(GameSystems::Finish))
            .add_systems(Update, (dress_player, dress_spinners, follow_camera));
    }
}

/// A spinning sphere: `params: [speed in mrad/s, colour 0|1]`.
pub const SPINNER_TYPE: ObjectTypeId = ObjectTypeId(2);
/// Flashes the HUD gem: `args: [ticks]`.
pub const FLASH_ACTION: ActionId = ActionId(1);

/// Turning speed, rad/s.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Spin(pub f32);

#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Spinner {
    pub colour: i32,
}

/// Ticks left of a triggered gem flash.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct Flash(pub u32);

fn flash(In(call): In<ActionCall>, mut flash: ResMut<Flash>) {
    flash.0 = call.args.first().copied().unwrap_or(1).max(0) as u32;
}
#[derive(Component)]
struct Indicator(Button);

const FOG: Color = Color::srgb(0.05, 0.06, 0.12);

/// A small paletted checkerboard, like a 4-bit CLUT texture.
fn checker(images: &mut Assets<Image>, a: [u8; 3], b: [u8; 3], repeat: bool) -> Handle<Image> {
    let size = 32;
    let mut data = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let c = if (x / 8 + y / 8) % 2 == 0 { a } else { b };
            // A dark border makes affine warping and snapping easy to see.
            let c = if x % 8 == 0 || y % 8 == 0 { [c[0] / 2, c[1] / 2, c[2] / 2] } else { c };
            data.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    let mut image = Image::new(
        Extent3d { width: size as u32, height: size as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    if repeat {
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::nearest()
        });
    }
    images.add(image)
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    // 3D camera: renders first and clears. DistanceFog drives PS1 depth cueing.
    commands.spawn((
        Camera3d::default(),
        Camera { order: 0, clear_color: ClearColorConfig::Custom(FOG), ..default() },
        PsxView,
        DistanceFog { color: FOG, falloff: FogFalloff::Linear { start: 6.0, end: 18.0 }, ..default() },
        Transform::from_xyz(0.0, 3.0, 7.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    // 2D camera: draws sprites over the 3D image without clearing it.
    commands.spawn((Camera2d, Camera { order: 1, clear_color: ClearColorConfig::None, ..default() }, PsxView));

    commands.spawn((
        DirectionalLight { illuminance: 900.0, ..default() },
        Transform::from_xyz(2.0, 4.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 300.0, ..default() });

    // Floor: one big quad per tile row would be authentic; a tiled texture is simpler.
    let mut floor = Plane3d::default().mesh().size(24.0, 24.0).subdivisions(5).build();
    if let Some(bevy::mesh::VertexAttributeValues::Float32x2(uvs)) = floor.attribute_mut(Mesh::ATTRIBUTE_UV_0) {
        uvs.iter_mut().for_each(|uv| *uv = [uv[0] * 6.0, uv[1] * 6.0]);
    }
    commands.spawn((
        Mesh3d(meshes.add(floor)),
        MeshMaterial3d(materials.add(PsxMaterial::textured(checker(&mut images, [90, 140, 70], [60, 100, 50], true)))),
    ));

    // Everything the level places comes from data, through the registry.
    let level: LevelData = load_ron("demo/level.ron").unwrap_or_else(|e| panic!("demo level: {e}"));
    commands.queue(SpawnLevel(level));
    // Additive "glow" panel: PS1 semi-transparency mode B+F.
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(1.5, 1.5))),
        MeshMaterial3d(materials.add(PsxMaterial {
            color: Color::srgb(0.6, 0.3, 0.1),
            blend: PsxBlend::Add,
            lit: false,
            double_sided: true,
            ..default()
        })),
        Transform::from_xyz(0.0, 1.5, -3.5),
        Spin(0.6),
    ));

    // HUD: 2D sprites in screen pixels (origin at the centre, y up), anchored
    // to the bottom edge so they stay there at any screen size.
    let bottom = Vec2::new(0.0, -1.0);
    for (i, button) in Button::ALL.into_iter().enumerate() {
        commands.spawn((
            Sprite::from_color(Color::srgb(0.3, 0.35, 0.4), Vec2::splat(8.0)),
            PsxAnchor::new(bottom, Vec2::new(-6.5 * 11.0 + i as f32 * 11.0, 8.0)),
            Indicator(button),
        ));
    }
}

/// Shows which disc the data was imported from, when there is an import:
/// the template's importer writes `imported/disc.ron` (`tools/import`).
fn show_disc(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let Ok(info) = load_ron::<import::DiscInfo>(import::DiscInfo::PATH) else { return };
    let text = format!("DISC {} ({}, {} files)", info.volume_id, info.serial, info.files);
    commands.spawn((
        Sprite::from_image(images.add(font::text_image(&text))),
        PsxAnchor::new(Vec2::new(0.0, -1.0), Vec2::new(0.0, 24.0)),
    ));
}

/// The gem's palette: row 0 normal (entries 1-4 cycle), row 1 a white flash.
#[derive(Resource)]
struct GemPalette(Handle<Image>);

/// 2D on the PS1 screen: a paletted sprite (CLUT swap, palette cycling and
/// a per-texel STP highlight), a Gouraud-shaded `Mesh2d` (dithered) and a
/// smooth 24-bit gradient sprite (cut to 15 bits by `PsxLook::quantize`).
fn setup_2d(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut sprites: ResMut<Assets<PsxSprite>>,
    mut materials2d: ResMut<Assets<PsxMaterial2d>>,
) {
    // A row along the top edge, kept there by `PsxAnchor`.
    let top = |x: f32| PsxAnchor::new(Vec2::new(0.0, 1.0), Vec2::new(x, -16.0));
    // 16×16 gem as 8-bit indices: 0 transparent, 1-4 body bands, 5 outline, 6 highlight.
    let indices = (0..16 * 16)
        .map(|i| {
            let (x, y) = ((i % 16) as f32 - 7.5, (i / 16) as f32 - 7.5);
            match x.abs() + y.abs() {
                d if d > 8.0 => 0,
                d if d > 7.0 => 5,
                _ if (x + 2.5).abs() + (y + 2.5).abs() < 2.0 => 6,
                d => 1 + (d as u8 / 2) % 4,
            }
        })
        .collect();
    let body = [rgb15(4, 10, 31), rgb15(8, 16, 31), rgb15(14, 22, 31), rgb15(20, 28, 31)];
    // Index 6 has the STP bit (8000h): with `PsxStp::Texel` only it blends (additively).
    let normal = [0, body[0], body[1], body[2], body[3], rgb15(2, 3, 10), 0x8000 | rgb15(16, 16, 16)];
    let flash = [0, 0x7FFF, 0x7FFF, 0x7FFF, 0x7FFF, rgb15(31, 31, 31), 0x7FFF];
    let palette = images.add(psx_palette(&[&normal, &flash]));
    commands.insert_resource(GemPalette(palette.clone()));
    commands.spawn((
        Sprite::from_image(images.add(psx_indices(16, 16, indices))),
        SpriteMaterial(sprites.add(PsxSprite {
            blend: PsxBlend::Add,
            stp: PsxStp::Texel,
            ..PsxSprite::indexed(palette, 0)
        })),
        top(-120.0),
        Transform::from_scale(Vec3::splat(2.0)),
    ));

    // Gouraud quad: vertex colours interpolate; `PsxLook::dither` dithers them.
    let quad = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[-40.0, -8.0, 0.0], [40.0, -8.0, 0.0], [-40.0, 8.0, 0.0], [40.0, 8.0, 0.0]],
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]])
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_COLOR,
            vec![[0.02, 0.0, 0.1, 1.0], [0.6, 0.1, 0.0, 1.0], [0.0, 0.05, 0.2, 1.0], [0.9, 0.5, 0.0, 1.0]],
        )
        .with_inserted_indices(Indices::U32(vec![0, 1, 2, 1, 3, 2]));
    commands.spawn((Mesh2d(meshes.add(quad)), MeshMaterial2d(materials2d.add(PsxMaterial2d::default())), top(-40.0)));

    // A plain sprite with a smooth 24-bit gradient: banding shows the 15-bit screen.
    let gradient: Vec<u8> =
        (0..8).flat_map(|_| (0..80u32).flat_map(|x| [(x * 3) as u8, (x * 2) as u8, 40, 255])).collect();
    let gradient = Image::new(
        Extent3d { width: 80, height: 8, depth_or_array_layers: 1 },
        TextureDimension::D2,
        gradient,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    commands.spawn((
        Sprite { custom_size: Some(Vec2::new(80.0, 16.0)), ..Sprite::from_image(images.add(gradient)) },
        top(60.0),
    ));
}

/// Classic palette cycling: rotate entries 1-4 of the gem's normal palette.
fn cycle_palette(clock: Res<GameClock>, palette: Res<GemPalette>, mut images: ResMut<Assets<Image>>) {
    if !clock.frame.is_multiple_of(8) {
        return;
    }
    if let Some(mut image) = images.get_mut(&palette.0)
        && let Some(data) = image.data.as_mut()
    {
        // Row 0, RGBA8 entries 1..=4.
        data[4..20].rotate_left(4);
    }
}

/// Circle, or the level's trigger, swaps the gem to its flash palette (a
/// CLUT swap: one field).
fn flash_gem(
    pad: Res<ControllerState>,
    mut flash: ResMut<Flash>,
    gems: Query<&SpriteMaterial<PsxSprite>>,
    mut sprites: ResMut<Assets<PsxSprite>>,
) {
    let row = u32::from(pad.held(Button::Circle) || flash.0 > 0);
    flash.0 = flash.0.saturating_sub(1);
    for gem in &gems {
        if sprites.get(&gem.0).is_some_and(|s| s.palette_row != row)
            && let Some(mut sprite) = sprites.get_mut(&gem.0)
        {
            sprite.palette_row = row;
        }
    }
}

/// Presentation for the rules-only player the level spawned: a textured crate.
fn dress_player(
    mut commands: Commands,
    players: Query<Entity, Added<Player>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    for player in &players {
        let texture = checker(&mut images, [200, 150, 80], [150, 100, 50], false);
        commands.entity(player).insert((
            Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
            MeshMaterial3d(materials.add(PsxMaterial::textured(texture))),
        ));
    }
}

fn dress_spinners(
    mut commands: Commands,
    spinners: Query<(Entity, &Spinner), Added<Spinner>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for (entity, spinner) in &spinners {
        let i = spinner.colour.clamp(0, 1) as f32;
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Sphere::new(0.6).mesh().ico(1).unwrap())),
            MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.3 + 0.5 * i, 0.4, 0.9 - 0.5 * i)))),
        ));
    }
}

fn spin(time: Res<Time>, mut q: Query<(&Spin, &mut Transform)>) {
    for (spin, mut t) in &mut q {
        t.rotate_y(spin.0 * time.delta_secs());
    }
}

/// Triangle toggles the PS1 effects for comparison.
fn toggle_look(pad: Res<ControllerState>, mut look: ResMut<PsxLook>) {
    if pad.pressed(Button::Triangle) {
        let on = !look.vertex_snap;
        *look = PsxLook { vertex_snap: on, dither: on, quantize: on };
    }
}

fn follow_camera(
    player: Query<&Transform, With<Player>>,
    mut cam: Query<&mut Transform, (With<Camera3d>, Without<Player>)>,
) {
    let (Ok(p), Ok(mut c)) = (player.single(), cam.single_mut()) else { return };
    // Follow on the floor plane only, so a hop is visible against the floor.
    let target = Vec3::new(p.translation.x, player::GROUND_Y, p.translation.z);
    c.translation = target + Vec3::new(0.0, 2.5, 6.5);
    c.look_at(target, Vec3::Y);
}

fn update_hud(pad: Res<ControllerState>, mut q: Query<(&Indicator, &mut Sprite)>) {
    for (indicator, mut sprite) in &mut q {
        sprite.color = if pad.held(indicator.0) {
            Color::srgb(1.0, 0.7, 0.25)
        } else if pad.released(indicator.0) {
            Color::srgb(0.45, 0.5, 0.55)
        } else {
            Color::srgb(0.3, 0.35, 0.4)
        };
    }
}

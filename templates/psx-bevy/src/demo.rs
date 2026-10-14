//! Example scene: replace this module with your game's own plugins.
//!
//! Shows the intended shape: ordinary Bevy entities, `PsxMaterial` meshes,
//! normal lights and fog, a 2D HUD of sprites over 3D, and gameplay in
//! `FixedUpdate` reading `ControllerState`.
use crate::{
    GameSystems,
    controller::{Button, ControllerState},
    psx::{PsxBlend, PsxLook, PsxMaterial, PsxScreen, PsxView},
};
use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

pub struct DemoPlugin;

impl Plugin for DemoPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(FixedUpdate, (move_player, spin, toggle_look).in_set(GameSystems::Game))
            .add_systems(FixedUpdate, update_hud.in_set(GameSystems::Finish))
            .add_systems(Update, follow_camera);
    }
}

#[derive(Component)]
struct Player;
#[derive(Component)]
struct Spin(f32);
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
    screen: Res<PsxScreen>,
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

    let crate_texture = checker(&mut images, [200, 150, 80], [150, 100, 50], false);
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
        MeshMaterial3d(materials.add(PsxMaterial::textured(crate_texture))),
        Transform::from_xyz(0.0, 0.5, 0.0),
        Player,
    ));
    for (i, x) in [-3.0, 3.0].into_iter().enumerate() {
        commands.spawn((
            Mesh3d(meshes.add(Sphere::new(0.6).mesh().ico(1).unwrap())),
            MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.3 + 0.5 * i as f32, 0.4, 0.9 - 0.5 * i as f32)))),
            Transform::from_xyz(x, 1.0, -2.0),
            Spin(1.0 + i as f32),
        ));
    }
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
        Spin(0.5),
    ));

    // HUD: 2D sprites in screen pixels (origin at the centre, y up).
    let bottom = -(screen.height as f32) / 2.0 + 8.0;
    for (i, button) in Button::ALL.into_iter().enumerate() {
        commands.spawn((
            Sprite::from_color(Color::srgb(0.3, 0.35, 0.4), Vec2::splat(8.0)),
            Transform::from_xyz(-6.5 * 11.0 + i as f32 * 11.0, bottom, 0.0),
            Indicator(button),
        ));
    }
}

/// Gameplay runs at a fixed rate; speeds are per tick.
fn move_player(pad: Res<ControllerState>, mut player: Query<&mut Transform, With<Player>>) {
    let dir = pad.direction();
    for mut t in &mut player {
        t.translation += Vec3::new(dir.x, 0.0, -dir.y) * 0.08;
        t.translation = t.translation.clamp(Vec3::new(-10.0, 0.5, -10.0), Vec3::new(10.0, 0.5, 10.0));
        t.rotate_y(0.03 * (f32::from(u8::from(pad.held(Button::L1))) - f32::from(u8::from(pad.held(Button::R1)))));
    }
}

fn spin(mut q: Query<(&Spin, &mut Transform)>) {
    for (spin, mut t) in &mut q {
        t.rotate_y(0.02 * spin.0);
    }
}

/// Triangle toggles the PS1 effects for comparison.
fn toggle_look(pad: Res<ControllerState>, mut look: ResMut<PsxLook>) {
    if pad.pressed(Button::Triangle) {
        let on = !look.vertex_snap;
        look.vertex_snap = on;
        look.dither = on;
    }
}

fn follow_camera(player: Query<&Transform, With<Player>>, mut cam: Query<&mut Transform, (With<Camera3d>, Without<Player>)>) {
    let (Ok(p), Ok(mut c)) = (player.single(), cam.single_mut()) else { return };
    c.translation = p.translation + Vec3::new(0.0, 2.5, 6.5);
    c.look_at(p.translation, Vec3::Y);
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

//! Placeholder world: a checkered meadow, a few posts and crystals, Croc as a
//! capsule with a snout, a blob shadow, a chase camera and a button HUD.
//! Nothing here comes from the original game's data.
use super::movement::{Croc, Facing, Ground, units};
use crate::{
    controller::{Button, ControllerState},
    psx::{PsxLook, PsxMaterial, PsxScreen, PsxView},
    shell::GameSystems,
};
use bevy::{
    asset::RenderAssetUsages,
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(FixedUpdate, (toggle_look.in_set(GameSystems::Game), update_hud.in_set(GameSystems::Finish)))
            .add_systems(Update, (follow_shadow, chase_camera));
    }
}

#[derive(Component)]
struct Shadow;
#[derive(Component)]
struct Indicator(Button);

const SKY: Color = Color::srgb(0.45, 0.62, 0.85);
/// Croc's placeholder size: about 600 units tall (a guess; the rules do not use it).
const CROC_HEIGHT: f32 = units(600.0);

fn checker(images: &mut Assets<Image>, a: [u8; 3], b: [u8; 3]) -> Handle<Image> {
    let size = 32;
    let mut data = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let c = if (x / 16 + y / 16) % 2 == 0 { a } else { b };
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
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::nearest()
    });
    images.add(image)
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
    mut images: ResMut<Assets<Image>>,
    screen: Res<PsxScreen>,
) {
    commands.spawn((
        Camera3d::default(),
        Camera { order: 0, clear_color: ClearColorConfig::Custom(SKY), ..default() },
        PsxView,
        DistanceFog { color: SKY, falloff: FogFalloff::Linear { start: 20.0, end: 60.0 }, ..default() },
        Transform::from_xyz(0.0, 2.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((Camera2d, Camera { order: 1, clear_color: ClearColorConfig::None, ..default() }, PsxView));
    commands.spawn((
        DirectionalLight { illuminance: 850.0, ..default() },
        Transform::from_xyz(3.0, 6.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 350.0, ..default() });

    // Meadow: one checker square is 1024 Croc units (two Bevy units).
    let size = 128.0;
    let mut floor = Plane3d::default().mesh().size(size, size).subdivisions(32).build();
    if let Some(bevy::mesh::VertexAttributeValues::Float32x2(uvs)) = floor.attribute_mut(Mesh::ATTRIBUTE_UV_0) {
        uvs.iter_mut().for_each(|uv| *uv = [uv[0] * size / 4.0, uv[1] * size / 4.0]);
    }
    commands.spawn((
        Mesh3d(meshes.add(floor)),
        MeshMaterial3d(materials.add(PsxMaterial::textured(checker(&mut images, [96, 150, 64], [78, 128, 52])))),
    ));

    // Posts every 4096 units and a ring of crystals, to read speed and turns against.
    let post = meshes.add(Cuboid::new(0.3, 2.0, 0.3));
    let post_material = materials.add(PsxMaterial::color(Color::srgb(0.55, 0.4, 0.25)));
    for i in -6..=6 {
        for j in -6..=6 {
            if (i + j) % 2 != 0 || (i == 0 && j == 0) {
                continue;
            }
            commands.spawn((
                Mesh3d(post.clone()),
                MeshMaterial3d(post_material.clone()),
                Transform::from_xyz(i as f32 * units(4096.0), 1.0, j as f32 * units(4096.0)),
            ));
        }
    }
    let crystal = meshes.add(Sphere::new(0.25).mesh().ico(0).unwrap());
    for k in 0..12 {
        let angle = k as f32 / 12.0 * std::f32::consts::TAU;
        commands.spawn((
            Mesh3d(crystal.clone()),
            MeshMaterial3d(materials.add(PsxMaterial::color(Color::hsl(k as f32 * 30.0, 0.8, 0.6)))),
            Transform::from_xyz(angle.sin() * 6.0, 0.6, angle.cos() * 6.0),
        ));
    }

    // Croc: a green capsule, a snout pointing forward (-Z) and a back plate.
    let green = materials.add(PsxMaterial::color(Color::srgb(0.25, 0.7, 0.25)));
    let radius = CROC_HEIGHT * 0.3;
    commands
        .spawn((Croc, Ground { height: 0.0 }, Facing::default(), Transform::default(), Visibility::default()))
        .with_children(|croc| {
            croc.spawn((
                Mesh3d(meshes.add(Capsule3d::new(radius, CROC_HEIGHT - 2.0 * radius))),
                MeshMaterial3d(green.clone()),
                Transform::from_xyz(0.0, CROC_HEIGHT / 2.0, 0.0),
            ));
            croc.spawn((
                Mesh3d(meshes.add(Cuboid::new(radius, radius * 0.6, radius * 1.6))),
                MeshMaterial3d(green.clone()),
                Transform::from_xyz(0.0, CROC_HEIGHT * 0.8, -radius * 1.4),
            ));
            croc.spawn((
                Mesh3d(meshes.add(Cuboid::new(radius * 1.4, radius * 1.4, radius * 0.5))),
                MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.55, 0.35, 0.2)))),
                Transform::from_xyz(0.0, CROC_HEIGHT * 0.55, radius * 1.1),
            ));
        });
    commands.spawn((
        Mesh3d(meshes.add(Circle::new(radius * 1.3))),
        MeshMaterial3d(materials.add(PsxMaterial { color: Color::srgb(0.1, 0.15, 0.1), lit: false, ..default() })),
        Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
        Shadow,
    ));

    // HUD: one square per button, lit while held.
    let bottom = -(screen.height as f32) / 2.0 + 8.0;
    for (i, button) in Button::ALL.into_iter().enumerate() {
        commands.spawn((
            Sprite::from_color(Color::srgb(0.3, 0.35, 0.4), Vec2::splat(8.0)),
            Transform::from_xyz(-6.5 * 11.0 + i as f32 * 11.0, bottom, 0.0),
            Indicator(button),
        ));
    }
}

/// A blob shadow on the floor under Croc shows how high he is.
fn follow_shadow(
    croc: Query<(&Transform, &Ground), With<Croc>>,
    mut shadow: Query<&mut Transform, (With<Shadow>, Without<Croc>)>,
) {
    let (Ok((croc, ground)), Ok(mut shadow)) = (croc.single(), shadow.single_mut()) else { return };
    shadow.translation = Vec3::new(croc.translation.x, ground.height + 0.01, croc.translation.z);
}

/// Behind and above Croc, easing towards his facing. Not the original camera.
fn chase_camera(
    time: Res<Time>,
    croc: Query<(&Transform, &Facing), With<Croc>>,
    mut camera: Query<&mut Transform, (With<Camera3d>, Without<Croc>)>,
) {
    let (Ok((croc, facing)), Ok(mut camera)) = (croc.single(), camera.single_mut()) else { return };
    let behind = Quat::from_rotation_y(facing.yaw) * Vec3::new(0.0, 2.5, 6.0);
    let goal = croc.translation + behind;
    let blend = 1.0 - (-4.0 * time.delta_secs()).exp();
    camera.translation = camera.translation.lerp(goal, blend);
    camera.look_at(croc.translation + Vec3::Y * CROC_HEIGHT, Vec3::Y);
}

/// Triangle toggles the PS1 effects for comparison.
fn toggle_look(pad: Res<ControllerState>, mut look: ResMut<PsxLook>) {
    if pad.pressed(Button::Triangle) {
        let on = !look.vertex_snap;
        look.vertex_snap = on;
        look.dither = on;
    }
}

fn update_hud(pad: Res<ControllerState>, mut q: Query<(&Indicator, &mut Sprite)>) {
    for (indicator, mut sprite) in &mut q {
        sprite.color = if pad.held(indicator.0) { Color::srgb(1.0, 0.7, 0.25) } else { Color::srgb(0.3, 0.35, 0.4) };
    }
}

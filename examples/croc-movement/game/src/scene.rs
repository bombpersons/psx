//! What the player sees: the floor, the characters (Croc as a stand-in
//! shape until his model is imported), lights and the camera. Dresses what
//! the rules spawn; no rules here.
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use psx_bevy::psx::{PsxMaterial, PsxView};

use crate::camera::{CameraFollow, CameraPlugin, CameraTarget, FollowCamera};
use crate::character::{Character, Facing, Position};
use crate::croc::Croc;
use crate::disc::GameData;
use crate::world::{Floor, METRES_PER_UNIT, to_metres};

const SKY: Color = Color::srgb(0.45, 0.65, 0.9);

/// The stand-in floor's size, metres: a flat floor has no edge, so it is
/// drawn this wide, centred on the world's middle (x, z 12288 units).
const FLOOR_SIZE: f32 = 400.0;
const WORLD_MIDDLE: f32 = 12288.0 * METRES_PER_UNIT;

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(CameraPlugin)
            .add_systems(OnEnter(GameData::Ready), setup)
            .add_observer(dress_floor)
            .add_observer(dress_croc)
            .add_systems(Update, place_characters.before(CameraFollow));
    }
}

fn setup(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        Camera { order: 0, clear_color: ClearColorConfig::Custom(SKY), ..default() },
        PsxView,
        DistanceFog { color: SKY, falloff: FogFalloff::Linear { start: 20.0, end: 60.0 }, ..default() },
        FollowCamera::default(),
        Transform::default(),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 900.0, ..default() },
        Transform::from_xyz(2.0, 4.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 300.0, ..default() });
}

fn dress_floor(
    add: On<Add<Floor>>,
    floors: Query<&Floor>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    let Ok(floor) = floors.get(add.entity) else { return };
    let y = floor.height as f32 * METRES_PER_UNIT;
    commands.entity(add.entity).insert((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(FLOOR_SIZE, FLOOR_SIZE).subdivisions(40))),
        MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.3, 0.55, 0.25)))),
        Transform::from_xyz(WORLD_MIDDLE, y, WORLD_MIDDLE),
    ));
}

fn dress_croc(
    add: On<Add<Croc>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    let body = meshes.add(Capsule3d::new(0.35, 0.5));
    let snout = meshes.add(Cuboid::new(0.3, 0.25, 0.5));
    let green = materials.add(PsxMaterial::color(Color::srgb(0.2, 0.7, 0.25)));
    commands.entity(add.entity).insert((Transform::default(), Visibility::default(), CameraTarget)).with_children(
        |c| {
            c.spawn((Mesh3d(body), MeshMaterial3d(green.clone()), Transform::from_xyz(0.0, 0.6, 0.0)));
            c.spawn((Mesh3d(snout), MeshMaterial3d(green), Transform::from_xyz(0.0, 0.9, 0.4)));
        },
    );
}

fn place_characters(mut q: Query<(&Position, &Facing, &mut Transform), With<Character>>) {
    for (position, facing, mut t) in &mut q {
        t.translation = to_metres(position.0);
        t.rotation = facing.to_rotation();
    }
}

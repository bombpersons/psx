//! An example mod: a small game built on the game crate by recompiling it,
//! using only `game`'s public API and this crate's hand-made `assets/`.
//!
//! - It keeps the game's player controller and spinner type and drops the
//!   demo's scene (`GamePlugins.build().disable::<DemoPlugin>()`).
//! - It brings its own level (`assets/example_mod/level.ron`), camera and
//!   character model for the ported controller.
//! - It adds an object type (a bobbing beacon) and replaces the spinner's
//!   spawn hook (`replace_object_type`), reusing the game's own.
//! - It overrides the player's tuning by shipping `assets/demo/player.ron`:
//!   this crate's `assets/` is searched before the game's
//!   (`engine::assets::add_root`).
//!
//! A port's mod looks the same: reuse the shared modules and controllers the
//! game exposes, register new types, replace what it changes.
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use engine::prelude::*;
use engine::psx::{PsxMaterial, PsxView};
use game::demo::player::{GROUND_Y, Player};
use game::demo::{SPINNER_TYPE, Spin, Spinner, spawn_spinner};
use serde::Deserialize;

/// The game's asset folder, relative to this crate (see
/// `engine::assets::add_root`). A shipped build points at wherever the
/// game's assets are installed instead.
pub const GAME_ASSETS: &str = "../../game/assets";

/// A beacon bobbing up and down: `params: [amplitude in mm]`. Mod type ids
/// start at 100, clear of the game's.
pub const BEACON_TYPE: ObjectTypeId = ObjectTypeId(100);

/// The mod's level file: level data plus its floor.
#[derive(Deserialize)]
pub struct Arena {
    pub floor_half_size: f32,
    pub floor_colour: [f32; 3],
    pub level: LevelData,
}

impl Arena {
    pub const PATH: &str = "example_mod/level.ron";
}

#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Beacon {
    pub base_y: f32,
    /// m.
    pub amplitude: f32,
}

/// The mod's rules: a new type and a replaced one. No rendering, so it runs
/// in headless tests (`tests/rules.rs`).
pub struct ModPlugin;

impl Plugin for ModPlugin {
    fn build(&self, app: &mut App) {
        app.register_object_type(
            BEACON_TYPE,
            ObjectTypeInfo::new("Beacon", |entity, placement| {
                let amplitude = placement.params.first().copied().unwrap_or(200) as f32 / 1000.0;
                entity.insert(Beacon { base_y: placement.position.y, amplitude });
            }),
        )
        // Spinners turn twice as fast here: the game's own hook, then a tweak.
        .replace_object_type(
            SPINNER_TYPE,
            ObjectTypeInfo::new("Fast spinner", |entity, placement| {
                spawn_spinner(entity, placement);
                if let Some(mut spin) = entity.get_mut::<Spin>() {
                    spin.0 *= 2.0;
                }
            }),
        )
        .add_systems(FixedUpdate, bob.in_set(EngineSet::Objects));
    }
}

fn bob(time: Res<Time>, mut beacons: Query<(&Beacon, &mut Transform)>) {
    for (beacon, mut transform) in &mut beacons {
        transform.translation.y = beacon.base_y + beacon.amplitude * (time.elapsed_secs() * 2.0).sin();
    }
}

/// The mod's scene and looks: camera, light, floor, the level, and models
/// for the player, spinners and beacons. Needs the renderer.
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(Update, (dress_player, dress_spinners, dress_beacons, follow_camera));
    }
}

const SKY: Color = Color::srgb(0.12, 0.05, 0.1);

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<PsxMaterial>>) {
    let arena: Arena = load_ron(Arena::PATH).unwrap_or_else(|e| panic!("arena: {e}"));
    commands.spawn((
        Camera3d::default(),
        Camera { order: 0, clear_color: ClearColorConfig::Custom(SKY), ..default() },
        PsxView,
        DistanceFog { color: SKY, falloff: FogFalloff::Linear { start: 8.0, end: 22.0 }, ..default() },
        Transform::from_xyz(0.0, 4.0, 7.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 1000.0, ..default() },
        Transform::from_xyz(-3.0, 5.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 250.0, ..default() });
    let [r, g, b] = arena.floor_colour;
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(arena.floor_half_size * 2.0, arena.floor_half_size * 2.0))),
        MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(r, g, b)))),
    ));
    commands.queue(SpawnLevel(arena.level));
}

/// A new character for the ported controller: a capsule with a nose that
/// shows which way it faces.
fn dress_player(
    mut commands: Commands,
    players: Query<Entity, Added<Player>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for player in &players {
        let nose = commands
            .spawn((
                Mesh3d(meshes.add(Cuboid::new(0.15, 0.15, 0.3))),
                MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.95, 0.8, 0.3)))),
                Transform::from_xyz(0.0, 0.2, -0.4),
            ))
            .id();
        commands
            .entity(player)
            .insert((
                Mesh3d(meshes.add(Capsule3d::new(0.35, 0.3))),
                MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.3, 0.75, 0.6)))),
            ))
            .add_child(nose);
    }
}

fn dress_spinners(
    mut commands: Commands,
    spinners: Query<Entity, Added<Spinner>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for spinner in &spinners {
        commands.entity(spinner).insert((
            Mesh3d(meshes.add(Torus::new(0.3, 0.6))),
            MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.85, 0.4, 0.3)))),
        ));
    }
}

fn dress_beacons(
    mut commands: Commands,
    beacons: Query<Entity, Added<Beacon>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for beacon in &beacons {
        commands.entity(beacon).insert((
            Mesh3d(meshes.add(Sphere::new(0.3).mesh().ico(0).unwrap())),
            MeshMaterial3d(materials.add(PsxMaterial { lit: false, ..PsxMaterial::color(Color::srgb(1.0, 0.9, 0.4)) })),
        ));
    }
}

fn follow_camera(
    player: Query<&Transform, With<Player>>,
    mut camera: Query<&mut Transform, (With<Camera3d>, Without<Player>)>,
) {
    let (Ok(player), Ok(mut camera)) = (player.single(), camera.single_mut()) else { return };
    let target = Vec3::new(player.translation.x, GROUND_Y, player.translation.z);
    camera.translation = target + Vec3::new(0.0, 3.5, 6.0);
    camera.look_at(target, Vec3::Y);
}

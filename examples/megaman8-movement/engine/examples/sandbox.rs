//! The sandbox: a second, tiny game built only from `engine` and the
//! hand-authored files in `engine/assets/sandbox/`. It must never depend on
//! `game/` or on anything extracted from the disc. If a feature of the port
//! cannot be used here without reaching into `game/`, the engine boundary is
//! in the wrong place: fix the boundary, not the sandbox.
//!
//! cargo run -p engine --example sandbox
//! cargo run -p engine --features dev-mcp --example sandbox -- --mcp --headless --software
//!
//! One room (a floor), one controllable character, one object type with a
//! trivial controller (a bobbing marker) and one trigger (it recolours the
//! floor). Every value comes from `room.ron` or `walker.ron`.
use bevy::prelude::*;
use engine::config::load_ron;
use engine::launcher::{self, Game};
use engine::prelude::*;
use engine::psx::{PsxMaterial, PsxView};
use serde::Deserialize;

const WALKER: ObjectTypeId = ObjectTypeId(1);
const BOBBER: ObjectTypeId = ObjectTypeId(2);
const RECOLOUR: ActionId = ActionId(1);

/// The sandbox's room: level data plus a floor size and colour.
#[derive(Deserialize)]
struct Room {
    floor_half_size: f32,
    floor_colour: [f32; 3],
    level: LevelData,
}

#[derive(Resource, Reflect, Deserialize)]
#[reflect(Resource)]
struct WalkerTuning {
    /// m/s.
    speed: f32,
}

#[derive(Component, Reflect, Default)]
#[reflect(Component)]
#[require(TriggerActivator)]
struct Walker;

/// Bobs up and down by `params[0]` mm.
#[derive(Component, Reflect)]
#[reflect(Component)]
struct Bobber {
    base_y: f32,
    amplitude: f32,
}

#[derive(Component)]
struct Floor;

fn main() {
    launcher::run(Game {
        name: "engine-sandbox",
        title: "Engine sandbox",
        region: Region::Ntsc,
        fields_per_tick: 1,
        add: |app| {
            app.add_plugins((EnginePlugin, TuningPlugin::<WalkerTuning>::new("sandbox/walker.ron")))
                .register_object_type(
                    WALKER,
                    ObjectTypeInfo::new("Walker", |e, _| {
                        e.insert(Walker);
                    })
                    .with_tuning::<WalkerTuning>(),
                )
                .register_object_type(
                    BOBBER,
                    ObjectTypeInfo::new("Bobber", |e, p| {
                        let amplitude = p.params.first().copied().unwrap_or(0) as f32 / 1000.0;
                        e.insert(Bobber { base_y: p.position.y, amplitude });
                    }),
                )
                .register_action(RECOLOUR, "recolour", recolour)
                .add_systems(Startup, setup)
                .add_systems(FixedUpdate, walk.in_set(EngineSet::Control))
                .add_systems(FixedUpdate, bob.in_set(EngineSet::Objects))
                .add_systems(Update, dress);
        },
    });
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<PsxMaterial>>) {
    let room: Room = load_ron("sandbox/room.ron").unwrap_or_else(|e| panic!("sandbox room: {e}"));
    commands.spawn((Camera3d::default(), PsxView, Transform::from_xyz(0.0, 6.0, 9.0).looking_at(Vec3::ZERO, Vec3::Y)));
    commands.spawn((
        DirectionalLight { illuminance: 1000.0, ..default() },
        Transform::default().looking_to(Vec3::new(-1.0, -2.0, -1.0), Vec3::Y),
    ));
    let [r, g, b] = room.floor_colour;
    commands.spawn((
        Floor,
        Mesh3d(meshes.add(Plane3d::default().mesh().size(room.floor_half_size * 2.0, room.floor_half_size * 2.0))),
        MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(r, g, b)))),
    ));
    commands.queue(SpawnLevel(room.level));
}

fn walk(
    pad: Res<ControllerState>,
    time: Res<Time>,
    tuning: Res<WalkerTuning>,
    mut q: Query<&mut Transform, With<Walker>>,
) {
    let dir = pad.direction();
    for mut t in &mut q {
        t.translation += Vec3::new(dir.x, 0.0, -dir.y) * tuning.speed * time.delta_secs();
    }
}

fn bob(clock: Res<GameClock>, mut q: Query<(&Bobber, &mut Transform)>) {
    for (bobber, mut t) in &mut q {
        t.translation.y = bobber.base_y + bobber.amplitude * (clock.frame as f32 * 0.1).sin();
    }
}

/// args: [r, g, b] in 0..=255.
fn recolour(
    In(call): In<ActionCall>,
    floors: Query<&MeshMaterial3d<PsxMaterial>, With<Floor>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    let channel = |i: usize| call.args.get(i).copied().unwrap_or(255).clamp(0, 255) as u8;
    for floor in &floors {
        if let Some(mut material) = materials.get_mut(&floor.0) {
            material.color = Color::srgb_u8(channel(0), channel(1), channel(2));
        }
    }
}

fn dress(
    mut commands: Commands,
    walkers: Query<Entity, Added<Walker>>,
    bobbers: Query<Entity, Added<Bobber>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for entity in &walkers {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Capsule3d::new(0.3, 1.0))),
            MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.9, 0.8, 0.2)))),
        ));
    }
    for entity in &bobbers {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(0.6, 0.6, 0.6))),
            MeshMaterial3d(materials.add(PsxMaterial::color(Color::srgb(0.3, 0.5, 0.9)))),
        ));
    }
}

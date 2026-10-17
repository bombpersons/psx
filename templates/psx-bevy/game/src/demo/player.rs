//! The demo's player rules: a per-type controller registered with the
//! engine's object type registry, with its design numbers in a tuning file
//! (`assets/demo/player.ron`). No rendering, so it runs in headless tests
//! (`tests/player.rs`).
//!
//! Tuning is in natural units (metres, seconds, radians) and systems scale it
//! by the tick length, so it reads the same at any `TickRate`. A port would
//! cite where each value came from; see AGENTS.md, "From evidence to
//! idiomatic code".
use bevy::prelude::*;
use engine::controller::Button;
use engine::prelude::*;
use serde::Deserialize;

/// The player's type id in level data.
pub const PLAYER_TYPE: ObjectTypeId = ObjectTypeId(1);
/// Height of the crate's centre when it rests on the floor.
pub const GROUND_Y: f32 = 0.5;
/// Half the size of the walkable square, m.
pub const ARENA_HALF_SIZE: f32 = 10.0;

/// Design numbers, read from `assets/demo/player.ron`.
#[derive(Resource, Reflect, Deserialize, Clone, Debug)]
#[reflect(Resource)]
pub struct PlayerTuning {
    /// Walking speed, m/s.
    pub walk_speed: f32,
    /// L1/R1 turning speed, rad/s.
    pub turn_speed: f32,
    /// Take-off speed of a hop, m/s.
    pub hop_speed: f32,
    /// Downward acceleration while airborne, m/s².
    pub gravity: f32,
}

impl PlayerTuning {
    pub const PATH: &str = "demo/player.ron";
}

#[derive(Component, Reflect, Default)]
#[reflect(Component)]
#[require(Hop, TriggerActivator, Transform = Transform::from_xyz(0.0, GROUND_Y, 0.0))]
pub struct Player;

/// Vertical movement as a small state machine: one enum, transitions in one
/// system, no per-tick dispatch on raw state numbers.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq, Default)]
#[reflect(Component)]
pub enum Hop {
    #[default]
    Grounded,
    /// Rising or falling; velocity is upward, m/s.
    Airborne { velocity: f32 },
}

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(TuningPlugin::<PlayerTuning>::new(PlayerTuning::PATH))
            .register_object_type(
                PLAYER_TYPE,
                ObjectTypeInfo::new("Player", |entity, _| {
                    entity.insert(Player);
                })
                .with_tuning::<PlayerTuning>(),
            )
            .add_systems(FixedUpdate, (walk, hop).chain().in_set(EngineSet::Control));
    }
}

/// D-pad or stick moves on the floor plane; L1/R1 turn.
fn walk(
    pad: Res<ControllerState>,
    time: Res<Time>,
    tuning: Res<PlayerTuning>,
    mut player: Query<&mut Transform, With<Player>>,
) {
    let dt = time.delta_secs();
    let dir = pad.direction();
    let turn = f32::from(u8::from(pad.held(Button::L1))) - f32::from(u8::from(pad.held(Button::R1)));
    for mut t in &mut player {
        t.translation += Vec3::new(dir.x, 0.0, -dir.y) * tuning.walk_speed * dt;
        t.translation.x = t.translation.x.clamp(-ARENA_HALF_SIZE, ARENA_HALF_SIZE);
        t.translation.z = t.translation.z.clamp(-ARENA_HALF_SIZE, ARENA_HALF_SIZE);
        t.rotate_y(tuning.turn_speed * turn * dt);
    }
}

/// Cross starts a hop on the tick it is pressed (an edge, so holding it does
/// not hop again on landing); gravity brings the crate back to the floor.
fn hop(
    pad: Res<ControllerState>,
    time: Res<Time>,
    tuning: Res<PlayerTuning>,
    mut player: Query<(&mut Hop, &mut Transform), With<Player>>,
) {
    let dt = time.delta_secs();
    for (mut hop, mut t) in &mut player {
        *hop = match *hop {
            Hop::Grounded if pad.pressed(Button::Cross) => Hop::Airborne { velocity: tuning.hop_speed },
            Hop::Grounded => Hop::Grounded,
            Hop::Airborne { velocity } => {
                let velocity = velocity - tuning.gravity * dt;
                t.translation.y += velocity * dt;
                if t.translation.y <= GROUND_Y {
                    t.translation.y = GROUND_Y;
                    Hop::Grounded
                } else {
                    Hop::Airborne { velocity }
                }
            }
        };
    }
}

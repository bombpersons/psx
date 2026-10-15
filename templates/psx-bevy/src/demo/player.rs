//! The demo's player rules: plain components and systems with no rendering,
//! so they run in headless tests (`tests/player.rs`).
//!
//! Constants are in natural units (metres, seconds, radians) and systems scale
//! them by the tick length, so they read the same at any [`crate::TickRate`].
//! A reimplementation would cite where each value came from; see AGENTS.md,
//! "From evidence to idiomatic code".
use crate::prelude::*;
use bevy::prelude::*;

/// Height of the crate's centre when it rests on the floor.
pub const GROUND_Y: f32 = 0.5;
/// Walking speed, m/s.
pub const WALK_SPEED: f32 = 4.8;
/// L1/R1 turning speed, rad/s.
pub const TURN_SPEED: f32 = 1.8;
/// Take-off speed of a hop, m/s.
pub const HOP_SPEED: f32 = 5.0;
/// Downward acceleration while airborne, m/s².
pub const GRAVITY: f32 = 20.0;
/// Half the size of the walkable square, m.
pub const ARENA_HALF_SIZE: f32 = 10.0;

#[derive(Component, Reflect, Default)]
#[reflect(Component)]
#[require(Hop, Transform = Transform::from_xyz(0.0, GROUND_Y, 0.0))]
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
        app.add_systems(FixedUpdate, (walk, hop).chain().in_set(GameSystems::Game));
    }
}

/// D-pad or stick moves on the floor plane; L1/R1 turn.
fn walk(pad: Res<ControllerState>, time: Res<Time>, mut player: Query<&mut Transform, With<Player>>) {
    let dt = time.delta_secs();
    let dir = pad.direction();
    let turn = f32::from(u8::from(pad.held(Button::L1))) - f32::from(u8::from(pad.held(Button::R1)));
    for mut t in &mut player {
        t.translation += Vec3::new(dir.x, 0.0, -dir.y) * WALK_SPEED * dt;
        t.translation.x = t.translation.x.clamp(-ARENA_HALF_SIZE, ARENA_HALF_SIZE);
        t.translation.z = t.translation.z.clamp(-ARENA_HALF_SIZE, ARENA_HALF_SIZE);
        t.rotate_y(TURN_SPEED * turn * dt);
    }
}

/// Cross starts a hop on the tick it is pressed (an edge, so holding it does
/// not hop again on landing); gravity brings the crate back to the floor.
fn hop(pad: Res<ControllerState>, time: Res<Time>, mut player: Query<(&mut Hop, &mut Transform), With<Player>>) {
    let dt = time.delta_secs();
    for (mut hop, mut t) in &mut player {
        *hop = match *hop {
            Hop::Grounded if pad.pressed(Button::Cross) => Hop::Airborne { velocity: HOP_SPEED },
            Hop::Grounded => Hop::Grounded,
            Hop::Airborne { velocity } => {
                let velocity = velocity - GRAVITY * dt;
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

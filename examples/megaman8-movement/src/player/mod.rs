//! Mega Man: one entity with a position in stage pixels, a velocity, a facing
//! and a movement state. `movement` holds the rules; `sprite` draws him.
pub mod movement;
mod sprite;

use crate::stage::StagePos;
use bevy::prelude::*;
use movement::{Grounded, MovementPlugin, WalkGrace};

#[derive(Component, Reflect, Default)]
#[reflect(Component)]
pub struct Player;

/// Pixels per tick, y down (stage space).
#[derive(Component, Reflect, Default, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct Velocity(pub Vec2);

#[derive(Component, Reflect, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Component)]
pub enum Facing {
    Left,
    #[default]
    Right,
}

impl Facing {
    pub fn sign(self) -> f32 {
        match self {
            Facing::Left => -1.0,
            Facing::Right => 1.0,
        }
    }
}

/// The movement state (the original's state byte at object +0x02:
/// 0 stand, 1 step, 2 walk, 3 jump, 4 fall, 5 land).
#[derive(Component, Reflect, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Component)]
pub enum PlayerState {
    #[default]
    Stand,
    /// The short "inching" step before a walk; 0 = just entered.
    Step {
        ticks_left: u8,
    },
    Walk,
    /// Rising; `launched` is false on the tick the jump starts.
    Jump {
        launched: bool,
    },
    Fall,
    /// Landing pose; behaves like standing.
    Land {
        ticks_left: u8,
    },
}

impl PlayerState {
    pub fn on_ground(self) -> bool {
        matches!(self, Self::Stand | Self::Step { .. } | Self::Walk | Self::Land { .. })
    }
}

/// Where Mega Man starts in the intro stage (object +0x0C/+0x10 at the
/// in-stage save state: x = 416, y = 441, the sprite centre).
pub const START: Vec2 = Vec2::new(416.0, 441.0);

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((MovementPlugin, sprite::PlayerSpritePlugin)).add_systems(Startup, spawn_player);
    }
}

fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Name::new("Mega Man"),
        Player,
        StagePos(START),
        Velocity::default(),
        PlayerState::Stand,
        Facing::Right,
        Grounded(true),
        WalkGrace::default(),
        Transform::default(),
        Visibility::default(),
    ));
}

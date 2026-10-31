//! The phases of one logic tick, in order. Every character's systems join a
//! phase; an ability that has to run before or after another one orders its
//! own set against that one's inside the phase (see the abilities' plugins).
//!
//! The order is the original's object loop (0x8002C790): the animation and
//! the vertical motion before an object's script, collision after it.
use bevy::prelude::*;
use psx_bevy::prelude::*;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum TickPhase {
    /// Controllers turn the pad into what the body reads this tick.
    Input,
    /// Animations advance a frame.
    Animate,
    /// Rising and falling.
    Physics,
    /// Reactions to what the physics did (landing).
    Reactions,
    /// Feet follow the floor.
    Ground,
    /// The action chosen last tick takes over.
    Latch,
    /// The free action's control: moving, turning, starting other actions.
    Control,
    /// Other actions' bodies.
    Actions,
    /// Countdowns run down.
    Timers,
    /// Collision: the floor for the next tick.
    Collide,
}

/// Puts the phases in order inside [`GameSystems::Game`].
pub struct TickPlugin;

impl Plugin for TickPlugin {
    fn build(&self, app: &mut App) {
        use TickPhase::*;
        app.configure_sets(
            FixedUpdate,
            (Input, Animate, Physics, Reactions, Ground, Latch, Control, Actions, Timers, Collide)
                .chain()
                .in_set(GameSystems::Game),
        );
    }
}

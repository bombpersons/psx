//! Croc: Legend of the Gobbos, player movement in a placeholder level.
//!
//! [`movement`] holds the rules recovered from the original; [`scene`] is a
//! stand-in world (no original assets) to walk around in.
pub mod movement;
mod scene;
#[cfg(test)]
mod verify;

use bevy::prelude::*;

pub struct CrocPlugin;

impl Plugin for CrocPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((movement::CrocMovementPlugin, scene::ScenePlugin));
    }
}

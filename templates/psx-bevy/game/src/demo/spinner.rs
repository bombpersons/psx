//! The demo's spinners, one feature as one plugin: anything with [`Spin`]
//! turns. Rules only; the scene dresses them ([`super::DemoPlugin`]).
use bevy::prelude::*;
use psx_bevy::prelude::*;

pub struct SpinnerPlugin;

impl Plugin for SpinnerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(FixedUpdate, spin.in_set(GameSystems::Game));
    }
}

/// Turning speed, rad/s.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Spin(pub f32);

/// A spinning sphere placed by the level; its colour is 0 or 1.
#[derive(Component, Reflect)]
#[reflect(Component)]
#[require(Transform)]
pub struct Spinner {
    pub colour: i32,
}

fn spin(time: Res<Time>, mut q: Query<(&Spin, &mut Transform)>) {
    for (spin, mut t) in &mut q {
        t.rotate_y(spin.0 * time.delta_secs());
    }
}

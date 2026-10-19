//! Headless gameplay tests: exact logic ticks with scripted buttons, no
//! window or GPU. The way to compare rules against trajectories recorded
//! from the original game (see the game's `tests/player.rs`):
//!
//! ```
//! use bevy::prelude::*;
//! use psx_bevy::prelude::*;
//! use psx_bevy::controller::Button;
//! use psx_bevy::testing::{TickApp, headless_app};
//!
//! // In a game these come from a feature's rules plugin (`app.add_plugins(...)`).
//! #[derive(Component)]
//! struct Player;
//!
//! fn walk(pad: Res<ControllerState>, mut players: Query<&mut Transform, With<Player>>) {
//!     for mut transform in &mut players {
//!         transform.translation.x += pad.direction().x;
//!     }
//! }
//!
//! let mut app = headless_app(TickRate::every_field(Region::Ntsc));
//! app.add_systems(FixedUpdate, walk.in_set(GameSystems::Game));
//! let player = app.world_mut().spawn((Player, Transform::default())).id();
//! app.run_ticks(10, &[Button::Right]);
//! assert_eq!(app.world().get::<Transform>(player).unwrap().translation.x, 10.0);
//! ```
//!
//! Only add rules plugins: anything that loads assets or spawns meshes needs
//! the renderer. Keeping rules and presentation in separate plugins is what
//! makes them testable.
//!
//! To check rules against the original game, replay a psx recording's input
//! and compare per-tick values with [`compare`] (see [`trajectory`]).
use crate::controller::Button;
use crate::shell::{GameClock, InjectedInput, ShellPlugin, TickRate};
use bevy::{prelude::*, time::TimeUpdateStrategy};

#[cfg(feature = "dev-mcp")]
pub mod mcp;
pub mod trajectory;
pub use crate::shell::InjectedMouse;
pub use trajectory::{Alignment, Comparison, Replay, Trajectory, compare};

/// One tick of injected input: pad buttons and the mouse. `Vec<Button>`
/// converts into one with the mouse left alone.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TickInput {
    pub buttons: Vec<Button>,
    pub mouse: InjectedMouse,
}

impl From<Vec<Button>> for TickInput {
    fn from(buttons: Vec<Button>) -> Self {
        Self { buttons, mouse: default() }
    }
}

impl From<&[Button]> for TickInput {
    fn from(buttons: &[Button]) -> Self {
        buttons.to_vec().into()
    }
}

/// An app with Bevy's minimal plugins and the template's tick loop. Each
/// [`TickApp::run_tick`] advances time by exactly one tick.
pub fn headless_app(tick: TickRate) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, ShellPlugin { tick }))
        .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period()));
    app
}

pub trait TickApp {
    /// Runs one logic tick with exactly these buttons held. `Res<Time>` in
    /// `FixedUpdate` reports one tick's duration, as in the game.
    fn run_tick(&mut self, buttons: &[Button]);

    /// Runs `ticks` ticks with the same buttons held.
    fn run_ticks(&mut self, ticks: u32, buttons: &[Button]) {
        for _ in 0..ticks {
            self.run_tick(buttons);
        }
    }

    /// Runs one logic tick with exactly this input, mouse included.
    fn run_input(&mut self, input: &TickInput);
}

impl TickApp for App {
    fn run_tick(&mut self, buttons: &[Button]) {
        self.run_input(&buttons.into());
    }

    fn run_input(&mut self, input: &TickInput) {
        {
            let mut injected = self.world_mut().resource_mut::<InjectedInput>();
            injected.set(&input.buttons);
            injected.mouse = input.mouse;
        }
        let start = self.world().resource::<GameClock>().frame;
        // The very first update only starts the clock; later ones each
        // advance exactly one tick.
        for _ in 0..2 {
            self.update();
            if self.world().resource::<GameClock>().frame != start {
                break;
            }
        }
        let ran = self.world().resource::<GameClock>().frame - start;
        assert_eq!(ran, 1, "one update should run exactly one fixed tick");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psx::Region;

    #[test]
    fn one_update_is_one_tick() {
        for rate in [TickRate::every_field(Region::Ntsc), TickRate::every_nth_field(Region::Pal, 2)] {
            let mut app = headless_app(rate);
            app.run_ticks(100, &[]);
            assert_eq!(app.world().resource::<GameClock>().frame, 100);
        }
    }

    #[test]
    fn scripted_buttons_reach_the_controller() {
        let mut app = headless_app(TickRate::default());
        app.run_tick(&[Button::Cross]);
        let pad = app.world().resource::<crate::controller::ControllerState>();
        assert!(pad.pressed(Button::Cross));
        app.run_tick(&[]);
        let pad = app.world().resource::<crate::controller::ControllerState>();
        assert!(pad.released(Button::Cross));
    }
}

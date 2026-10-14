//! Native game shell: PlayStation-look rendering, fixed-step logic and controllers.
mod controller;
mod demo;
#[cfg(feature = "dev-mcp")]
mod dev_mcp;
mod psx;

use bevy::prelude::*;
use bevy::render::{RenderPlugin, settings::WgpuSettings};
use controller::{ControllerBindings, ControllerState};
use psx::{PsxPlugin, PsxScreen, Region};

#[derive(Resource, Default)]
pub struct GameClock {
    pub frame: u64,
    pub ticking: bool,
}

/// Simulation runs in `FixedUpdate` at the region's field rate (60/50 Hz).
/// Put gameplay in `GameSystems::Game`; developer stepping pauses exactly there.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum GameSystems {
    Input,
    Game,
    Finish,
}

fn main() {
    let mcp = std::env::args().any(|arg| arg == "--mcp");
    let software = std::env::args().any(|arg| arg == "--software");
    #[cfg(not(feature = "dev-mcp"))]
    if mcp {
        eprintln!("--mcp requires a developer build: cargo run --features dev-mcp -- --mcp");
        std::process::exit(2);
    }
    let screen = PsxScreen { width: 320, height: 240, region: Region::Ntsc, ..default() };
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "PSX game template — arrows, Z/X/C/S, Q/E, Enter".into(),
                    resolution: (screen.width * 3, screen.height * 3).into(),
                    ..default()
                }),
                ..default()
            })
            .set(ImagePlugin::default_nearest())
            // Prefers hardware; --software selects a fallback adapter (e.g. Mesa lavapipe).
            .set(RenderPlugin {
                render_creation: WgpuSettings { force_fallback_adapter: software, ..default() }.into(),
                synchronous_pipeline_compilation: true,
                ..default()
            }),
    )
    .insert_resource(Time::<Fixed>::from_hz(screen.region.hz()))
    .add_plugins(PsxPlugin { screen })
    .init_resource::<GameClock>()
    .init_resource::<ControllerBindings>()
    .init_resource::<ControllerState>()
    .configure_sets(FixedUpdate, (GameSystems::Input, GameSystems::Game, GameSystems::Finish).chain())
    .configure_sets(FixedUpdate, GameSystems::Game.run_if(|clock: Res<GameClock>| clock.ticking))
    .add_systems(FixedUpdate, begin_tick.in_set(GameSystems::Input))
    .add_systems(FixedUpdate, finish_tick.in_set(GameSystems::Finish))
    .add_plugins(demo::DemoPlugin);
    #[cfg(feature = "dev-mcp")]
    if mcp {
        app.add_plugins(dev_mcp::GameMcpPlugin);
    }
    app.run();
}

fn begin_tick(
    keys: Res<ButtonInput<KeyCode>>,
    pads: Query<(Entity, &Gamepad)>,
    bindings: Res<ControllerBindings>,
    mut state: ResMut<ControllerState>,
    mut clock: ResMut<GameClock>,
    #[cfg(feature = "dev-mcp")] mut control: Option<ResMut<dev_mcp::Control>>,
) {
    clock.ticking = true;
    #[cfg(not(feature = "dev-mcp"))]
    let injected = 0;
    #[cfg(feature = "dev-mcp")]
    let mut injected = 0;
    #[cfg(feature = "dev-mcp")]
    if let Some(ref mut control) = control {
        control.discard_cancelled_step();
        clock.ticking = !control.paused || control.has_step();
        injected = control.buttons;
    }
    if clock.ticking {
        let pad = pads.iter().min_by_key(|(entity, _)| entity.to_bits()).map(|(_, pad)| pad);
        let (buttons, left, right) = controller::sample(&keys, pad, &bindings);
        state.update(buttons | injected, left, right);
    }
}

fn finish_tick(
    mut clock: ResMut<GameClock>,
    #[cfg(feature = "dev-mcp")] control: Option<ResMut<dev_mcp::Control>>,
) {
    if !clock.ticking {
        return;
    }
    clock.frame += 1;
    #[cfg(feature = "dev-mcp")]
    if let Some(mut control) = control {
        control.finish_tick(clock.frame);
    }
}

//! Native game shell: PlayStation-look rendering, fixed-step logic and controllers.
mod controller;
mod croc;
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
    let args: Vec<String> = std::env::args().collect();
    let mcp = args.iter().any(|arg| arg == "--mcp");
    let software = args.iter().any(|arg| arg == "--software");
    // --scene PATH (under assets/): view a psx-toolkit frame capture instead of the game.
    let scene = args.iter().position(|a| a == "--scene").and_then(|i| args.get(i + 1)).cloned();
    // --painter: draw its 3D elements in capture order without depth, as the PS1 did (P toggles).
    let depth = if args.iter().any(|a| a == "--painter") { psx::PsxSceneDepth::Painter } else { default() };
    #[cfg(not(feature = "dev-mcp"))]
    if mcp {
        eprintln!("--mcp requires a developer build: cargo run --features dev-mcp -- --mcp");
        std::process::exit(2);
    }
    let screen = PsxScreen { width: 320, height: 240, region: Region::Pal, ..default() };
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Croc movement — arrows walk/turn, X jump, Q/E sidestep (air: turn), S toggles PS1 look".into(),
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
    // Croc's game logic runs at half the PAL field rate (one tick per two
    // vblanks) and samples the pad at that rate too, so the fixed step is 25 Hz
    // rather than 50 Hz: button edges then match the original's.
    .insert_resource(Time::<Fixed>::from_hz(screen.region.hz() / croc::movement::VBLANKS_PER_TICK))
    .add_plugins(PsxPlugin { screen })
    .init_resource::<GameClock>()
    .init_resource::<ControllerBindings>()
    .init_resource::<ControllerState>()
    .configure_sets(FixedUpdate, (GameSystems::Input, GameSystems::Game, GameSystems::Finish).chain())
    .configure_sets(FixedUpdate, GameSystems::Game.run_if(|clock: Res<GameClock>| clock.ticking))
    .add_systems(FixedUpdate, begin_tick.in_set(GameSystems::Input))
    .add_systems(FixedUpdate, finish_tick.in_set(GameSystems::Finish));
    match scene {
        Some(path) => {
            app.add_systems(Startup, move |mut commands: Commands, assets: Res<AssetServer>| {
                // Background 2D (drawn before any 3D), lifted 3D geometry, then 2D on top (HUD).
                commands.spawn((
                    Camera2d,
                    psx::PsxView,
                    Camera { order: -1, ..default() },
                    bevy::camera::visibility::RenderLayers::layer(psx::SCENE_BACKGROUND_LAYER),
                ));
                commands.spawn((
                    Camera3d::default(),
                    psx::PsxView,
                    psx::PsxSceneCamera,
                    Camera { order: 0, clear_color: ClearColorConfig::None, ..default() },
                ));
                commands.spawn((Camera2d, psx::PsxView, Camera { order: 1, clear_color: ClearColorConfig::None, ..default() }));
                commands.spawn((psx::PsxSceneRoot(assets.load(path.clone())), depth, Transform::default(), Visibility::default()));
            })
            .add_systems(Update, toggle_scene_depth);
        }
        None => {
            app.add_plugins(croc::CrocPlugin);
        }
    }
    #[cfg(feature = "dev-mcp")]
    if mcp {
        app.add_plugins(dev_mcp::GameMcpPlugin);
    }
    app.run();
}

/// Scene viewer: P switches between depth-tested and painter's-order 3D.
fn toggle_scene_depth(keys: Res<ButtonInput<KeyCode>>, mut roots: Query<&mut psx::PsxSceneDepth>) {
    if !keys.just_pressed(KeyCode::KeyP) {
        return;
    }
    for mut depth in &mut roots {
        *depth = match *depth {
            psx::PsxSceneDepth::Tested => psx::PsxSceneDepth::Painter,
            psx::PsxSceneDepth::Painter => psx::PsxSceneDepth::Tested,
        };
    }
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

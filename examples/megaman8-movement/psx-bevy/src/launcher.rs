//! The executable's side of a game: window, renderer, headless mode and the
//! command line every game built on the template shares. A game's `main.rs`
//! describes itself with [`Game`] and calls [`run`]:
//!
//! ```no_run
//! use bevy::math::UVec2;
//! use psx_bevy::{launcher::{self, Game}, psx::Region};
//!
//! fn main() {
//!     launcher::run(Game {
//!         name: env!("CARGO_PKG_NAME"),
//!         title: "My game",
//!         region: Region::Pal,
//!         fields_per_tick: 2,
//!         screen: UVec2::new(320, 240),
//!         add: |app| { /* app.add_plugins(game::GamePlugin); */ },
//!     });
//! }
//! ```
//!
//! Flags: `--mcp` (developer MCP, needs the `dev-mcp` feature), `--headless`,
//! `--software`, `--region ntsc|pal`, `--scene PATH` [`--painter`],
//! `--model PATH.tmd [--texture PATH.tim]... [--cull]`, `--controls`,
//! `--bindings PATH`, `--no-dither`, `--no-quantize`.
use crate::controller::BindingStore;
use crate::controls::ControlsPlugin;
use crate::model_viewer::ModelViewerPlugin;
use crate::psx::{PsxLook, PsxPlugin, PsxSceneDepth, PsxScreen, Region};
use crate::shell::{ShellPlugin, TickRate};
use crate::viewer::SceneViewerPlugin;
use bevy::prelude::*;
use bevy::render::{RenderPlugin, settings::WgpuSettings};

/// What the launcher needs to know about a game.
pub struct Game<F: FnOnce(&mut App)> {
    /// Package name: names the settings folder (controls.json).
    pub name: &'static str,
    /// Window title.
    pub title: &'static str,
    /// The original's video region; `--region` overrides it.
    pub region: Region,
    /// Video fields per logic tick (2 when the original waits two vblanks).
    pub fields_per_tick: u32,
    /// The original's display size in pixels, which sizes [`PsxScreen`] and
    /// the window (at 3x). 320x240 for most games; read the real one from
    /// the original's display setup (the GPU's display range and mode).
    pub screen: UVec2,
    /// Adds the game's plugins. Not called for `--scene` or `--model`.
    pub add: F,
}

/// The parsed command line.
pub struct Args(Vec<String>);

impl Args {
    pub fn from_env() -> Self {
        Self(std::env::args().collect())
    }
    pub fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|arg| arg == name)
    }
    pub fn value(&self, name: &str) -> Option<String> {
        self.0.iter().position(|arg| arg == name).and_then(|i| self.0.get(i + 1)).cloned()
    }
    pub fn values(&self, name: &str) -> Vec<String> {
        self.0.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect()
    }
}

/// Builds the app from the command line and runs it.
pub fn run<F: FnOnce(&mut App)>(game: Game<F>) -> AppExit {
    app(game, &Args::from_env()).run()
}

/// Builds the app without running it: window or headless runner, renderer,
/// [`PsxPlugin`], [`ShellPlugin`], then either a viewer or the game with the
/// controls menu, then the developer MCP when `--mcp` is given.
pub fn app<F: FnOnce(&mut App)>(game: Game<F>, args: &Args) -> App {
    let mcp = args.flag("--mcp");
    let headless = args.flag("--headless");
    #[cfg(not(feature = "dev-mcp"))]
    if mcp {
        eprintln!("--mcp requires a developer build: cargo run --features dev-mcp -- --mcp");
        std::process::exit(2);
    }
    // --region ntsc|pal: override the game's region (display and tick rate).
    let region = match args.value("--region").as_deref() {
        None => game.region,
        Some("ntsc") => Region::Ntsc,
        Some("pal") => Region::Pal,
        Some(other) => {
            eprintln!("--region must be ntsc or pal, not {other}");
            std::process::exit(2);
        }
    };
    // --scene PATH (under assets/): view a psx-toolkit frame capture instead of the game.
    let scene = args.value("--scene");
    // --painter: draw its 3D elements in capture order without depth, as the PS1 did (P toggles).
    let depth = if args.flag("--painter") { PsxSceneDepth::Painter } else { default() };
    // --no-dither / --no-quantize: switch PsxLook effects off (e.g. to compare against a capture).
    let look = PsxLook { dither: !args.flag("--no-dither"), quantize: !args.flag("--no-quantize"), ..default() };
    // --model PATH.tmd [--texture PATH.tim]... [--cull] (under assets/): view an extracted original model.
    let model = args.value("--model");
    let textures = args.values("--texture");
    let screen = PsxScreen { width: game.screen.x, height: game.screen.y, region, ..default() };

    let mut app = App::new();
    let mut plugins = DefaultPlugins
        .set(WindowPlugin {
            primary_window: (!headless).then(|| Window {
                title: game.title.into(),
                resolution: (screen.width * 3, screen.height * 3).into(),
                ..default()
            }),
            exit_condition: if headless {
                bevy::window::ExitCondition::DontExit
            } else {
                bevy::window::ExitCondition::OnAllClosed
            },
            ..default()
        })
        .set(ImagePlugin::default_nearest())
        // Prefers hardware; --software selects a fallback adapter (e.g. Mesa lavapipe).
        .set(RenderPlugin {
            render_creation: WgpuSettings { force_fallback_adapter: args.flag("--software"), ..default() }.into(),
            synchronous_pipeline_compilation: true,
            ..default()
        });
    if headless {
        plugins = plugins.disable::<bevy::winit::WinitPlugin>();
        app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(std::time::Duration::from_secs_f64(
            1.0 / region.hz(),
        )));
    }
    app.add_plugins(plugins)
        .add_plugins((
            PsxPlugin { screen },
            ShellPlugin { tick: TickRate::every_nth_field(region, game.fields_per_tick) },
        ))
        .insert_resource(look);
    match (scene, model) {
        (Some(path), _) => {
            app.add_plugins(SceneViewerPlugin { path, depth });
        }
        (None, Some(path)) => {
            app.add_plugins(ModelViewerPlugin { path, textures, cull: args.flag("--cull") });
        }
        (None, None) => {
            app.add_plugins(ControlsPlugin {
                path: args
                    .value("--bindings")
                    .map(std::path::PathBuf::from)
                    .or_else(|| BindingStore::default_path(game.name)),
                initially_open: args.flag("--controls"),
                ..default()
            });
            (game.add)(&mut app);
        }
    }
    #[cfg(feature = "dev-mcp")]
    if mcp {
        app.add_plugins(crate::dev_mcp::GameMcpPlugin);
    }
    app
}

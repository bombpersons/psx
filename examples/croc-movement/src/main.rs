//! The executable: window, renderer and command line. The game itself is the
//! library in `src/lib.rs` (crate `game`).
use bevy::prelude::*;
use bevy::render::{RenderPlugin, settings::WgpuSettings};
use game::model_viewer::ModelViewerPlugin;
use game::psx::{PsxLook, PsxPlugin, PsxSceneDepth, PsxScreen, Region};
use game::shell::ShellPlugin;
use game::viewer::SceneViewerPlugin;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let value = |name: &str| args.iter().position(|arg| arg == name).and_then(|i| args.get(i + 1)).cloned();
    let mcp = flag("--mcp");
    #[cfg(not(feature = "dev-mcp"))]
    if mcp {
        eprintln!("--mcp requires a developer build: cargo run --features dev-mcp -- --mcp");
        std::process::exit(2);
    }
    // --region ntsc|pal: override `game::REGION` (display and tick rate).
    let region = match value("--region").as_deref() {
        None => game::REGION,
        Some("ntsc") => Region::Ntsc,
        Some("pal") => Region::Pal,
        Some(other) => {
            eprintln!("--region must be ntsc or pal, not {other}");
            std::process::exit(2);
        }
    };
    // --scene PATH (under assets/): view a psx-toolkit frame capture instead of the game.
    let scene = value("--scene");
    // --painter: draw its 3D elements in capture order without depth, as the PS1 did (P toggles).
    let depth = if flag("--painter") { PsxSceneDepth::Painter } else { default() };
    // --no-dither / --no-quantize: switch PsxLook effects off (e.g. to compare against a capture).
    let look = PsxLook { dither: !flag("--no-dither"), quantize: !flag("--no-quantize"), ..default() };

    // --model PATH.tmd [--texture PATH.tim]... [--cull] (under assets/): view an extracted original model.
    let model = value("--model");
    let textures: Vec<String> = args.windows(2).filter(|w| w[0] == "--texture").map(|w| w[1].clone()).collect();
    let screen = PsxScreen { width: 320, height: 240, region, ..default() };
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: game::TITLE.into(),
                    resolution: (screen.width * 3, screen.height * 3).into(),
                    ..default()
                }),
                ..default()
            })
            .set(ImagePlugin::default_nearest())
            // Prefers hardware; --software selects a fallback adapter (e.g. Mesa lavapipe).
            .set(RenderPlugin {
                render_creation: WgpuSettings { force_fallback_adapter: flag("--software"), ..default() }.into(),
                synchronous_pipeline_compilation: true,
                ..default()
            }),
    )
    .add_plugins((PsxPlugin { screen }, ShellPlugin { tick: game::tick_rate(region) }))
    .insert_resource(look);
    match (scene, model) {
        (Some(path), _) => app.add_plugins(SceneViewerPlugin { path, depth }),
        (None, Some(path)) => app.add_plugins(ModelViewerPlugin { path, textures, cull: flag("--cull") }),
        (None, None) => app.add_plugins(game::GamePlugin),
    };
    #[cfg(feature = "dev-mcp")]
    if mcp {
        app.add_plugins(game::dev_mcp::GameMcpPlugin);
    }
    app.run();
}

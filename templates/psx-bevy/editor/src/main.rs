//! The editor executable: `cargo run -p editor -- [file.ron] [flags]`.
//!
//! `--mcp` serves the editor's MCP on stdio. Tools > Agents serves it over HTTP,
//! or `--mcp-listen 127.0.0.1:7878` [`--mcp-token TOKEN`] for one run; see `mcp`.
//! `--headless` (with `--mcp`) runs without an OS window, `--size 1280x800`
//! sets the window size and `--software` picks a software renderer. Developer
//! builds (`--features dev-mcp`) add testing tools on stdio.
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::WgpuSettings;
use bevy::window::{ExitCondition, WindowResolution};
use editor::EditorPlugin;
use editor::mcp::EditorMcpPlugin;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut open, mut stdio, mut listen, mut token) = (None, false, None, None);
    let (mut headless, mut software, mut size) = (false, false, (1440, 900));
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match arg.as_str() {
            "--mcp" => stdio = true,
            "--mcp-listen" => {
                let addr = value("--mcp-listen");
                listen = Some(addr.parse().unwrap_or_else(|_| fail("--mcp-listen takes an address: 127.0.0.1:7878")));
            }
            "--mcp-token" => token = Some(value("--mcp-token")),
            "--headless" => headless = true,
            "--software" => software = true,
            "--size" => {
                let text = value("--size");
                size = text
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .unwrap_or_else(|| fail("--size takes WIDTHxHEIGHT: 1280x800"));
            }
            flag if flag.starts_with("--") => fail(&format!("unknown flag {flag}")),
            file => open = Some(PathBuf::from(file)),
        }
    }
    if headless && !stdio {
        fail("--headless needs --mcp: nothing else could use a windowless editor");
    }

    let mut resolution = WindowResolution::new(size.0, size.1);
    if headless {
        // No OS window: the window is only a size, drawn into an image (see `mcp`).
        resolution = resolution.with_scale_factor_override(1.0);
    }
    let mut plugins = DefaultPlugins
        .set(WindowPlugin {
            primary_window: Some(Window { title: "Editor".into(), resolution, ..default() }),
            // Closing asks about unsaved changes first (`EditorPlugin`).
            close_when_requested: false,
            exit_condition: if headless { ExitCondition::DontExit } else { ExitCondition::OnAllClosed },
            ..default()
        })
        .set(RenderPlugin {
            render_creation: WgpuSettings { force_fallback_adapter: software, ..default() }.into(),
            synchronous_pipeline_compilation: headless,
            ..default()
        });
    let mut app = App::new();
    if headless {
        plugins = plugins.disable::<bevy::winit::WinitPlugin>();
        app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(std::time::Duration::from_secs_f64(1.0 / 60.0)));
    }
    app.add_plugins(plugins)
        .add_plugins(EditorPlugin { open })
        .add_plugins(EditorMcpPlugin { stdio, listen, token, headless })
        .run();
}

fn fail(message: &str) -> ! {
    eprintln!("editor: {message}");
    std::process::exit(2);
}

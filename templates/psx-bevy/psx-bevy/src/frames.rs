//! A scenario recording as the port draws it, one PNG per tick, for `psx
//! web` to show beside the original's screens:
//!
//! ```text
//! cargo run -p game -- --replay scenarios/<id>/<name>.jsonl --frames DIR [--software]
//! ```
//!
//! The launcher builds the whole game, headless, with [`Replaying`] in the
//! world, runs the scenario's launcher (as the scenario tests do) and then
//! each of the recording's ticks with its pad, mode and sticks. The game's
//! own opening (a title screen, its first level) must not run while
//! [`Replaying`] is present (`run_if(not(resource_exists::<Replaying>))`), or
//! it shares the screen with the scenario's world.
//!
//! `DIR/<tick>.png` is the screen ([`PsxScreen`]) after that tick, at the
//! game's own resolution: `0.png` is the initial moment, after the launcher.
//! `DIR/frames.json` says how many were drawn and what stopped them early.
//! Progress goes to stdout (`tick N/M`).
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::PluginsState;
use bevy::prelude::*;
use bevy::render::RenderApp;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::time::TimeUpdateStrategy;
use psx_scenario::scenario::{Recording, Scenario};
use serde_json::{Value, json};

use crate::psx::PsxScreen;
use crate::shell::{GameClock, InjectedInput, TickRate};

/// In the world while the app draws a recording: the game skips its own
/// opening, as the scenario's launcher builds the world.
#[derive(Resource, Clone, Debug)]
pub struct Replaying {
    /// `scenarios/<id>/<name>.jsonl`.
    pub recording: PathBuf,
}

/// Longest wait for the renderer to settle before the first tick, and for
/// the last screens to come back.
const SETTLE_LIMIT: Duration = Duration::from_secs(30);

/// Draws `recording` into `out` and writes `out/frames.json`.
pub fn run(mut app: App, recording: &Path, out: &Path) -> AppExit {
    let mut summary = json!({"recording": recording, "drawn": 0});
    let result = std::fs::create_dir_all(out)
        .map_err(|e| format!("{}: {e}", out.display()))
        .and_then(|_| draw(&mut app, recording, out, &mut summary));
    if let Err(e) = &result {
        summary["error"] = json!(e);
        eprintln!("replay: {e}");
    }
    let _ = std::fs::write(out.join("frames.json"), serde_json::to_vec_pretty(&summary).unwrap_or_default());
    if result.is_ok() { AppExit::Success } else { AppExit::error() }
}

/// The screens that came back: tick, or why it failed.
type Captured = Arc<Mutex<Vec<Result<u64, String>>>>;

fn draw(app: &mut App, recording: &Path, out: &Path, summary: &mut Value) -> Result<(), String> {
    let (scenario, rec) = load(recording)?;
    // Ready the app as App::run would.
    while app.plugins_state() == PluginsState::Adding {
        bevy::tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();
    let registry = crate::scenario::registry(app);
    let launch = registry.launcher(&scenario.kind).ok_or(format!("no launcher registered for scenario kind {}", scenario.kind))?;
    // Nothing ticks until the renderer is ready.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    launch(app, &scenario.parameters).map_err(|e| format!("the launcher failed: {e}"))?;
    settle(app);
    let screen = app.world().resource::<PsxScreen>();
    summary["width"] = json!(screen.width);
    summary["height"] = json!(screen.height);
    let ticks = rec.expand();
    summary["ticks"] = json!(ticks.len());
    let captured = Captured::default();
    // The initial moment: an update that draws without ticking.
    shoot(app, 0, out, &captured);
    let mut shots = 1;
    app.update();
    let period = app.world().resource::<TickRate>().period();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(period));
    let mut failure = None;
    for t in &ticks {
        let input = match crate::scenario::tick_input(t) {
            Ok(i) => i,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        {
            let mut injected = app.world_mut().resource_mut::<InjectedInput>();
            injected.set(&input.buttons);
            injected.sticks = input.sticks;
            injected.analog = input.analog;
        }
        // Spawned before the update, so its render draws this tick.
        shoot(app, t.tick, out, &captured);
        shots += 1;
        let start = app.world().resource::<GameClock>().frame;
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.update()));
        if let Err(p) = run {
            failure = Some(format!("tick {} panicked: {}", t.tick, panic_message(&p)));
            break;
        }
        let ran = app.world().resource::<GameClock>().frame - start;
        if ran != 1 {
            failure = Some(format!("tick {}: an update ran {ran} ticks, not 1", t.tick));
            break;
        }
        if t.tick % 60 == 0 {
            println!("tick {}/{}", t.tick, ticks.len());
        }
    }
    // The last screens come back a few updates later.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    let deadline = Instant::now() + if failure.is_some() { Duration::from_secs(2) } else { SETTLE_LIMIT };
    while count(&captured) < shots && Instant::now() < deadline {
        app.update();
        std::thread::sleep(Duration::from_millis(1));
    }
    let got = captured.lock().unwrap();
    let drawn = got.iter().filter(|r| r.is_ok()).count();
    summary["drawn"] = json!(drawn);
    if let Some(Err(e)) = got.iter().find(|r| r.is_err()) {
        failure.get_or_insert(e.clone());
    }
    println!("tick {}/{} drawn {drawn}", ticks.len(), ticks.len());
    failure.map_or(Ok(()), Err)
}

fn count(c: &Captured) -> usize {
    c.lock().unwrap().len()
}

/// The scenario (`scenarios/<id>.toml`) and the recording.
fn load(recording: &Path) -> Result<(Scenario, Recording), String> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let id = recording
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .ok_or(format!("{}: not scenarios/<id>/<name>.jsonl", recording.display()))?;
    let toml = recording.parent().and_then(|d| d.parent()).unwrap_or(Path::new(".")).join(format!("{id}.toml"));
    let scenario = Scenario::parse(&read(&toml)?)?;
    let rec = Recording::parse(&read(recording)?)?;
    let problems = rec.check(id);
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok((scenario, rec))
}

/// Updates without ticking until every pipeline has compiled for a few
/// updates running, so the first screens are not blank, and assets queued
/// at the start have had a moment to load.
fn settle(app: &mut App) {
    let deadline = Instant::now() + SETTLE_LIMIT;
    let (mut settled, mut updates) = (0, 0);
    while (settled < 3 || updates < 20) && Instant::now() < deadline {
        app.update();
        updates += 1;
        let cache = app.sub_app(RenderApp).world().get_resource::<PipelineCache>();
        let compiled = cache.is_some_and(|c| {
            c.pipelines().next().is_some() && c.pipelines().all(|p| matches!(p.state, CachedPipelineState::Ok(_)))
        });
        settled = if compiled { settled + 1 } else { 0 };
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Asks for the screen as the next update draws it, saved as `<tick>.png`.
fn shoot(app: &mut App, tick: u64, out: &Path, captured: &Captured) {
    let image = app.world().resource::<PsxScreen>().image.clone();
    let path = out.join(format!("{tick}.png"));
    let captured = captured.clone();
    app.world_mut().spawn(Screenshot::image(image)).observe(move |c: On<ScreenshotCaptured>| {
        let saved = c
            .image
            .clone()
            .try_into_dynamic()
            .map_err(|e| e.to_string())
            .and_then(|i| i.to_rgb8().save(&path).map_err(|e| e.to_string()))
            .map(|_| tick)
            .map_err(|e| format!("tick {tick}: {e}"));
        captured.lock().unwrap().push(saved);
    });
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|m| m.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into())
}

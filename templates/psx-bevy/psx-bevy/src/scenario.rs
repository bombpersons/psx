//! Scenarios: the port against the original in situations both can reach,
//! tick by tick. psx's `docs/scenarios.md` defines the files; `psx port
//! check` runs the game's `tests/scenarios.rs` and records the verdicts.
//!
//! The port registers a launcher for each scenario kind
//! (`kind` in `scenarios/<id>.toml`), which builds the world from the
//! scenario's parameters with the port's own loading, and a function that
//! observes the glossary's quantities it keeps: it fills `Observed`, which
//! the game's build generates from `glossary/` (one `Option` per quantity,
//! in the original's raw units, `None` where the port has no such thing):
//!
//! ```ignore
//! use bevy::prelude::*;
//! use psx_bevy::scenario::{Parameters, ScenarioApp};
//! use crate::observed::Observed;
//!
//! #[derive(Component)]
//! struct Player {
//!     x: i32,
//! }
//!
//! fn level(app: &mut App, p: &Parameters) -> Result<(), String> {
//!     let level = p.get("level").and_then(|l| l.as_str()).ok_or("level: a name")?;
//!     // In a game: load the level by name, then place the player at its start.
//!     let _ = level;
//!     app.world_mut().spawn(Player { x: 4096 });
//!     Ok(())
//! }
//!
//! struct PlayerPlugin;
//!
//! impl Plugin for PlayerPlugin {
//!     fn build(&self, app: &mut App) {
//!         app.register_scenario("level", level).observe(|w: &mut World| Observed {
//!             player_x: w.query::<&Player>().iter(w).next().map(|c| c.x),
//!             ..Observed::none()
//!         });
//!     }
//! }
//!
//! // #[test] fn scenarios() { Scenarios::new(root, || { /* a headless app with PlayerPlugin */ }).run(); }
//! ```
//!
//! A misspelt or removed quantity is a compile error. Several plugins may
//! each observe their own quantities; a quantity two of them observe is a
//! failure.
//!
//! The runner is `psx_scenario::runner`'s, which runs any port; this module
//! is how a Bevy app is one. For each recording (`scenarios/<id>/<name>.jsonl`,
//! made by `psx scenario record`) the runner makes a fresh app, launches the scenario, compares
//! the quantities with the recording's initial values (the end of the
//! original's first tick), then runs one tick per line with the line's pad,
//! the pad's mode ([`crate::controller::ControllerState::analog`]) and sticks
//! (their raw bytes, in [`crate::controller::ControllerState::sticks`]) and
//! compares again. Each quantity is compared within the scenario's
//! tolerance (`compare`, raw units); the first tick each one differs is
//! reported. The tick's input is the recording's alone: live devices are
//! ignored ([`crate::testing::apply`]).
//!
//! Each recording then runs again in another fresh app, and where the two
//! runs' quantities first part (the tick and quantity) is a failure too: the
//! port must be deterministic, its random numbers seeded and its rules
//! independent of query and hash-map iteration order. A port's difference
//! from the recording after that tick may be chance.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use psx_scenario::Value;
pub use psx_scenario::runner::{Observation, Parameters, RecordingReport, Report, ScenarioReport};
use psx_scenario::runner::{Pad, Port};

use crate::testing::{TickApp, TickInput};

type Launcher = Arc<dyn Fn(&mut App, &Parameters) -> Result<(), String> + Send + Sync>;
type Observer = Arc<dyn Fn(&mut World) -> Vec<(&'static str, Option<Value>)> + Send + Sync>;

/// The launchers and observers the app's plugins registered.
#[derive(Resource, Default, Clone)]
pub struct ScenarioRegistry {
    launchers: BTreeMap<String, Launcher>,
    observers: Vec<Observer>,
}

impl ScenarioRegistry {
    pub fn kinds(&self) -> Vec<String> {
        self.launchers.keys().cloned().collect()
    }

    pub(crate) fn launcher(&self, kind: &str) -> Option<Launcher> {
        self.launchers.get(kind).cloned()
    }

    /// Every observer's quantities, in the order they were registered.
    fn observe(&self, w: &mut World) -> Vec<(&'static str, Option<Value>)> {
        self.observers.iter().flat_map(|o| o(w)).collect()
    }
}

/// Registering a feature's scenario launchers and what it observes.
pub trait ScenarioApp {
    /// The launcher for scenarios of `kind`: it builds the world from the
    /// scenario's parameters, at the moment the scenario starts.
    fn register_scenario(
        &mut self,
        kind: &str,
        launch: impl Fn(&mut App, &Parameters) -> Result<(), String> + Send + Sync + 'static,
    ) -> &mut Self;

    /// A function giving the glossary's quantities the feature keeps, now,
    /// as `Observed { …, ..Observed::none() }`.
    fn observe<O: Observation>(&mut self, observe: impl Fn(&mut World) -> O + Send + Sync + 'static) -> &mut Self;
}

impl ScenarioApp for App {
    fn register_scenario(
        &mut self,
        kind: &str,
        launch: impl Fn(&mut App, &Parameters) -> Result<(), String> + Send + Sync + 'static,
    ) -> &mut Self {
        let mut r = self.world_mut().get_resource_or_insert_with(ScenarioRegistry::default);
        assert!(!r.launchers.contains_key(kind), "scenario kind {kind} registered twice");
        r.launchers.insert(kind.into(), Arc::new(launch));
        self
    }

    fn observe<O: Observation>(&mut self, observe: impl Fn(&mut World) -> O + Send + Sync + 'static) -> &mut Self {
        let mut r = self.world_mut().get_resource_or_insert_with(ScenarioRegistry::default);
        r.observers.push(Arc::new(move |w| observe(w).values()));
        self
    }
}

/// A Bevy app as the runner's port: the launchers and observers its
/// plugins registered, and one tick per [`TickApp::run_input`].
pub struct AppPort {
    pub app: App,
    registry: ScenarioRegistry,
}

impl AppPort {
    pub fn new(mut app: App) -> AppPort {
        let registry = registry(&mut app);
        AppPort { app, registry }
    }
}

impl Port for AppPort {
    type Observed = Vec<(&'static str, Option<Value>)>;

    fn kinds(&self) -> Vec<String> {
        self.registry.kinds()
    }

    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
        let launch = self.registry.launcher(kind).ok_or(format!("no launcher registered for scenario kind {kind}"))?;
        launch(&mut self.app, p)
    }

    fn tick(&mut self, pad: &Pad) {
        self.app.run_input(&pad_input(pad));
    }

    fn observe(&mut self) -> Result<Self::Observed, String> {
        Ok(self.registry.observe(self.app.world_mut()))
    }
}

/// The scenarios of a workspace, run against apps `app` makes.
pub struct Scenarios(psx_scenario::Scenarios<AppPort>);

impl Scenarios {
    /// `root` is the workspace (it has `scenarios/`); `app` makes a fresh
    /// headless app with the plugins that register the launchers and
    /// observers ([`crate::testing::headless_app`] and the features' logic
    /// plugins).
    pub fn new(root: impl Into<PathBuf>, app: impl Fn() -> App + 'static) -> Scenarios {
        Scenarios(psx_scenario::Scenarios::new(root, move || AppPort::new(app())))
    }

    pub fn report(&self) -> Report {
        self.0.report()
    }

    /// Runs every scenario, prints the results, writes them to
    /// `$PSX_SCENARIO_REPORT` when it is set, and panics when one fails.
    pub fn run(self) {
        self.0.run()
    }
}

/// The runner's pad as the app's tick input: the buttons psx-bevy's
/// controller has, the mode and the sticks.
pub(crate) fn pad_input(pad: &Pad) -> TickInput {
    let buttons = pad.buttons.iter().filter_map(|b| crate::testing::trajectory::button(b.name())).collect();
    TickInput { buttons, analog: Some(pad.analog), sticks: Some(pad.sticks), ..default() }
}

/// A recording's tick as input: its pad, mode and sticks.
pub(crate) fn tick_input(t: &psx_scenario::scenario::Full) -> Result<TickInput, String> {
    Pad::of(t).map(|p| pad_input(&p))
}

pub(crate) fn registry(app: &mut App) -> ScenarioRegistry {
    app.world_mut().get_resource_or_insert_with(ScenarioRegistry::default).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Button, ControllerState};
    use crate::prelude::*;
    use crate::testing::headless_app;

    #[derive(Component)]
    struct Walker(i32);

    /// What the game's build generates from a glossary with one quantity, x.
    struct Observed {
        x: Option<i32>,
    }

    impl Observation for Observed {
        fn values(&self) -> Vec<(&'static str, Option<Value>)> {
            vec![("x", self.x.map(|v| Value::Int(v as i128)))]
        }
    }

    fn walk(pad: Res<ControllerState>, mut q: Query<&mut Walker>) {
        for mut w in &mut q {
            // Slower on an analog pad, as a game can act on the mode.
            if pad.held(Button::Right) {
                w.0 += if pad.analog { 50 } else { 100 };
            }
            // The left stick's raw byte, from centre.
            w.0 += i32::from(pad.sticks[0]) - 128;
        }
    }

    fn app() -> App {
        let mut app = headless_app(TickRate::every_field(Region::Ntsc));
        app.add_systems(FixedUpdate, walk.in_set(GameSystems::Game));
        app.register_scenario("level", |app, p| {
            let x = p.get("x").and_then(|x| x.as_i64()).ok_or("x: a number")?;
            app.world_mut().spawn(Walker(x as i32));
            Ok(())
        })
        .observe(|w: &mut World| Observed { x: w.query::<&Walker>().iter(w).next().map(|c| c.0) });
        app
    }

    const RECORDING: &str = r#"{"format":"scenario-recording","format_version":1,"scenario":"walk","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0"},"quantities":{"x":{"type":"int","bits":32,"signed":true}},"initial":{"x":4096}}
{"tick":1,"field":1,"pad":["RIGHT"],"values":{"x":4196}}
{"tick":2,"field":2,"values":{"x":4296}}
{"tick":3,"field":3,"pad":[]}
"#;

    fn workspace(scenario: &str, recording: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("psx-bevy-scenario-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scenarios/walk")).unwrap();
        std::fs::write(root.join("scenarios/walk.toml"), scenario).unwrap();
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording).unwrap();
        root
    }

    const SCENARIO: &str =
        "moment = \"m\"\nchain = \"c\"\nkind = \"level\"\n[compare]\nx = 0\n[parameters]\nx = 4096\n";

    #[test]
    fn a_port_that_matches_passes() {
        let root = workspace(SCENARIO, RECORDING);
        let r = Scenarios::new(&root, app).report();
        assert_eq!((r.kinds.clone(), r.quantities.clone()), (vec!["level".to_string()], vec!["x".to_string()]));
        let s = &r.scenarios[0];
        assert!(s.passed(), "{s:?}");
        assert_eq!((s.recordings[0].ticks, s.recordings[0].captured), (3, true));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_mode_and_sticks_replay_tick_for_tick() {
        let header = RECORDING.lines().next().unwrap();
        let rows = [
            r#"{"tick":1,"field":1,"analog":true,"sticks":{"lx":160},"values":{"x":4128}}"#,
            r#"{"tick":2,"field":2,"pad":["RIGHT"],"sticks":{"lx":0},"values":{"x":4050}}"#,
            r#"{"tick":3,"field":3,"analog":false,"sticks":{"lx":128},"values":{"x":4150}}"#, // RIGHT still held
        ];
        let root = workspace(SCENARIO, &format!("{header}\n{}\n", rows.join("\n")));
        let s = &Scenarios::new(&root, app).report().scenarios[0];
        assert!(s.passed(), "{s:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_difference_names_the_tick_and_quantity() {
        let root = workspace(&SCENARIO.replace("x = 4096", "x = 4095 "), &RECORDING.replace("4296", "4300"));
        let r = Scenarios::new(&root, app).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["initial x: expected 4096, got 4095"]);
        // Within the tolerance, the first tick past it is named.
        let tolerant = SCENARIO.replace("x = 0", "x = 3").replace("x = 4096", "x = 4095 ");
        let root2 = workspace(&tolerant, &RECORDING.replace("4296", "4300"));
        let r = Scenarios::new(&root2, app).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["tick 2 x: expected 4300, got 4295"]);
        // What the port had, initially and after each tick it ran.
        let had: Vec<_> = r.scenarios[0].recordings[0].values.iter().map(|v| v["x"].clone()).collect();
        assert_eq!(had, [Value::Int(4095), Value::Int(4195), Value::Int(4295)]);
        let missing = SCENARIO.replace("kind = \"level\"", "kind = \"race\"");
        let root3 = workspace(&missing, RECORDING);
        let r = Scenarios::new(&root3, app).report();
        assert_eq!(r.scenarios[0].problems, ["no launcher registered for scenario kind race"]);
        // Two plugins observing the same quantity.
        let root4 = workspace(SCENARIO, RECORDING);
        let twice = || {
            let mut a = app();
            a.observe(|_: &mut World| Observed { x: Some(1) });
            a
        };
        let r = Scenarios::new(&root4, twice).report();
        assert_eq!(
            r.scenarios[0].recordings[0].failures,
            ["initial: x is observed by two functions: observe each in one place"]
        );
        for d in [root, root2, root3, root4] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// A key held on the keyboard all along: the replay has only the
    /// recording's pad.
    #[test]
    fn a_held_key_does_not_reach_the_replay() {
        let header = RECORDING.lines().next().unwrap();
        let root = workspace(SCENARIO, &format!("{header}\n{}\n", r#"{"tick":1,"field":1,"values":{"x":4096}}"#));
        let held = || {
            let mut a = app();
            a.init_resource::<ButtonInput<KeyCode>>();
            a.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::ArrowRight);
            a
        };
        let s = &Scenarios::new(&root, held).report().scenarios[0];
        assert!(s.passed(), "{s:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Component)]
    struct Tracked;

    /// Nudges whichever walker the query gives first: the order the
    /// launcher spawned them in, which `LAUNCHES` turns each app.
    fn nudge_first(mut q: Query<&mut Walker>) {
        if let Some(mut w) = q.iter_mut().next() {
            w.0 += 1;
        }
    }

    #[test]
    fn a_rule_picking_by_query_order_is_not_deterministic() {
        static LAUNCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = workspace(&SCENARIO.replace("\"level\"", "\"crowd\""), RECORDING);
        let crowd = || {
            let mut a = headless_app(TickRate::every_field(Region::Ntsc));
            a.add_systems(FixedUpdate, (walk, nudge_first).chain().in_set(GameSystems::Game));
            a.register_scenario("crowd", |app, _| {
                let n = LAUNCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let tracked = (Walker(4096), Tracked);
                if n.is_multiple_of(2) {
                    app.world_mut().spawn(tracked);
                    app.world_mut().spawn(Walker(4096));
                } else {
                    app.world_mut().spawn(Walker(4096));
                    app.world_mut().spawn(tracked);
                }
                Ok(())
            })
            .observe(|w: &mut World| Observed {
                x: w.query_filtered::<&Walker, With<Tracked>>().iter(w).next().map(|c| c.0),
            });
            a
        };
        let r = Scenarios::new(&root, crowd).report();
        let failures = &r.scenarios[0].recordings[0].failures;
        // The walker the observer follows is nudged in one run, not the other.
        let parted = ["first run 4197, second run 4196", "first run 4196, second run 4197"]
            .map(|runs| format!("tick 1 x: the port is not deterministic: {runs}"));
        assert!(parted.contains(&failures[0]), "{failures:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[derive(Component)]
    struct Fragile;

    fn break_at(q: Query<&Walker, With<Fragile>>) {
        if q.iter().any(|w| w.0 > 4196) {
            panic!("walked off the floor");
        }
    }

    #[test]
    fn a_panic_fails_its_recording_and_the_rest_still_run() {
        let root = workspace(SCENARIO, RECORDING);
        std::fs::create_dir_all(root.join("scenarios/fragile")).unwrap();
        std::fs::write(root.join("scenarios/fragile.toml"), SCENARIO.replace("\"level\"", "\"fragile\"")).unwrap();
        std::fs::write(root.join("scenarios/fragile/a.jsonl"), RECORDING.replace("\"walk\"", "\"fragile\"")).unwrap();
        let fragile = || {
            let mut a = app();
            a.add_systems(FixedUpdate, break_at.in_set(GameSystems::Game).after(walk));
            a.register_scenario("fragile", |app, _| {
                app.world_mut().spawn((Walker(4096), Fragile));
                Ok(())
            });
            a
        };
        let r = Scenarios::new(&root, fragile).report();
        let by_id = |id: &str| r.scenarios.iter().find(|s| s.id == id).unwrap();
        assert_eq!(by_id("fragile").recordings[0].failures, ["tick 2 panicked: walked off the floor"]);
        assert!(by_id("walk").passed(), "{:?}", by_id("walk"));
        let _ = std::fs::remove_dir_all(root);
    }
}

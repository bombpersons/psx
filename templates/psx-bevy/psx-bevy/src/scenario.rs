//! Scenarios: the port against the original in situations both can reach,
//! tick by tick. psx's `docs/scenarios.md` defines the files; `psx port
//! check` runs the game's `tests/scenarios.rs` and records the verdicts.
//!
//! A feature's plugin registers a launcher for each scenario kind it owns
//! (`kind` in `scenarios/<id>.toml`), which builds the world from the
//! scenario's parameters with the port's own loading, and a getter for each
//! glossary quantity it keeps, in raw units ([`Field`]):
//!
//! ```
//! use bevy::prelude::*;
//! use psx_bevy::prelude::*;
//! use psx_bevy::scenario::{Parameters, ScenarioApp};
//!
//! #[derive(Component)]
//! struct Croc {
//!     x: Num<Exact, i32, 12>,
//! }
//!
//! fn level(app: &mut App, p: &Parameters) -> Result<(), String> {
//!     let level = p.get("level").and_then(|l| l.as_str()).ok_or("level: a name")?;
//!     // In a game: load the level by name, then place Croc at its start.
//!     let _ = level;
//!     app.world_mut().spawn(Croc { x: Num::from_raw(4096) });
//!     Ok(())
//! }
//!
//! struct CrocPlugin;
//!
//! impl Plugin for CrocPlugin {
//!     fn build(&self, app: &mut App) {
//!         app.register_scenario("level", level).register_quantity("croc_x", |w| {
//!             let mut q = w.query::<&Croc>();
//!             q.iter(w).next().map(|c| c.x)
//!         });
//!     }
//! }
//!
//! // #[test] fn scenarios() { Scenarios::new(root, || { /* a headless app with CrocPlugin */ }).run(); }
//! ```
//!
//! For each recording (`scenarios/<id>/<name>.jsonl`, made by `psx scenario
//! record`) the runner makes a fresh app, launches the scenario, compares
//! the quantities with the recording's initial values (the end of the
//! original's first tick), then runs one tick per line with the line's pad
//! and compares again. Each quantity is compared within the scenario's
//! tolerance (`compare`, raw units); the first tick each one differs is
//! reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;
use psx_mechanics::scenario::{Recording, Scenario};
use psx_mechanics::trace::SourceKind;
use serde::Serialize;

use crate::replay::{Diff, Field, Raw, describe, diff, show};
use crate::testing::TickApp;

/// A scenario's `[parameters]`: what its launcher builds the world from.
pub type Parameters = serde_json::Map<String, serde_json::Value>;

type Launcher = Arc<dyn Fn(&mut App, &Parameters) -> Result<(), String> + Send + Sync>;
type Getter = Arc<dyn Fn(&mut World) -> Option<Raw> + Send + Sync>;

/// The launchers and quantity getters the app's plugins registered.
#[derive(Resource, Default, Clone)]
pub struct ScenarioRegistry {
    launchers: BTreeMap<String, Launcher>,
    quantities: BTreeMap<String, Getter>,
}

impl ScenarioRegistry {
    pub fn kinds(&self) -> Vec<String> {
        self.launchers.keys().cloned().collect()
    }

    pub fn quantities(&self) -> Vec<String> {
        self.quantities.keys().cloned().collect()
    }
}

/// Registering a feature's scenario launchers and quantities.
pub trait ScenarioApp {
    /// The launcher for scenarios of `kind`: it builds the world from the
    /// scenario's parameters, at the moment the scenario starts.
    fn register_scenario(
        &mut self,
        kind: &str,
        launch: impl Fn(&mut App, &Parameters) -> Result<(), String> + Send + Sync + 'static,
    ) -> &mut Self;

    /// The getter for a glossary quantity: its value now, in the original's
    /// raw units, or `None` when the port has no such thing now.
    fn register_quantity<T: Field>(
        &mut self,
        name: &str,
        get: impl Fn(&mut World) -> Option<T> + Send + Sync + 'static,
    ) -> &mut Self;
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

    fn register_quantity<T: Field>(
        &mut self,
        name: &str,
        get: impl Fn(&mut World) -> Option<T> + Send + Sync + 'static,
    ) -> &mut Self {
        let mut r = self.world_mut().get_resource_or_insert_with(ScenarioRegistry::default);
        assert!(!r.quantities.contains_key(name), "quantity {name} registered twice");
        r.quantities.insert(name.into(), Arc::new(move |w| get(w).map(|v| v.to_raw())));
        self
    }
}

/// The scenarios' results, written to `$PSX_SCENARIO_REPORT` for psx.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// What the app's plugins registered: the scenario kinds and quantities.
    pub kinds: Vec<String>,
    pub quantities: Vec<String>,
    pub scenarios: Vec<ScenarioReport>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScenarioReport {
    pub id: String,
    pub feature: String,
    pub kind: String,
    /// What stops the scenario from running at all.
    pub problems: Vec<String>,
    pub recordings: Vec<RecordingReport>,
}

impl ScenarioReport {
    pub fn passed(&self) -> bool {
        self.problems.is_empty() && self.recordings.iter().all(|r| r.failures.is_empty())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RecordingReport {
    pub name: String,
    /// Recorded from the original (the rest are examples).
    pub captured: bool,
    pub ticks: usize,
    /// The first tick each quantity differs at, or why the run stopped.
    pub failures: Vec<String>,
}

/// The scenarios of a workspace, run against apps `app` makes.
pub struct Scenarios {
    root: PathBuf,
    app: Box<dyn Fn() -> App>,
}

impl Scenarios {
    /// `root` is the workspace (it has `scenarios/`); `app` makes a fresh
    /// headless app with the plugins that register the launchers and
    /// quantities ([`crate::testing::headless_app`] and the features' logic
    /// plugins).
    pub fn new(root: impl Into<PathBuf>, app: impl Fn() -> App + 'static) -> Scenarios {
        Scenarios { root: root.into(), app: Box::new(app) }
    }

    pub fn report(&self) -> Report {
        let registry = registry(&mut (self.app)());
        let mut report = Report { kinds: registry.kinds(), quantities: registry.quantities(), scenarios: Vec::new() };
        let dir = self.root.join("scenarios");
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
            .collect();
        ids.sort();
        for id in ids {
            report.scenarios.push(self.scenario(&dir, &id, &registry));
        }
        report
    }

    fn scenario(&self, dir: &Path, id: &str, registry: &ScenarioRegistry) -> ScenarioReport {
        let mut r = ScenarioReport { id: id.into(), ..default() };
        let s = match read(&dir.join(format!("{id}.toml"))).and_then(|t| Scenario::parse(&t)) {
            Ok(s) => s,
            Err(e) => {
                r.problems.push(e);
                return r;
            }
        };
        (r.feature, r.kind) = (s.feature.clone(), s.kind.clone());
        if !registry.launchers.contains_key(&s.kind) {
            r.problems.push(format!("no launcher registered for scenario kind {}", s.kind));
        }
        for q in s.compare.keys().filter(|q| !registry.quantities.contains_key(*q)) {
            r.problems.push(format!("no getter registered for quantity {q}"));
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join(id))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            let rec = read(&f).and_then(|t| Recording::parse(&t)).and_then(|rec| {
                let p = rec.check(id);
                if p.is_empty() { Ok(rec) } else { Err(p.join("; ")) }
            });
            r.recordings.push(match rec {
                Ok(rec) if r.problems.is_empty() => self.recording(&name, &s, &rec),
                Ok(rec) => RecordingReport {
                    name,
                    captured: rec.header.source.kind == SourceKind::Captured,
                    ticks: rec.rows.len(),
                    failures: vec!["not run: the scenario has problems".into()],
                },
                Err(e) => RecordingReport { name, failures: vec![e], ..default() },
            });
        }
        r
    }

    fn recording(&self, name: &str, s: &Scenario, rec: &Recording) -> RecordingReport {
        let mut out = RecordingReport {
            name: name.into(),
            captured: rec.header.source.kind == SourceKind::Captured,
            ticks: rec.rows.len(),
            failures: Vec::new(),
        };
        let mut app = (self.app)();
        let registry = registry(&mut app);
        if let Err(e) = (registry.launchers[&s.kind])(&mut app, &s.parameters) {
            out.failures.push(format!("the launcher failed: {e}"));
            return out;
        }
        let compared: Vec<(&String, &Getter, f64)> = rec
            .header
            .quantities
            .keys()
            .filter_map(|q| {
                let tol = match s.compare.get(q)? {
                    psx_mechanics::Value::Int(t) => *t as f64,
                    _ => 0.0,
                };
                Some((q, &registry.quantities[q], tol))
            })
            .collect();
        let mut differed: BTreeMap<String, String> = BTreeMap::new();
        let check =
            |app: &mut App, at: &str, values: &psx_mechanics::Values, differed: &mut BTreeMap<String, String>| {
                for (q, get, tol) in &compared {
                    if differed.contains_key(q.as_str()) {
                        continue;
                    }
                    let Some(want) = values.get(*q) else { continue };
                    let line = match get(app.world_mut()) {
                        None => Some(format!("{at} {q}: expected {}, the port has none", describe(want))),
                        Some(got) => match diff(&got, want) {
                            Diff::Equal => None,
                            Diff::Off(d) if d <= *tol => None,
                            _ => Some(format!("{at} {q}: expected {}, got {}", describe(want), show(&got))),
                        },
                    };
                    if let Some(l) = line {
                        differed.insert((*q).clone(), l);
                    }
                }
            };
        check(&mut app, "initial", &rec.header.initial, &mut differed);
        for t in rec.expand() {
            if differed.len() == compared.len() {
                break;
            }
            let pad: Result<Vec<_>, String> = t
                .pad
                .iter()
                .map(|b| crate::testing::trajectory::button(b).ok_or(format!("tick {}: no button {b:?}", t.tick)))
                .collect();
            match pad {
                Ok(pad) => app.run_tick(&pad),
                Err(e) => {
                    out.failures.push(e);
                    return out;
                }
            }
            check(&mut app, &format!("tick {}", t.tick), &t.values, &mut differed);
        }
        // In the order they first differed.
        let mut lines: Vec<String> = differed.into_values().collect();
        lines.sort_by_key(|l| tick_of(l));
        out.failures = lines;
        out
    }

    /// Runs every scenario, prints the results, writes them to
    /// `$PSX_SCENARIO_REPORT` when it is set, and panics when one fails.
    pub fn run(self) {
        let report = self.report();
        if let Some(p) = std::env::var_os("PSX_SCENARIO_REPORT") {
            std::fs::write(&p, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
        let mut failed = Vec::new();
        for s in &report.scenarios {
            let ticks: usize = s.recordings.iter().map(|r| r.ticks).sum();
            println!(
                "{} ({}, {}): {} recordings, {ticks} ticks, {}",
                s.id,
                s.feature,
                s.kind,
                s.recordings.len(),
                if s.passed() { "pass" } else { "FAIL" }
            );
            failed.extend(s.problems.iter().map(|p| format!("{}: {p}", s.id)));
            for r in &s.recordings {
                failed.extend(r.failures.iter().map(|f| format!("{} {}: {f}", s.id, r.name)));
            }
        }
        assert!(failed.is_empty(), "scenarios failed:\n{}", failed.join("\n"));
    }
}

fn registry(app: &mut App) -> ScenarioRegistry {
    app.world_mut().get_resource_or_insert_with(ScenarioRegistry::default).clone()
}

/// A failure line's tick: 0 for `initial`.
fn tick_of(line: &str) -> u64 {
    line.strip_prefix("tick ").and_then(|r| r.split(' ').next()).and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Button, ControllerState};
    use crate::prelude::*;
    use crate::testing::headless_app;

    #[derive(Component)]
    struct Walker(i32);

    fn walk(pad: Res<ControllerState>, mut q: Query<&mut Walker>) {
        for mut w in &mut q {
            if pad.held(Button::Right) {
                w.0 += 100;
            }
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
        .register_quantity("x", |w| w.query::<&Walker>().iter(w).next().map(|c| c.0));
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

    const SCENARIO: &str = "feature = \"F-0001\"\nmoment = \"m\"\nchain = \"c\"\nkind = \"level\"\n[compare]\nx = 0\n[parameters]\nx = 4096\n";

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
    fn a_difference_names_the_tick_and_quantity() {
        let root = workspace(&SCENARIO.replace("x = 4096", "x = 4095 "), &RECORDING.replace("4296", "4300"));
        let r = Scenarios::new(&root, app).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["initial x: expected 4096, got 4095"]);
        // Within the tolerance, the first tick past it is named.
        let tolerant = SCENARIO.replace("x = 0", "x = 3").replace("x = 4096", "x = 4095 ");
        let root2 = workspace(&tolerant, &RECORDING.replace("4296", "4300"));
        let r = Scenarios::new(&root2, app).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["tick 2 x: expected 4300, got 4295"]);
        let missing = SCENARIO.replace("kind = \"level\"", "kind = \"race\"");
        let root3 = workspace(&missing, RECORDING);
        let r = Scenarios::new(&root3, app).report();
        assert_eq!(r.scenarios[0].problems, ["no launcher registered for scenario kind race"]);
        for d in [root, root2, root3] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

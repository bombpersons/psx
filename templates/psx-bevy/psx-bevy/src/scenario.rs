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
//! struct Croc {
//!     x: i32,
//! }
//!
//! fn level(app: &mut App, p: &Parameters) -> Result<(), String> {
//!     let level = p.get("level").and_then(|l| l.as_str()).ok_or("level: a name")?;
//!     // In a game: load the level by name, then place Croc at its start.
//!     let _ = level;
//!     app.world_mut().spawn(Croc { x: 4096 });
//!     Ok(())
//! }
//!
//! struct CrocPlugin;
//!
//! impl Plugin for CrocPlugin {
//!     fn build(&self, app: &mut App) {
//!         app.register_scenario("level", level).observe(|w: &mut World| Observed {
//!             croc_x: w.query::<&Croc>().iter(w).next().map(|c| c.x),
//!             ..Observed::none()
//!         });
//!     }
//! }
//!
//! // #[test] fn scenarios() { Scenarios::new(root, || { /* a headless app with CrocPlugin */ }).run(); }
//! ```
//!
//! A misspelt or removed quantity is a compile error. Several plugins may
//! each observe their own quantities; a quantity two of them observe is a
//! failure.
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
use psx_scenario::Value;
use psx_scenario::scenario::SourceKind;
use psx_scenario::scenario::{Recording, Scenario};
use serde::Serialize;

use crate::testing::TickApp;

/// A scenario's `[parameters]`: what its launcher builds the world from.
pub type Parameters = serde_json::Map<String, serde_json::Value>;

/// The glossary's quantities as the port has them: `Observed`, generated
/// from `glossary/` by the game's build (`psx_scenario::rust::write_observed`).
pub trait Observation: Send + Sync + 'static {
    /// Each quantity by name, in the recording's encoding, or `None`.
    fn values(&self) -> Vec<(&'static str, Option<Value>)>;
}

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

    /// Every quantity some observer has now: by name, or why not.
    fn observe(&self, w: &mut World) -> Result<BTreeMap<&'static str, Value>, String> {
        let mut out = BTreeMap::new();
        for o in &self.observers {
            for (name, v) in o(w) {
                let Some(v) = v else { continue };
                if out.insert(name, v).is_some() {
                    return Err(format!("{name} is observed by two functions: observe each in one place"));
                }
            }
        }
        Ok(out)
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

/// The scenarios' results, written to `$PSX_SCENARIO_REPORT` for psx.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// The scenario kinds the app's plugins registered launchers for.
    pub kinds: Vec<String>,
    /// The quantities the port observed in any scenario it launched.
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
    /// observers ([`crate::testing::headless_app`] and the features' logic
    /// plugins).
    pub fn new(root: impl Into<PathBuf>, app: impl Fn() -> App + 'static) -> Scenarios {
        Scenarios { root: root.into(), app: Box::new(app) }
    }

    pub fn report(&self) -> Report {
        let registry = registry(&mut (self.app)());
        let mut report = Report { kinds: registry.kinds(), quantities: Vec::new(), scenarios: Vec::new() };
        let dir = self.root.join("scenarios");
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
            .collect();
        ids.sort();
        let mut observed = std::collections::BTreeSet::new();
        for id in ids {
            report.scenarios.push(self.scenario(&dir, &id, &registry, &mut observed));
        }
        report.quantities = observed.into_iter().collect();
        report
    }

    fn scenario(
        &self,
        dir: &Path,
        id: &str,
        registry: &ScenarioRegistry,
        observed: &mut std::collections::BTreeSet<String>,
    ) -> ScenarioReport {
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
                Ok(rec) if r.problems.is_empty() => self.recording(&name, &s, &rec, observed),
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

    fn recording(
        &self,
        name: &str,
        s: &Scenario,
        rec: &Recording,
        observed: &mut std::collections::BTreeSet<String>,
    ) -> RecordingReport {
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
        let compared: Vec<(&String, i128)> = rec
            .header
            .quantities
            .keys()
            .filter_map(|q| {
                let tol = match s.compare.get(q)? {
                    Value::Int(t) => *t,
                    _ => 0,
                };
                Some((q, tol))
            })
            .collect();
        let mut differed: BTreeMap<String, String> = BTreeMap::new();
        let mut check =
            |app: &mut App, at: &str, values: &psx_scenario::Values, differed: &mut BTreeMap<String, String>| {
                let now = match registry.observe(app.world_mut()) {
                    Ok(now) => now,
                    Err(e) => return Some(format!("{at}: {e}")),
                };
                observed.extend(now.keys().map(|q| q.to_string()));
                for (q, tol) in &compared {
                    if differed.contains_key(q.as_str()) {
                        continue;
                    }
                    let Some(want) = values.get(*q) else { continue };
                    let line = match now.get(q.as_str()) {
                        None => Some(format!("{at} {q}: expected {}, the port has none", show(want))),
                        Some(got) if !differs(got, want, *tol) => None,
                        Some(got) => Some(format!("{at} {q}: expected {}, got {}", show(want), show(got))),
                    };
                    if let Some(l) = line {
                        differed.insert((*q).clone(), l);
                    }
                }
                None
            };
        if let Some(e) = check(&mut app, "initial", &rec.header.initial, &mut differed) {
            out.failures.push(e);
            return out;
        }
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
            if let Some(e) = check(&mut app, &format!("tick {}", t.tick), &t.values, &mut differed) {
                out.failures.push(e);
                return out;
            }
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

/// Whether `got` is further from `want` than `tolerance` (raw units, for
/// numbers; anything else is compared exactly).
fn differs(got: &Value, want: &Value, tolerance: i128) -> bool {
    match (got, want) {
        (Value::Int(a), Value::Int(b)) => (a - b).abs() > tolerance,
        _ => got != want,
    }
}

fn show(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
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
}

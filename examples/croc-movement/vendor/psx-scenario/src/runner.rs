//! Running a port against its scenarios: the port against the original in
//! situations both can reach, tick by tick. psx's `docs/scenarios.md`
//! defines the files; `psx port check` runs them against the port over the
//! port protocol ([`crate::protocol`]) and records the verdicts.
//!
//! The port is a value implementing [`Port`]: it launches each scenario
//! kind (`kind` in `scenarios/<id>.toml`), building its world from the
//! scenario's parameters with its own loading; runs one logic tick with a
//! [`Pad`]; and observes the glossary's quantities it keeps, as `Observed`,
//! which the game's build generates from `glossary/` (one `Option` per
//! quantity, in the original's raw units, `None` where the port has no
//! such thing):
//!
//! ```ignore
//! use psx_scenario::{Pad, Parameters, Port, Scenarios};
//! use crate::observed::Observed;
//!
//! #[derive(Default)]
//! struct Game {
//!     player_x: Option<i32>,
//! }
//!
//! impl Port for Game {
//!     type Observed = Observed;
//!
//!     fn kinds(&self) -> Vec<String> {
//!         vec!["level".into()]
//!     }
//!
//!     fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
//!         let level = p.get("level").and_then(|l| l.as_str()).ok_or("level: a name")?;
//!         // In a game: load the level by name, then place the player at its start.
//!         let _ = (kind, level);
//!         self.player_x = Some(4096);
//!         Ok(())
//!     }
//!
//!     fn tick(&mut self, pad: &Pad) {
//!         // The game's rules, once.
//!     }
//!
//!     fn observe(&mut self) -> Result<Observed, String> {
//!         Ok(Observed { player_x: self.player_x, ..Observed::none() })
//!     }
//! }
//!
//! // #[test] fn scenarios() { Scenarios::new(root, Game::default).run(); }
//! ```
//!
//! A misspelt or removed quantity is a compile error.
//!
//! For each recording (`scenarios/<id>/<name>.jsonl`, made by `psx scenario
//! record`) the runner makes a fresh port, launches the scenario, compares
//! the quantities with the recording's initial values (the end of the
//! original's first tick), then runs one tick per line with the line's pad,
//! the pad's mode ([`Pad::analog`]) and sticks (their raw bytes, in
//! [`Pad::sticks`]) and compares again. Each quantity is compared within
//! the scenario's tolerance (`compare`, raw units); the first tick each one
//! differs is reported. The tick's input is the recording's alone: a port
//! must not read live devices in [`Port::tick`].
//!
//! Each recording then runs again in another fresh port, and where the two
//! runs' quantities first part (the tick and quantity) is a failure too: the
//! port must be deterministic, its random numbers seeded and its rules
//! independent of hash-map iteration order. A port's difference from the
//! recording after that tick may be chance.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::scenario::{CENTRED, Full, Recording, Scenario, SourceKind};
use crate::{Value, Values};

/// A scenario's `[parameters]`: what its launcher builds the world from.
pub type Parameters = serde_json::Map<String, serde_json::Value>;

/// The glossary's quantities as the port has them: `Observed`, generated
/// from `glossary/` by the game's build (`crate::rust::write_observed`).
pub trait Observation {
    /// Each quantity by name, in the recording's encoding, or `None`.
    fn values(&self) -> Vec<(&'static str, Option<Value>)>;
}

/// Quantities gathered from several places (an engine's plugins, say); a
/// name given twice is a failure.
impl Observation for Vec<(&'static str, Option<Value>)> {
    fn values(&self) -> Vec<(&'static str, Option<Value>)> {
        self.clone()
    }
}

/// A PS1 pad button, as recordings name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Button {
    Select,
    L3,
    R3,
    Start,
    Up,
    Right,
    Down,
    Left,
    L2,
    R2,
    L1,
    R1,
    Triangle,
    Circle,
    Cross,
    Square,
}

impl Button {
    pub const ALL: [Button; 16] = [
        Button::Select,
        Button::L3,
        Button::R3,
        Button::Start,
        Button::Up,
        Button::Right,
        Button::Down,
        Button::Left,
        Button::L2,
        Button::R2,
        Button::L1,
        Button::R1,
        Button::Triangle,
        Button::Circle,
        Button::Cross,
        Button::Square,
    ];

    /// The name recordings use (`CROSS`, `L1`).
    pub fn name(self) -> &'static str {
        match self {
            Button::Select => "SELECT",
            Button::L3 => "L3",
            Button::R3 => "R3",
            Button::Start => "START",
            Button::Up => "UP",
            Button::Right => "RIGHT",
            Button::Down => "DOWN",
            Button::Left => "LEFT",
            Button::L2 => "L2",
            Button::R2 => "R2",
            Button::L1 => "L1",
            Button::R1 => "R1",
            Button::Triangle => "TRIANGLE",
            Button::Circle => "CIRCLE",
            Button::Cross => "CROSS",
            Button::Square => "SQUARE",
        }
    }

    /// A button by its name, in any case.
    pub fn parse(name: &str) -> Option<Button> {
        Button::ALL.into_iter().find(|b| b.name().eq_ignore_ascii_case(name))
    }
}

/// One tick's input: the whole pad, as the recording holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pad {
    pub buttons: Vec<Button>,
    /// The pad in analog mode (the game's or the ANALOG button's).
    pub analog: bool,
    /// lx, ly, rx, ry: 0 is left or up, 0xFF right or down, 0x80 at rest.
    pub sticks: [u8; 4],
}

impl Default for Pad {
    fn default() -> Pad {
        Pad { buttons: Vec::new(), analog: false, sticks: CENTRED }
    }
}

impl Pad {
    pub fn held(&self, b: Button) -> bool {
        self.buttons.contains(&b)
    }

    /// A recording's tick as input: its pad, mode and sticks.
    pub fn of(t: &Full) -> Result<Pad, String> {
        let buttons = t
            .pad
            .iter()
            .map(|b| Button::parse(b).ok_or(format!("tick {}: no button {b:?}", t.tick)))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Pad { buttons, analog: t.analog, sticks: t.sticks })
    }
}

/// The port, as scenarios run it.
pub trait Port {
    /// `Observed`, as the game's build generates it from the glossary.
    type Observed: Observation;

    /// The scenario kinds this port can launch.
    fn kinds(&self) -> Vec<String>;

    /// Builds the world for a scenario of `kind` from its parameters, at
    /// the moment the scenario starts.
    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String>;

    /// One logic tick with exactly this input.
    fn tick(&mut self, pad: &Pad);

    /// The glossary's quantities the port keeps, now, as
    /// `Observed { …, ..Observed::none() }`.
    fn observe(&mut self) -> Result<Self::Observed, String>;

    /// The screen after the last tick (or the launch), at the game's own
    /// resolution, for `--replay` ([`crate::replay`]). None while the port
    /// draws nothing.
    fn screen(&mut self) -> Option<crate::replay::Frame> {
        None
    }
}

/// The scenarios' results: what `psx port check` records.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// The scenario kinds the port can launch.
    pub kinds: Vec<String>,
    /// The quantities the port observed in any scenario it launched.
    pub quantities: Vec<String>,
    pub scenarios: Vec<ScenarioReport>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScenarioReport {
    pub id: String,
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
    /// Which quantities differ from the recording, or why the run
    /// stopped: not where or by how much, so a failure points at the rule
    /// and not at a number to move.
    pub failures: Vec<String>,
    /// Where each differing quantity first differed and the values there,
    /// for the person's view (`psx web` plots them); kept out of the
    /// failures.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<Difference>,
    /// When a quantity differed: the compared quantities the port had,
    /// initially and after each tick it ran (`psx web` plots them beside
    /// the recording's).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Values>,
}

/// Where a quantity first differed from the recording: `initial` or
/// `tick N`, the recorded value and the port's (none where it had none).
#[derive(Clone, Debug, Serialize)]
pub struct Difference {
    pub quantity: String,
    pub at: String,
    pub expected: Value,
    pub got: Option<Value>,
}

/// The scenarios of a workspace, run against ports `port` makes.
pub struct Scenarios<P> {
    root: PathBuf,
    port: Box<dyn Fn() -> P>,
}

impl<P: Port> Scenarios<P> {
    /// `root` is the workspace (it has `scenarios/`); `port` makes a fresh
    /// port, headless, with every feature's rules.
    pub fn new(root: impl Into<PathBuf>, port: impl Fn() -> P + 'static) -> Scenarios<P> {
        Scenarios { root: root.into(), port: Box::new(port) }
    }

    pub fn report(&self) -> Report {
        let kinds = (self.port)().kinds();
        let mut report = Report { kinds: kinds.clone(), quantities: Vec::new(), scenarios: Vec::new() };
        let dir = self.root.join("scenarios");
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(String::from))
            .collect();
        ids.sort();
        let mut observed = BTreeSet::new();
        for id in ids {
            report.scenarios.push(self.scenario(&dir, &id, &kinds, &mut observed));
        }
        report.quantities = observed.into_iter().collect();
        report
    }

    fn scenario(&self, dir: &Path, id: &str, kinds: &[String], observed: &mut BTreeSet<String>) -> ScenarioReport {
        let mut r = ScenarioReport { id: id.into(), ..Default::default() };
        let s = match read(&dir.join(format!("{id}.toml"))).and_then(|t| Scenario::parse(&t)) {
            Ok(s) => s,
            Err(e) => {
                r.problems.push(e);
                return r;
            }
        };
        r.kind = s.kind.clone();
        if !kinds.contains(&s.kind) {
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
                    ..Default::default()
                },
                Err(e) => RecordingReport { name, failures: vec![e], ..Default::default() },
            });
        }
        r
    }

    /// Replays one recording. A panic in the port (its launcher, a tick or
    /// an observation) fails this recording, at the tick it happened, and
    /// the rest still run and reach the report.
    fn recording(&self, name: &str, s: &Scenario, rec: &Recording, observed: &mut BTreeSet<String>) -> RecordingReport {
        let run = |observed: &mut BTreeSet<String>| {
            let mut at = String::from("the launcher");
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.replay(name, s, rec, observed, &mut at)))
                .map_err(|p| (at, panic_message(&p)))
        };
        let (mut out, first) = match run(observed) {
            Ok(r) => r,
            Err((at, why)) => {
                return RecordingReport {
                    name: name.into(),
                    captured: rec.header.source.kind == SourceKind::Captured,
                    ticks: rec.rows.len(),
                    failures: vec![format!("{at} panicked: {why}")],
                    ..Default::default()
                };
            }
        };
        // Again in a fresh port: the port's own determinism. The differences
        // from the recording stay the first run's; this adds where the two
        // runs part (a difference after it may be chance).
        let line = match run(&mut Default::default()) {
            Err((at, why)) => Some(format!("{at}: the port is not deterministic: only the second run panicked: {why}")),
            Ok((_, second)) => parting(&first, &second),
        };
        if let Some(line) = line {
            out.failures.insert(0, line);
        }
        out
    }

    /// `at` says where the replay is, for a panic's failure. Also gives
    /// every quantity the port had at each moment it ran, for
    /// [`parting`].
    fn replay(
        &self,
        name: &str,
        s: &Scenario,
        rec: &Recording,
        observed: &mut BTreeSet<String>,
        at: &mut String,
    ) -> (RecordingReport, Trail) {
        let mut out = RecordingReport {
            name: name.into(),
            captured: rec.header.source.kind == SourceKind::Captured,
            ticks: rec.rows.len(),
            failures: Vec::new(),
            differences: Vec::new(),
            values: Vec::new(),
        };
        let mut port = (self.port)();
        if let Err(e) = port.launch(&s.kind, &s.parameters) {
            out.failures.push(format!("the launcher failed: {e}"));
            return (out, Trail::new());
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
        // A quantity [compare] names that the recording lacks would never be
        // compared, and with none in common the replay would pass unrun.
        let unrecorded: Vec<&String> = s.compare.keys().filter(|q| !rec.header.quantities.contains_key(*q)).collect();
        if !unrecorded.is_empty() || compared.is_empty() {
            out.failures.push(if unrecorded.is_empty() {
                "none of the quantities in [compare] is in the recording".to_string()
            } else {
                format!(
                    "[compare] names {}, which the recording does not hold: record it again",
                    unrecorded.iter().map(|q| q.as_str()).collect::<Vec<_>>().join(", ")
                )
            });
            return (out, Trail::new());
        }
        let mut differed: Vec<Difference> = Vec::new();
        let mut trail: Vec<Values> = Vec::new();
        let mut had = Trail::new();
        let mut check = |port: &mut P, at: &str, values: &Values, differed: &mut Vec<Difference>| {
            let now = match port.observe().and_then(|o| gather(o.values())) {
                Ok(now) => now,
                Err(e) => return Some(format!("{at}: {e}")),
            };
            observed.extend(now.keys().map(|q| q.to_string()));
            trail.push(
                compared.iter().filter_map(|(q, _)| Some(((*q).clone(), now.get(q.as_str())?.clone()))).collect(),
            );
            had.push((at.to_string(), now.clone()));
            for (q, tol) in &compared {
                if differed.iter().any(|d| &d.quantity == *q) {
                    continue;
                }
                let Some(want) = values.get(*q) else { continue };
                let got = now.get(q.as_str());
                if got.is_none_or(|got| differs(got, want, *tol)) {
                    differed.push(Difference {
                        quantity: (*q).clone(),
                        at: at.to_string(),
                        expected: want.clone(),
                        got: got.cloned(),
                    });
                }
            }
            None
        };
        *at = "initial".into();
        if let Some(e) = check(&mut port, "initial", &rec.header.initial, &mut differed) {
            out.failures.push(e);
            return (out, had);
        }
        for t in rec.expand() {
            if differed.len() == compared.len() {
                break;
            }
            *at = format!("tick {}", t.tick);
            match Pad::of(&t) {
                Ok(pad) => port.tick(&pad),
                Err(e) => {
                    out.failures.push(e);
                    return (out, had);
                }
            }
            if let Some(e) = check(&mut port, &format!("tick {}", t.tick), &t.values, &mut differed) {
                out.failures.push(e);
                return (out, had);
            }
        }
        if !differed.is_empty() {
            out.values = trail;
        }
        out.failures = failures(&differed);
        out.differences = differed;
        (out, had)
    }

    /// Runs every scenario, prints the results and panics when one fails.
    pub fn run(self) {
        let report = self.report();
        let mut failed = Vec::new();
        for s in &report.scenarios {
            let ticks: usize = s.recordings.iter().map(|r| r.ticks).sum();
            println!(
                "{} ({}): {} recordings, {ticks} ticks, {}",
                s.id,
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

/// The quantities the port has, by name; a name given twice is an error.
pub(crate) fn gather(values: Vec<(&'static str, Option<Value>)>) -> Result<BTreeMap<&'static str, Value>, String> {
    let mut out = BTreeMap::new();
    for (name, v) in values {
        let Some(v) = v else { continue };
        if out.insert(name, v).is_some() {
            return Err(format!("{name} is observed by two functions: observe each in one place"));
        }
    }
    Ok(out)
}

/// Every quantity the port had at each moment of a replay: `initial`,
/// `tick N`.
type Trail = Vec<(String, BTreeMap<&'static str, Value>)>;

/// Where two runs of one recording first part: the moment and the first
/// quantity (by name) that differs.
fn parting(first: &Trail, second: &Trail) -> Option<String> {
    let Some((at, a, b)) = first.iter().zip(second).find(|((_, a), (_, b))| a != b).map(|((at, a), (_, b))| (at, a, b))
    else {
        // The same as far as both went: one stopping sooner is a parting too.
        let end = |t: &Trail| t.last().map_or("before initial".to_string(), |(at, _)| format!("after {at}"));
        return (first.len() != second.len()).then(|| {
            format!("the port is not deterministic: the first run stopped {}, the second {}", end(first), end(second))
        });
    };
    let q = a.keys().chain(b.keys()).filter(|q| a.get(*q) != b.get(*q)).min()?;
    let had = |m: &BTreeMap<&str, Value>| m.get(q).map_or("none".into(), show);
    Some(format!("{at} {q}: the port is not deterministic: first run {}, second run {}", had(a), had(b)))
}

/// A caught panic's message.
pub fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|m| m.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into())
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

/// The failures differences make: the quantities that differ, in the
/// order they first did, and those the port had none of where the
/// recording had a value.
fn failures(d: &[Difference]) -> Vec<String> {
    let names = |missing: bool| d.iter().filter(move |d| d.got.is_none() == missing).map(|d| d.quantity.as_str());
    let (differ, missing): (Vec<&str>, Vec<&str>) = (names(false).collect(), names(true).collect());
    let mut out = Vec::new();
    if !differ.is_empty() {
        out.push(format!("differs from the recording in {}", differ.join(", ")));
    }
    if !missing.is_empty() {
        out.push(format!("the port has no {} where the recording has a value", missing.join(", ")));
    }
    out
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the game's build generates from a glossary with one quantity, x.
    struct Observed {
        x: Option<i32>,
    }

    impl Observation for Observed {
        fn values(&self) -> Vec<(&'static str, Option<Value>)> {
            vec![("x", self.x.map(|v| Value::Int(v as i128)))]
        }
    }

    /// Walkers moving right while RIGHT is held, slower on an analog pad,
    /// and by the left stick's raw byte from centre. The observer follows
    /// the first.
    #[derive(Default)]
    struct Walk {
        kinds: Vec<&'static str>,
        walkers: Vec<i32>,
        /// Nudges the last walker each tick when set.
        nudge_last: bool,
        /// Panics once a walker passes this.
        floor: Option<i32>,
        /// Gives x twice.
        twice: bool,
    }

    impl Walk {
        fn new() -> Walk {
            Walk { kinds: vec!["level"], ..Default::default() }
        }
    }

    impl Port for Walk {
        type Observed = Vec<(&'static str, Option<Value>)>;

        fn kinds(&self) -> Vec<String> {
            self.kinds.iter().map(|k| k.to_string()).collect()
        }

        fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
            assert!(self.kinds.contains(&kind));
            let x = p.get("x").and_then(|x| x.as_i64()).ok_or("x: a number")?;
            self.walkers.push(x as i32);
            Ok(())
        }

        fn tick(&mut self, pad: &Pad) {
            for w in &mut self.walkers {
                if pad.held(Button::Right) {
                    *w += if pad.analog { 50 } else { 100 };
                }
                *w += i32::from(pad.sticks[0]) - 128;
                if self.floor.is_some_and(|f| *w > f) {
                    panic!("walked off the floor");
                }
            }
            if self.nudge_last {
                *self.walkers.last_mut().unwrap() += 1;
            }
        }

        fn observe(&mut self) -> Result<Self::Observed, String> {
            let mut v = Observed { x: self.walkers.first().copied() }.values();
            if self.twice {
                v.extend(Observed { x: Some(1) }.values());
            }
            Ok(v)
        }
    }

    const RECORDING: &str = r#"{"format":"scenario-recording","format_version":1,"scenario":"walk","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0"},"quantities":{"x":{"type":"int","bits":32,"signed":true}},"initial":{"x":4096}}
{"tick":1,"field":1,"pad":["RIGHT"],"values":{"x":4196}}
{"tick":2,"field":2,"values":{"x":4296}}
{"tick":3,"field":3,"pad":[]}
"#;

    const SCENARIO: &str =
        "moment = \"m\"\nchain = \"c\"\nkind = \"level\"\n[compare]\nx = 0\n[parameters]\nx = 4096\n";

    fn workspace(scenario: &str, recording: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("psx-scenario-runner-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scenarios/walk")).unwrap();
        std::fs::write(root.join("scenarios/walk.toml"), scenario).unwrap();
        std::fs::write(root.join("scenarios/walk/a.jsonl"), recording).unwrap();
        root
    }

    #[test]
    fn a_port_that_matches_passes() {
        let root = workspace(SCENARIO, RECORDING);
        let r = Scenarios::new(&root, Walk::new).report();
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
        let s = &Scenarios::new(&root, Walk::new).report().scenarios[0];
        assert!(s.passed(), "{s:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_difference_names_the_quantity_and_keeps_where_for_the_plots() {
        let root = workspace(&SCENARIO.replace("x = 4096", "x = 4095 "), &RECORDING.replace("4296", "4300"));
        let r = Scenarios::new(&root, Walk::new).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["differs from the recording in x"]);
        let d = &r.scenarios[0].recordings[0].differences[0];
        assert_eq!((d.at.as_str(), &d.expected, &d.got), ("initial", &Value::Int(4096), &Some(Value::Int(4095))));
        // Within the tolerance, the first tick past it is where.
        let tolerant = SCENARIO.replace("x = 0", "x = 3").replace("x = 4096", "x = 4095 ");
        let root2 = workspace(&tolerant, &RECORDING.replace("4296", "4300"));
        let r = Scenarios::new(&root2, Walk::new).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["differs from the recording in x"]);
        assert_eq!(r.scenarios[0].recordings[0].differences[0].at, "tick 2");
        // What the port had, initially and after each tick it ran.
        let had: Vec<_> = r.scenarios[0].recordings[0].values.iter().map(|v| v["x"].clone()).collect();
        assert_eq!(had, [Value::Int(4095), Value::Int(4195), Value::Int(4295)]);
        let missing = SCENARIO.replace("kind = \"level\"", "kind = \"race\"");
        let root3 = workspace(&missing, RECORDING);
        let r = Scenarios::new(&root3, Walk::new).report();
        assert_eq!(r.scenarios[0].problems, ["no launcher registered for scenario kind race"]);
        // The same quantity given twice.
        let root4 = workspace(SCENARIO, RECORDING);
        let r = Scenarios::new(&root4, || Walk { twice: true, ..Walk::new() }).report();
        assert_eq!(
            r.scenarios[0].recordings[0].failures,
            ["initial: x is observed by two functions: observe each in one place"]
        );
        for d in [root, root2, root3, root4] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_button_names_and_a_bad_one_fails_its_tick() {
        assert_eq!(Button::ALL.map(|b| Button::parse(&b.name().to_lowercase())), Button::ALL.map(Some));
        let root = workspace(SCENARIO, &RECORDING.replace("[\"RIGHT\"]", "[\"TURBO\"]"));
        let r = Scenarios::new(&root, Walk::new).report();
        assert_eq!(r.scenarios[0].recordings[0].failures, ["tick 1: no button \"TURBO\""]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_port_that_differs_between_runs_is_not_deterministic() {
        static LAUNCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = workspace(SCENARIO, RECORDING);
        // Every other port nudges the walker the observer follows.
        let flaky = || {
            let n = LAUNCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Walk { nudge_last: n.is_multiple_of(2), ..Walk::new() }
        };
        let r = Scenarios::new(&root, flaky).report();
        let failures = &r.scenarios[0].recordings[0].failures;
        let parted = ["first run 4197, second run 4196", "first run 4196, second run 4197"]
            .map(|runs| format!("tick 1 x: the port is not deterministic: {runs}"));
        assert!(parted.contains(&failures[0]), "{failures:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_panic_fails_its_recording_and_the_rest_still_run() {
        let root = workspace(SCENARIO, RECORDING);
        std::fs::create_dir_all(root.join("scenarios/fragile")).unwrap();
        std::fs::write(root.join("scenarios/fragile.toml"), SCENARIO.replace("\"level\"", "\"fragile\"")).unwrap();
        std::fs::write(root.join("scenarios/fragile/a.jsonl"), RECORDING.replace("\"walk\"", "\"fragile\"")).unwrap();
        let fragile = || Walk { kinds: vec!["level", "fragile"], ..Walk::new() };
        // The fragile kind's walker breaks past 4196; the level's does not.
        struct Both(Walk);
        impl Port for Both {
            type Observed = Vec<(&'static str, Option<Value>)>;
            fn kinds(&self) -> Vec<String> {
                self.0.kinds()
            }
            fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
                if kind == "fragile" {
                    self.0.floor = Some(4196);
                }
                self.0.launch(kind, p)
            }
            fn tick(&mut self, pad: &Pad) {
                self.0.tick(pad)
            }
            fn observe(&mut self) -> Result<Self::Observed, String> {
                self.0.observe()
            }
        }
        let r = Scenarios::new(&root, move || Both(fragile())).report();
        let by_id = |id: &str| r.scenarios.iter().find(|s| s.id == id).unwrap();
        assert_eq!(by_id("fragile").recordings[0].failures, ["tick 2 panicked: walked off the floor"]);
        assert!(by_id("walk").passed(), "{:?}", by_id("walk"));
        let _ = std::fs::remove_dir_all(root);
    }
}

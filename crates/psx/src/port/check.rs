//! A feature's mechanics as the port's stages see them: whether each is
//! captured (stage 2), and `port check`, which runs the features' scenarios
//! against the port (the game's `tests/scenarios.rs`,
//! `psx_bevy::scenario`) and writes their scenario verdicts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::next::Git;
use super::verdict::{self, Kind, Verdict};
use super::{Entry, Port};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use crate::mechanic::workspace;

/// What stops the mechanic from counting as captured, one line each; empty
/// when it is: its files check clean, it has a captured trace, every
/// captured trace's read audit is clear, and NOTES.md names by its PC each
/// branch that no captured trace took both ways.
pub fn uncaptured(root: &Path, id: &str) -> Vec<String> {
    let l = match workspace::load(root, id) {
        Ok(l) => l,
        Err(e) => return vec![e.to_string()],
    };
    let mut out = l.problems.clone();
    let traces = match workspace::traces(&l) {
        Ok(t) => t,
        Err(e) => return vec![e.to_string()],
    };
    let notes = std::fs::read_to_string(l.dir.join("NOTES.md")).unwrap_or_default().to_ascii_lowercase();
    let mut captured = 0;
    let mut ways: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for (name, problems, header) in &traces {
        let file = format!("mechanics/{id}/traces/{name}.trace.jsonl");
        out.extend(problems.iter().map(|p| format!("{file}: {p}")));
        let Some(h) = header.as_ref().filter(|h| h.source.kind == psx_mechanics::trace::SourceKind::Captured) else {
            continue;
        };
        captured += 1;
        match &h.audit {
            None => out.push(format!("{file}: no read audit")),
            Some(a) => {
                if !a.unaccounted.is_empty() {
                    out.push(format!(
                        "{file}: the read audit lists {} reads the binding neither binds nor explains",
                        a.unaccounted.len()
                    ));
                }
                if !a.unaccounted_writes.is_empty() {
                    out.push(format!(
                        "{file}: the write audit lists {} stores read afterwards that the binding neither binds nor explains",
                        a.unaccounted_writes.len()
                    ));
                }
            }
        }
        if let Some(c) = &h.coverage {
            merge(&mut ways, &took(c));
        }
    }
    for (pc, (t, n)) in &ways {
        let missed = match (t, n) {
            (true, true) => continue,
            (false, false) => "no trace ran",
            (true, false) => "no trace fell through",
            (false, true) => "no trace took",
        };
        if !pc.is_empty() && !notes.contains(pc.as_str()) {
            out.push(format!("mechanics/{id}/NOTES.md: does not list the branch at {pc}, which {missed}"));
        }
    }
    if captured == 0 {
        out.push(format!("mechanics/{id}: no captured trace (psx mechanic capture)"));
    }
    out.sort();
    out.dedup();
    out
}

/// Branch PC -> whether a trace took it, and whether it fell through.
type Ways = BTreeMap<String, (bool, bool)>;

/// The ways a trace took each branch of its walk: both, unless missed says
/// otherwise. A trace from before `branches` gives only what it missed.
fn took(c: &psx_mechanics::trace::Coverage) -> Ways {
    let pc = |at: &serde_json::Map<String, Value>| {
        at.get("pc").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase()
    };
    let mut took: Ways = c.branches.iter().map(|b| (pc(b), (true, true))).collect();
    for m in &c.missed {
        let ways = match m.note.as_deref() {
            Some("never fell through") => (true, false),
            Some("never taken") => (false, true),
            _ => (false, false),
        };
        took.insert(pc(&m.at), ways);
    }
    took
}

fn merge(into: &mut Ways, from: &Ways) {
    for (p, (t, n)) in from {
        let w = into.entry(p.clone()).or_insert((false, false));
        *w = (w.0 || *t, w.1 || *n);
    }
}

/// The branch outcomes (each way of each branch) in `ways`.
fn outcomes(ways: &Ways) -> BTreeSet<(String, bool)> {
    ways.iter()
        .flat_map(|(pc, (t, n))| [(pc, true, *t), (pc, false, *n)])
        .filter(|(_, _, took)| *took)
        .map(|(pc, way, _)| (pc.clone(), way))
        .collect()
}

/// A recording the port does not pass, by the last check: the first tick a
/// quantity differs at (0: the initial moment) and that line, or only a line
/// when it did not run that far.
#[derive(Clone, Debug, PartialEq)]
pub struct Failing {
    pub tick: Option<u64>,
    pub first: String,
}

impl Failing {
    fn describe(&self) -> String {
        match self.tick {
            Some(0) => "fails at its initial moment".into(),
            Some(t) => format!("fails at tick {t}"),
            None => format!("fails: {}", self.first),
        }
    }
}

/// How the port ran each scenario recording in the last check at `head`,
/// keyed `<scenario>/<recording>`: None when it passed. None when no check
/// ran at `head`.
pub fn port_runs(root: &Path, head: Option<&str>) -> Option<BTreeMap<String, Option<Failing>>> {
    let report = last_report(root, head).ok()?;
    let tick_of = |l: &str| {
        if l.starts_with("initial ") {
            Some(0)
        } else {
            l.strip_prefix("tick ").and_then(|r| r.split(' ').next()).and_then(|n| n.parse().ok())
        }
    };
    let mut out = BTreeMap::new();
    for s in &report.scenarios {
        for r in &s.recordings {
            let failing = if let Some(p) = s.problems.first() {
                Some(Failing { tick: None, first: p.clone() })
            } else {
                let first = r.failures.iter().filter(|l| tick_of(l).is_some()).min_by_key(|l| tick_of(l));
                first.or(r.failures.first()).map(|l| Failing { tick: tick_of(l), first: l.clone() })
            };
            out.insert(format!("{}/{}", s.id, r.name), failing);
        }
    }
    Some(out)
}

/// A mechanic's branch outcomes in the port: every way of every branch its
/// captured traces took, and how many of them the traces of recordings the
/// port passes took (`runs`, from `port_runs`; null without a check at
/// HEAD). Each recording the port does not pass is listed with the
/// outcomes only it holds of the unconfirmed ones.
pub fn port_coverage(root: &Path, id: &str, runs: Option<&BTreeMap<String, Option<Failing>>>) -> Value {
    let Ok(l) = workspace::load(root, id) else { return json!({"mechanic": id, "outcomes": 0}) };
    let traces = workspace::traces(&l).unwrap_or_default();
    let (mut reference, mut port) = (Ways::new(), Ways::new());
    let mut failing: Vec<(String, String, Ways)> = Vec::new();
    for (name, _, header) in &traces {
        let Some(c) = header
            .as_ref()
            .filter(|h| h.source.kind == psx_mechanics::trace::SourceKind::Captured)
            .and_then(|h| h.coverage.as_ref())
        else {
            continue;
        };
        let w = took(c);
        merge(&mut reference, &w);
        match runs.map(|r| r.get(name)) {
            Some(Some(None)) => merge(&mut port, &w),
            Some(Some(Some(f))) => failing.push((name.clone(), f.describe(), w)),
            Some(None) => failing.push((name.clone(), "not run".into(), w)),
            None => {}
        }
    }
    let (all, confirmed) = (outcomes(&reference), outcomes(&port));
    let mut out = json!({"mechanic": id, "outcomes": all.len()});
    if runs.is_some() {
        out["confirmed"] = json!(confirmed.len());
        let unconfirmed: Vec<Value> = failing
            .iter()
            .map(|(name, why, w)| (name, why, outcomes(w).iter().filter(|o| !confirmed.contains(*o)).count()))
            .filter(|(_, _, n)| *n > 0)
            .map(|(name, why, n)| json!({"recording": name, "why": why, "outcomes": n}))
            .collect();
        out["unconfirmed"] = json!(unconfirmed);
    }
    out
}

/// The line `port status --plain` gives a mechanic's `port_coverage`.
pub fn coverage_line(c: &Value) -> String {
    let id = c["mechanic"].as_str().unwrap_or_default();
    let all = c["outcomes"].as_u64().unwrap_or(0);
    let Some(confirmed) = c["confirmed"].as_u64() else {
        return format!("{id}: {all} branch outcomes in its traces; no psx port check at HEAD");
    };
    let mut s = format!("{id}: {confirmed} of {all} branch outcomes confirmed in the port");
    if confirmed < all {
        let list: Vec<String> = c["unconfirmed"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|u| format!("{}, which {}", u["recording"].as_str().unwrap_or(""), u["why"].as_str().unwrap_or("")))
            .collect();
        let other = match all - confirmed {
            1 => "the other one appears".to_string(),
            n => format!("the other {n} appear"),
        };
        s.push_str(&format!("; {other} only in {}", list.join(" and ")));
    }
    s
}

/// The trace steps that ran in a recording's tick: what each mechanic's
/// reference did there, one line per step.
pub fn steps_at(root: &Path, recording: &str, tick: u64) -> Vec<String> {
    let mut out = Vec::new();
    for id in workspace::ids(root) {
        let file = format!("mechanics/{id}/traces/{recording}.trace.jsonl");
        let Ok(t) = std::fs::read_to_string(root.join(&file))
            .map_err(|e| e.to_string())
            .and_then(|t| psx_mechanics::Trace::parse(&t))
        else {
            continue;
        };
        let steps: Vec<String> = t.steps.iter().filter(|s| s.tick == Some(tick)).map(|s| s.step.to_string()).collect();
        if !steps.is_empty() {
            out.push(format!("{file}: step {}", steps.join(", ")));
        }
    }
    out
}

/// For each of the feature's recordings the port fails at a tick, the trace
/// steps of that tick.
pub fn diagnosis(root: &Path, e: &Entry, runs: Option<&BTreeMap<String, Option<Failing>>>) -> Vec<Value> {
    let Some(runs) = runs else { return Vec::new() };
    let mut out = Vec::new();
    for (name, f) in runs {
        let Some(Failing { tick: Some(tick), first }) = f else { continue };
        if !e.scenarios.iter().any(|s| name.split('/').next() == Some(s.as_str())) {
            continue;
        }
        out.push(json!({"recording": name, "tick": tick, "first": first, "steps": steps_at(root, name, *tick)}));
    }
    out
}

/// What stops the feature's mechanics from counting as captured, each line
/// naming its mechanic; empty when they all are. A feature that names no
/// mechanics is not captured: every feature runs code of the original's.
pub fn feature_uncaptured(root: &Path, e: &Entry) -> Vec<String> {
    if e.mechanics.is_empty() {
        return vec![format!(
            "{}: names no mechanics (find the steps the original runs for it and list their ids under mechanics)",
            e.file
        )];
    }
    e.mechanics.iter().flat_map(|m| uncaptured(root, m)).collect()
}

/// `psx_bevy::scenario::Report`, as the harness writes it.
#[derive(Debug, Default, Deserialize)]
pub struct ScenarioReport {
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub quantities: Vec<String>,
    #[serde(default)]
    pub scenarios: Vec<ScenarioRun>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScenarioRun {
    pub id: String,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub recordings: Vec<RecordingRun>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RecordingRun {
    pub name: String,
    #[serde(default)]
    pub captured: bool,
    #[serde(default)]
    pub failures: Vec<String>,
}

/// The scenario run's failures for one feature: no scenarios, what the
/// report says of each, and recordings on disk it did not run.
fn scenario_failures(root: &Path, e: &Entry, report: &ScenarioReport) -> Vec<String> {
    if e.scenarios.is_empty() {
        return vec![format!("{}: lists no scenarios", e.file)];
    }
    let mut out = Vec::new();
    for id in &e.scenarios {
        let Some(s) = report.scenarios.iter().find(|s| &s.id == id) else {
            out.push(format!("{id}: not run (no scenarios/{id}.toml)"));
            continue;
        };
        out.extend(s.problems.iter().map(|p| format!("{id}: {p}")));
        for r in &s.recordings {
            out.extend(r.failures.iter().map(|f| format!("{id} {}: {f}", r.name)));
        }
        let on_disk = crate::scenario::Loaded::load(root, id).recordings;
        for (name, _, _) in &on_disk {
            if !s.recordings.iter().any(|r| &r.name == name) {
                out.push(format!("{id} {name}: not run"));
            }
        }
        if !s.recordings.iter().any(|r| r.captured) && s.problems.is_empty() {
            out.push(format!("{id}: no captured recording run"));
        }
    }
    out
}

/// What the feature's plugins have not registered, by the last check when
/// it ran at `head`: a launcher for each of its scenarios' kinds, and each
/// quantity they compare observed. Empty when every one is.
pub fn unimplemented(root: &Path, e: &Entry, head: Option<&str>) -> Vec<String> {
    let report = match last_report(root, head) {
        Ok(r) => r,
        Err(why) => return vec![why.into()],
    };
    let mut out = Vec::new();
    for id in &e.scenarios {
        let Some(s) = crate::scenario::Loaded::load(root, id).scenario else {
            out.push(format!("{id}: no scenario file"));
            continue;
        };
        if !report.kinds.contains(&s.kind) {
            out.push(format!("{id}: no launcher for scenario kind {}", s.kind));
        }
        for q in s.compare.keys().filter(|q| !report.quantities.contains(q)) {
            out.push(format!("{id}: the port observes no {q} (a feature plugin's app.observe)"));
        }
    }
    out
}

/// The scenario report of the last check, when it ran at `head`.
fn last_report(root: &Path, head: Option<&str>) -> Result<ScenarioReport, &'static str> {
    let last = std::fs::read(root.join(LAST)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(last) = last.filter(|l| head.is_some() && l["commit"].as_str() == head) else {
        return Err("no psx port check at HEAD");
    };
    serde_json::from_value::<ScenarioReport>(last["scenarios"].clone())
        .map_err(|_| "the last psx port check ran no scenarios")
}

/// `.psx/port/last.json`: the last check's scenario report and the commit
/// it ran at.
const LAST: &str = ".psx/port/last.json";

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortCheckArgs {
    /// The feature (F-NNNN); default every catalogued feature of the goal's
    /// closure.
    #[serde(default)]
    pub feature: Option<String>,
}

/// A test's report, or why the test wrote none.
type Ran<T> = Result<T, String>;

/// The game's scenario test: `cargo test --locked --test scenarios`, with
/// its report written to `scenarios`. A test that wrote none gets the
/// output's error lines.
fn run_tests(root: &Path, scenarios: &Path) -> crate::Result<Ran<ScenarioReport>> {
    let _ = std::fs::remove_file(scenarios);
    let out = std::process::Command::new("cargo")
        .args(["test", "--locked", "--no-fail-fast", "--manifest-path", "game/Cargo.toml", "--test", "scenarios"])
        .env("PSX_SCENARIO_REPORT", scenarios)
        .current_dir(root)
        .output()
        .map_err(|e| Error::invalid(format!("cargo: {e}")))?;
    let text = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = text.lines().filter(|l| l.starts_with("error")).take(5).collect();
    let why =
        if errors.is_empty() { text.lines().last().unwrap_or("no output").to_string() } else { errors.join("; ") };
    match std::fs::read(scenarios) {
        Ok(b) => {
            serde_json::from_slice(&b).map(Ok).map_err(|e| Error::invalid(format!("{}: {e}", scenarios.display())))
        }
        Err(_) => Ok(Err(format!("the scenario test did not run: {why}"))),
    }
}

impl PortCheckArgs {
    fn run(&self, root: &Path) -> crate::Result<Value> {
        let port = Port::load(root)?;
        let git = Git::read(root);
        let Some(head) = git.head.clone().filter(|_| git.clean) else {
            return Err(Error::invalid(
                "commit first: a verdict counts only for a commit with no uncommitted changes (for a quick look, cargo test -p <game> --test scenarios)",
            ));
        };
        let features: Vec<&Entry> = match &self.feature {
            Some(id) => vec![port.entries.get(id).ok_or_else(|| Error::invalid(format!("{id} is not catalogued")))?],
            None => port.closure().iter().filter_map(|id| port.entries.get(id)).collect(),
        };
        let scenarios_path = root.join(".psx/port/scenarios.json");
        std::fs::create_dir_all(scenarios_path.parent().unwrap()).map_err(|e| Error::io(&scenarios_path, e))?;
        let ran = run_tests(root, &scenarios_path)?;
        let after = Git::read(root);
        if after.head != git.head || !after.clean {
            return Err(Error::invalid(
                "the workspace changed while the check ran (cargo may have updated Cargo.lock): commit and check again",
            ));
        }
        let kept =
            |p: &Path| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap_or_default()).unwrap_or_default();
        let last = json!({"commit": head, "at": crate::stamp::utc_now(),
                          "scenarios": if ran.is_ok() { kept(&scenarios_path) } else { Value::Null }});
        let p = root.join(LAST);
        crate::write_atomic(&p, &serde_json::to_vec_pretty(&last).unwrap_or_default()).map_err(|e| Error::io(&p, e))?;
        let mut verdicts = Vec::new();
        for e in features {
            let failures: Vec<String> = match &ran {
                Ok(r) => scenario_failures(root, e, r),
                Err(why) => vec![why.clone()],
            };
            let kind = Kind::Scenario;
            let v = Verdict {
                feature: e.id.clone(),
                kind: kind.name().into(),
                commit: head.clone(),
                clean: true,
                at: crate::stamp::utc_now(),
                passed: failures.is_empty(),
                failures,
            };
            let p = verdict::path(root, &e.id, kind);
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| Error::io(&p, e))?;
            crate::write_atomic(&p, &serde_json::to_vec_pretty(&v).unwrap_or_default())
                .map_err(|e| Error::io(&p, e))?;
            verdicts.push(json!({"feature": v.feature, "kind": v.kind, "passed": v.passed, "failures": v.failures}));
        }
        Ok(json!({"commit": head, "verdicts": verdicts}))
    }
}

impl Op for PortCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(&ctx.root))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::workspace;
    use super::*;

    const MECHANIC: &str = "//! Walk\n//!\n//! s\n\npub const ID: &str = \"demo.walk\";\npub const VERSION: u32 = 1;\npub const RATE: &str = \"tick\";\npub const RATE_HZ: u32 = 25;\n\npub struct State {\n    pub speed: i32,\n}\n";

    fn trace(recording: &str, audit: &str, missed: &str) -> String {
        format!(
            "{{\"format\":\"mechanic-trace\",\"format_version\":1,\"mechanic\":\"demo.walk\",\"mechanic_version\":1,\"source\":{{\"kind\":\"captured\",\"platform\":\"psx\",\"image_sha256\":\"00\"}},\"producer\":{{\"name\":\"psx\",\"version\":\"0\"}},\"scenario\":\"walk\",\"recording\":\"{recording}\",\"params\":{{}},\"initial_state\":{{\"speed\":0}}{audit},\"coverage\":{{\"reached\":3,\"total\":4,\"missed\":[{missed}],\"branches\":[{{\"pc\":\"0x800432a8\"}},{{\"pc\":\"0x800432c0\"}}]}}}}\n{{\"step\":0,\"state\":{{\"speed\":1}}}}\n"
        )
    }

    #[test]
    fn a_mechanic_is_captured_once_its_audit_is_clear_and_its_missed_branches_are_listed() {
        let (d, _) = workspace("description = \"x\"\nfeatures = []\n", &[]);
        let m = d.0.join("mechanics/demo.walk");
        std::fs::create_dir_all(m.join("traces/walk")).unwrap();
        std::fs::create_dir_all(m.join("src")).unwrap();
        std::fs::write(m.join("Cargo.toml"), "[package]\nname = \"demo-walk\"\n").unwrap();
        std::fs::write(m.join("src/lib.rs"), MECHANIC).unwrap();
        let u = uncaptured(&d.0, "demo.walk");
        assert_eq!(
            u,
            [
                "mechanics/demo.walk/binding: not written yet",
                "mechanics/demo.walk: no captured trace (psx mechanic capture)"
            ]
        );
        // The binding's own checks are capture's (B1); a trace is what this adds.
        let missed = "{\"at\":{\"pc\":\"0x800432a8\"},\"note\":\"never taken\"}";
        std::fs::write(
            m.join("traces/walk/a.trace.jsonl"),
            trace("a", ",\"audit\":{\"unaccounted\":[{\"at\":{\"pc\":\"0x80043260\"}}]}", missed),
        )
        .unwrap();
        let u = uncaptured(&d.0, "demo.walk");
        assert!(u.contains(&"mechanics/demo.walk/traces/walk/a.trace.jsonl: the read audit lists 1 reads the binding neither binds nor explains".to_string()), "{u:?}");
        assert!(
            u.contains(
                &"mechanics/demo.walk/NOTES.md: does not list the branch at 0x800432a8, which no trace took"
                    .to_string()
            ),
            "{u:?}"
        );
        std::fs::write(m.join("traces/walk/a.trace.jsonl"), trace("a", ",\"audit\":{\"unaccounted\":[]}", missed))
            .unwrap();
        std::fs::write(m.join("NOTES.md"), "Never taken: 0x800432A8, the slope branch.\n").unwrap();
        assert_eq!(uncaptured(&d.0, "demo.walk"), ["mechanics/demo.walk/binding: not written yet"]);
        // A branch counts once some trace took each way of it.
        std::fs::write(m.join("NOTES.md"), "").unwrap();
        let other = "{\"at\":{\"pc\":\"0x800432a8\"},\"note\":\"never fell through\"}";
        std::fs::write(m.join("traces/walk/b.trace.jsonl"), trace("b", ",\"audit\":{\"unaccounted\":[]}", other))
            .unwrap();
        assert_eq!(uncaptured(&d.0, "demo.walk"), ["mechanics/demo.walk/binding: not written yet"]);
        // A trace belongs to the recording its path names.
        std::fs::write(m.join("traces/walk/c.trace.jsonl"), trace("b", ",\"audit\":{\"unaccounted\":[]}", other))
            .unwrap();
        std::fs::write(m.join("traces/d.trace.jsonl"), trace("d", ",\"audit\":{\"unaccounted\":[]}", other)).unwrap();
        assert_eq!(
            uncaptured(&d.0, "demo.walk"),
            [
                "mechanics/demo.walk/binding: not written yet",
                "mechanics/demo.walk/traces/d.trace.jsonl: not captured from a scenario recording: psx mechanic capture --scenario S --recording R writes traces/S/R.trace.jsonl",
                "mechanics/demo.walk/traces/walk/c.trace.jsonl: its header names scenario \"walk\" and recording \"b\", its path walk/c",
            ]
        );
    }

    #[test]
    fn the_port_confirms_the_branch_outcomes_of_the_recordings_it_passes() {
        let (d, _) = workspace("description = \"x\"\nfeatures = []\n", &[]);
        let root = d.0.as_path();
        let m = root.join("mechanics/demo.walk");
        std::fs::create_dir_all(m.join("traces/walk")).unwrap();
        std::fs::create_dir_all(m.join("src")).unwrap();
        std::fs::write(m.join("Cargo.toml"), "[package]\nname = \"demo-walk\"\n").unwrap();
        std::fs::write(m.join("src/lib.rs"), MECHANIC).unwrap();
        let ticked = |recording: &str, note: &str| {
            let missed = format!("{{\"at\":{{\"pc\":\"0x800432a8\"}},\"note\":\"{note}\"}}");
            trace(recording, ",\"audit\":{\"unaccounted\":[]}", &missed).replace(
                "{\"step\":0,\"state\":{\"speed\":1}}",
                "{\"step\":0,\"state\":{\"speed\":1},\"tick\":1}\n{\"step\":1,\"state\":{\"speed\":2},\"tick\":2}",
            )
        };
        // a never takes the branch at 0x800432a8; b never falls through it.
        std::fs::write(m.join("traces/walk/a.trace.jsonl"), ticked("a", "never taken")).unwrap();
        std::fs::write(m.join("traces/walk/b.trace.jsonl"), ticked("b", "never fell through")).unwrap();
        assert_eq!(port_coverage(root, "demo.walk", None), json!({"mechanic": "demo.walk", "outcomes": 4}));
        let last = json!({"commit": "abc", "scenarios": {"scenarios": [{"id": "walk", "recordings": [
            {"name": "a", "captured": true, "failures": []},
            {"name": "b", "captured": true, "failures": ["tick 2 x: expected 1, got 2"]},
        ]}]}});
        std::fs::create_dir_all(root.join(".psx/port")).unwrap();
        std::fs::write(root.join(LAST), last.to_string()).unwrap();
        assert_eq!(port_runs(root, Some("def")), None);
        let runs = port_runs(root, Some("abc")).unwrap();
        assert_eq!(runs["walk/a"], None);
        let c = port_coverage(root, "demo.walk", Some(&runs));
        assert_eq!(
            coverage_line(&c),
            "demo.walk: 3 of 4 branch outcomes confirmed in the port; the other one appears only in walk/b, which fails at tick 2"
        );
        assert_eq!(steps_at(root, "walk/b", 2), ["mechanics/demo.walk/traces/walk/b.trace.jsonl: step 1"]);
    }
}

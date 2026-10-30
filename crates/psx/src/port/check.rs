//! `port check`, which runs the features' scenarios against the port (the
//! game's `tests/scenarios.rs`, `psx_bevy::scenario`) and writes their
//! scenario verdicts.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::next::Git;
use super::verdict::{self, Kind, Verdict};
use super::{Entry, Port};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

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

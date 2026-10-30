//! A feature's mechanics as the port's stages see them: whether each is
//! captured (stage 2), and `port check`, which runs the features' scenarios
//! against the port (the game's `tests/scenarios.rs`,
//! `psx_bevy::scenario`) and writes their scenario verdicts.

use std::collections::BTreeMap;
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
            Some(a) if !a.unaccounted.is_empty() => out.push(format!(
                "{file}: the read audit lists {} reads the binding neither binds nor explains",
                a.unaccounted.len()
            )),
            Some(_) => {}
        }
        if let Some(c) = &h.coverage {
            // The ways this trace took each branch of its walk: both, unless
            // missed says otherwise. A trace from before `branches` gives
            // only what it missed.
            let pc = |at: &serde_json::Map<String, Value>| {
                at.get("pc").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase()
            };
            let mut took: BTreeMap<String, (bool, bool)> = c.branches.iter().map(|b| (pc(b), (true, true))).collect();
            for m in &c.missed {
                let ways = match m.note.as_deref() {
                    Some("never fell through") => (true, false),
                    Some("never taken") => (false, true),
                    _ => (false, false),
                };
                took.insert(pc(&m.at), ways);
            }
            for (p, (t, n)) in took {
                let w = ways.entry(p).or_insert((false, false));
                *w = (w.0 || t, w.1 || n);
            }
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
    let last = std::fs::read(root.join(LAST)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(last) = last.filter(|l| head.is_some() && l["commit"].as_str() == head) else {
        return vec!["no psx port check at HEAD".into()];
    };
    let Ok(report) = serde_json::from_value::<ScenarioReport>(last["scenarios"].clone()) else {
        return vec!["the last psx port check ran no scenarios".into()];
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

    fn trace(audit: &str, missed: &str) -> String {
        format!(
            "{{\"format\":\"mechanic-trace\",\"format_version\":1,\"mechanic\":\"demo.walk\",\"mechanic_version\":1,\"source\":{{\"kind\":\"captured\",\"platform\":\"psx\",\"image_sha256\":\"00\"}},\"producer\":{{\"name\":\"psx\",\"version\":\"0\"}},\"params\":{{}},\"initial_state\":{{\"speed\":0}}{audit},\"coverage\":{{\"reached\":3,\"total\":4,\"missed\":[{missed}],\"branches\":[{{\"pc\":\"0x800432a8\"}},{{\"pc\":\"0x800432c0\"}}]}}}}\n{{\"step\":0,\"state\":{{\"speed\":1}}}}\n"
        )
    }

    #[test]
    fn a_mechanic_is_captured_once_its_audit_is_clear_and_its_missed_branches_are_listed() {
        let (d, _) = workspace("description = \"x\"\nfeatures = []\n", &[]);
        let m = d.0.join("mechanics/demo.walk");
        std::fs::create_dir_all(m.join("traces")).unwrap();
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
            m.join("traces/a.trace.jsonl"),
            trace(",\"audit\":{\"unaccounted\":[{\"at\":{\"pc\":\"0x80043260\"}}]}", missed),
        )
        .unwrap();
        let u = uncaptured(&d.0, "demo.walk");
        assert!(u.contains(&"mechanics/demo.walk/traces/a.trace.jsonl: the read audit lists 1 reads the binding neither binds nor explains".to_string()), "{u:?}");
        assert!(
            u.contains(
                &"mechanics/demo.walk/NOTES.md: does not list the branch at 0x800432a8, which no trace took"
                    .to_string()
            ),
            "{u:?}"
        );
        std::fs::write(m.join("traces/a.trace.jsonl"), trace(",\"audit\":{\"unaccounted\":[]}", missed)).unwrap();
        std::fs::write(m.join("NOTES.md"), "Never taken: 0x800432A8, the slope branch.\n").unwrap();
        assert_eq!(uncaptured(&d.0, "demo.walk"), ["mechanics/demo.walk/binding: not written yet"]);
        // A branch counts once some trace took each way of it.
        std::fs::write(m.join("NOTES.md"), "").unwrap();
        let other = "{\"at\":{\"pc\":\"0x800432a8\"},\"note\":\"never fell through\"}";
        std::fs::write(m.join("traces/b.trace.jsonl"), trace(",\"audit\":{\"unaccounted\":[]}", other)).unwrap();
        assert_eq!(uncaptured(&d.0, "demo.walk"), ["mechanics/demo.walk/binding: not written yet"]);
    }
}

//! A feature's mechanics as the port's stages see them: whether each is
//! captured (stage 2), and `port check`, which replays them against the
//! port (the game's `tests/mechanics.rs`, `psx_bevy::replay`) and writes
//! the features' replay verdicts.

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
/// captured trace's read audit is clear, and NOTES.md names each branch a
/// captured trace missed by its PC.
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
        for m in h.coverage.iter().flat_map(|c| &c.missed) {
            let pc = m.at.get("pc").and_then(Value::as_str).unwrap_or_default();
            if !pc.is_empty() && !notes.contains(&pc.to_ascii_lowercase()) {
                out.push(format!("mechanics/{id}/NOTES.md: does not list the branch at {pc} that {name} missed"));
            }
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
/// naming its mechanic; empty when they all are, or it has none.
pub fn feature_uncaptured(root: &Path, e: &Entry) -> Vec<String> {
    e.mechanics.iter().flat_map(|m| uncaptured(root, m)).collect()
}

/// `psx_bevy::replay::Report`, as the harness writes it: what the verdicts
/// need (the drift is kept whole in `.psx/replay/last.json`).
#[derive(Debug, Default, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub mechanics: Vec<MechanicReport>,
    #[serde(default)]
    pub unregistered: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MechanicReport {
    pub id: String,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub traces: Vec<TraceReport>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TraceReport {
    pub name: String,
    #[serde(default)]
    pub captured: bool,
    #[serde(default)]
    pub failure: Option<String>,
}

/// The replay's failures for one mechanic: what the report says, and the
/// traces on disk it did not replay.
fn mechanic_failures(root: &Path, id: &str, report: &Report) -> Vec<String> {
    let Some(m) = report.mechanics.iter().find(|m| m.id == id) else {
        return vec![if report.unregistered.iter().any(|u| u == id) {
            format!("{id}: no adapter registered in game/tests/mechanics.rs")
        } else {
            format!("{id}: not replayed (no mechanics/{id}/mechanic.toml, or no adapter)")
        }];
    };
    let mut out: Vec<String> = m.problems.iter().map(|p| format!("{id}: {p}")).collect();
    for t in &m.traces {
        if let Some(f) = &t.failure {
            out.push(format!("{id} {}: {f}", t.name));
        }
    }
    let on_disk = workspace::load(root, id).and_then(|l| workspace::traces(&l)).unwrap_or_default();
    for (name, _, _) in &on_disk {
        if !m.traces.iter().any(|t| &t.name == name) {
            out.push(format!("{id} {name}: not replayed"));
        }
    }
    if !m.traces.iter().any(|t| t.captured) && m.problems.is_empty() {
        out.push(format!("{id}: no captured trace replayed"));
    }
    out
}

/// `.psx/replay/last.json`: the last check's report and the commit it ran
/// at, for the drift in `port status`.
const LAST: &str = ".psx/replay/last.json";

/// The f32 drift of each exact mechanic, from the last check when it ran
/// at `head`.
pub fn drift(root: &Path, head: Option<&str>) -> Value {
    let Some(last) = std::fs::read(root.join(LAST)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return json!({"state": "missing"});
    };
    if head.is_none() || last["commit"].as_str() != head {
        return json!({"state": "stale", "commit": last["commit"]});
    }
    let mechanics: BTreeMap<String, Value> = last["report"]["mechanics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| !m["drift"].is_null())
        .map(|m| (m["id"].as_str().unwrap_or_default().to_string(), m["drift"].clone()))
        .collect();
    json!({"state": "current", "mechanics": mechanics})
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortCheckArgs {
    /// The feature (F-NNNN); default every catalogued feature of the goal's
    /// closure.
    #[serde(default)]
    pub feature: Option<String>,
}

/// The replay test: `cargo test --locked --test mechanics` in the game,
/// with the report written to `report`. Its output's error lines when it
/// wrote none.
fn run_replay(root: &Path, report: &Path) -> crate::Result<Result<Report, String>> {
    let _ = std::fs::remove_file(report);
    let out = std::process::Command::new("cargo")
        .args(["test", "--locked", "--manifest-path", "game/Cargo.toml", "--test", "mechanics"])
        .env("PSX_REPLAY_REPORT", report)
        .current_dir(root)
        .output()
        .map_err(|e| Error::invalid(format!("cargo: {e}")))?;
    match std::fs::read(report) {
        Ok(b) => serde_json::from_slice(&b).map(Ok).map_err(|e| Error::invalid(format!("{}: {e}", report.display()))),
        Err(_) => {
            let text = String::from_utf8_lossy(&out.stderr);
            let errors: Vec<&str> = text.lines().filter(|l| l.starts_with("error")).take(5).collect();
            let why = if errors.is_empty() {
                text.lines().last().unwrap_or("no output").to_string()
            } else {
                errors.join("; ")
            };
            Ok(Err(format!("the replay test did not run: {why}")))
        }
    }
}

impl PortCheckArgs {
    fn run(&self, root: &Path) -> crate::Result<Value> {
        let port = Port::load(root)?;
        let git = Git::read(root);
        let Some(head) = git.head.clone().filter(|_| git.clean) else {
            return Err(Error::invalid(
                "commit first: a verdict counts only for a commit with no uncommitted changes (for a quick look, cargo test -p <game> --test mechanics)",
            ));
        };
        let features: Vec<&Entry> = match &self.feature {
            Some(id) => vec![port.entries.get(id).ok_or_else(|| Error::invalid(format!("{id} is not catalogued")))?],
            None => port.closure().iter().filter_map(|id| port.entries.get(id)).collect(),
        };
        let report_path = root.join(".psx/replay/report.json");
        std::fs::create_dir_all(report_path.parent().unwrap()).map_err(|e| Error::io(&report_path, e))?;
        let replayed =
            if features.iter().any(|e| !e.mechanics.is_empty()) { Some(run_replay(root, &report_path)?) } else { None };
        let after = Git::read(root);
        if after.head != git.head || !after.clean {
            return Err(Error::invalid(
                "the workspace changed while the check ran (cargo may have updated Cargo.lock): commit and check again",
            ));
        }
        if let Some(Ok(_)) = &replayed {
            let report: Value =
                serde_json::from_slice(&std::fs::read(&report_path).unwrap_or_default()).unwrap_or_default();
            let last = json!({"commit": head, "at": crate::stamp::utc_now(), "report": report});
            let p = root.join(LAST);
            crate::write_atomic(&p, &serde_json::to_vec_pretty(&last).unwrap_or_default())
                .map_err(|e| Error::io(&p, e))?;
        }
        let mut verdicts = Vec::new();
        for e in features {
            let failures: Vec<String> = match &replayed {
                _ if e.mechanics.is_empty() => Vec::new(),
                Some(Ok(r)) => e.mechanics.iter().flat_map(|m| mechanic_failures(root, m, r)).collect(),
                Some(Err(why)) => vec![why.clone()],
                None => unreachable!("features with mechanics run the replay"),
            };
            let v = Verdict {
                feature: e.id.clone(),
                kind: Kind::Replay.name().into(),
                commit: head.clone(),
                clean: true,
                at: crate::stamp::utc_now(),
                passed: failures.is_empty(),
                failures,
            };
            let p = verdict::path(root, &e.id, Kind::Replay);
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| Error::io(&p, e))?;
            crate::write_atomic(&p, &serde_json::to_vec_pretty(&v).unwrap_or_default())
                .map_err(|e| Error::io(&p, e))?;
            verdicts.push(json!({"feature": v.feature, "kind": v.kind, "passed": v.passed,
                                 "mechanics": e.mechanics, "failures": v.failures}));
        }
        Ok(json!({"commit": head, "verdicts": verdicts, "drift": drift(root, Some(&head))["mechanics"]}))
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

    const MECHANIC: &str = "id = \"demo.walk\"\nversion = 1\ntitle = \"Walk\"\nsummary = \"s\"\nrate = { per = \"tick\", hz = 25 }\n\n[state.speed]\ntype = \"int\"\nbits = 32\nsigned = true\n";

    fn trace(audit: &str, missed: &str) -> String {
        format!(
            "{{\"format\":\"mechanic-trace\",\"format_version\":1,\"mechanic\":\"demo.walk\",\"mechanic_version\":1,\"source\":{{\"kind\":\"captured\",\"platform\":\"psx\",\"image_sha256\":\"00\"}},\"producer\":{{\"name\":\"psx\",\"version\":\"0\"}},\"params\":{{}},\"initial_state\":{{\"speed\":0}}{audit},\"coverage\":{{\"reached\":3,\"total\":4,\"missed\":[{missed}]}}}}\n{{\"step\":0,\"state\":{{\"speed\":1}}}}\n"
        )
    }

    #[test]
    fn a_mechanic_is_captured_once_its_audit_is_clear_and_its_missed_branches_are_listed() {
        let (d, _) = workspace("description = \"x\"\nfeatures = []\n", &[]);
        let m = d.0.join("mechanics/demo.walk");
        std::fs::create_dir_all(m.join("traces")).unwrap();
        std::fs::write(m.join("mechanic.toml"), MECHANIC).unwrap();
        let u = uncaptured(&d.0, "demo.walk");
        assert_eq!(
            u,
            [
                "mechanics/demo.walk/binding.toml: not written yet",
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
                &"mechanics/demo.walk/NOTES.md: does not list the branch at 0x800432a8 that a missed".to_string()
            ),
            "{u:?}"
        );
        std::fs::write(m.join("traces/a.trace.jsonl"), trace(",\"audit\":{\"unaccounted\":[]}", missed)).unwrap();
        std::fs::write(m.join("NOTES.md"), "Never taken: 0x800432A8, the slope branch.\n").unwrap();
        assert_eq!(uncaptured(&d.0, "demo.walk"), ["mechanics/demo.walk/binding.toml: not written yet"]);
    }

    #[test]
    fn a_feature_fails_replay_for_each_mechanic_not_replayed_cleanly() {
        let (d, _) = workspace("description = \"x\"\nfeatures = []\n", &[]);
        let r: Report = serde_json::from_value(json!({
            "mechanics": [{"id": "demo.walk", "exactness": "exact", "problems": [],
                           "traces": [{"name": "a", "captured": true, "steps": 3, "failure": "step 1: state speed: expected 2, got 1"}]},
                          {"id": "demo.jump", "exactness": "exact", "problems": [], "traces": [{"name": "s", "captured": false, "steps": 1}]}],
            "unregistered": ["demo.swim"],
        }))
        .unwrap();
        assert_eq!(mechanic_failures(&d.0, "demo.walk", &r), ["demo.walk a: step 1: state speed: expected 2, got 1"]);
        assert_eq!(mechanic_failures(&d.0, "demo.jump", &r), ["demo.jump: no captured trace replayed"]);
        assert_eq!(
            mechanic_failures(&d.0, "demo.swim", &r),
            ["demo.swim: no adapter registered in game/tests/mechanics.rs"]
        );
    }
}

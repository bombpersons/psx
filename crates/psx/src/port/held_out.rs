//! Held-out plays: recordings goal.toml's `held_out` lists, which port check
//! judges and the agent never sees. A play the person starts is held out
//! unless they give it to the agent; an agent asks for one with
//! `scenario_play held_out=true`; psx web moves a recording either way.
//! The agent's psx calls that name one are refused, and a held-out play's
//! answer reaches it without the play.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::{GOAL, Goal, SCENARIOS};
use crate::Error;

/// The recordings goal.toml holds out; none outside a port's workspace.
pub fn listed(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(GOAL))
        .ok()
        .and_then(|t| toml::from_str::<Goal>(&t).ok())
        .map(|g| g.held_out)
        .unwrap_or_default()
}

/// Hold the recording `rec` (`scenario/name`) out, or give it back to the
/// work (`on` false), in goal.toml. Returns the held-out list as it is now.
pub fn set(root: &Path, rec: &str, on: bool) -> crate::Result<Vec<String>> {
    let path = root.join(GOAL);
    let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    let goal: Goal = toml::from_str(&text).map_err(|e| Error::invalid(format!("{GOAL}: {e}")))?;
    let Some((scenario, name)) = rec.split_once('/') else {
        return Err(Error::invalid(format!("{rec}: a recording is scenario/name")));
    };
    if on && !root.join(SCENARIOS).join(scenario).join(format!("{name}.jsonl")).is_file() {
        return Err(Error::invalid(format!("{rec}: no such recording under {SCENARIOS}/")));
    }
    let mut held_out = goal.held_out;
    let had = held_out.iter().any(|r| r == rec);
    match (on, had) {
        (true, false) => held_out.push(rec.to_string()),
        (false, true) => held_out.retain(|r| r != rec),
        _ => return Ok(held_out),
    }
    let plays = toml::Value::from(held_out.clone());
    let text = super::migrate::set_key(&text, "held_out", (!held_out.is_empty()).then_some(&plays));
    std::fs::write(&path, text).map_err(|e| Error::io(&path, e))?;
    Ok(held_out)
}

/// Why the agent's call of `tool` with `args` is refused: it names a
/// held-out play (by scenario and name, as `scenario/name`, or by a path
/// to its files). None when it names none.
pub fn refused(root: &Path, tool: &str, args: &Map<String, Value>) -> Option<String> {
    let held_out = listed(root);
    if held_out.is_empty() {
        return None;
    }
    let mut strings = Vec::new();
    collect(&Value::Object(args.clone()), &mut strings);
    let id = args.get("id").and_then(Value::as_str);
    let name = args.get("name").and_then(Value::as_str);
    let hit = held_out.iter().find(|r| {
        let Some((s, n)) = r.split_once('/') else { return false };
        let file = format!("{SCENARIOS}/{s}/{n}.");
        (id == Some(s) && name == Some(n)) || strings.iter().any(|x| x == *r || x.replace('\\', "/").contains(&file))
    })?;
    Some(format!(
        "{tool}: {hit} is held out (goal.toml's held_out): port_check judges the port on it and port_status counts it, but it is never shown to the agent or worked on; work from the work recordings, and record the case again yourself if you need it"
    ))
}

fn collect(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// A play's answer as the agent reads it: a held-out play's without the
/// play (its input, how to record it again, its recording's numbers).
/// `held_out` is whether the request asked for one.
pub fn answer(root: &Path, report: Value, held_out: bool) -> Value {
    let rec = report["recording"]["recording"]
        .as_str()
        .and_then(|p| p.strip_prefix(&format!("{SCENARIOS}/")))
        .and_then(|p| p.strip_suffix(".jsonl"))
        .map(String::from);
    let listed = rec.as_ref().is_some_and(|r| listed(root).contains(r));
    if !held_out && !listed {
        return report;
    }
    let mut v = json!({"status": report["status"], "held_out": true,
        "recorded": report["recording"]["written"] == true,
        "note": "a held-out play: port_check judges the port on it and port_status counts it; the play itself is not shown"});
    for k in ["run_dir", "error", "note"] {
        if !report[k].is_null() {
            v[if k == "note" { "person_note" } else { k }] = report[k].clone();
        }
    }
    if let Some(r) = rec {
        v["recording"] = json!(r);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> crate::port::tests::tempdir::Dir {
        let d = crate::port::tests::tempdir::Dir::new();
        let root = d.0.as_path();
        std::fs::create_dir_all(root.join("scenarios/walk")).unwrap();
        for n in ["play", "short"] {
            std::fs::write(root.join(format!("scenarios/walk/{n}.jsonl")), "{}\n").unwrap();
        }
        std::fs::write(
            root.join(GOAL),
            super::super::GOAL_TOML.replace("description = \"\"", "description = \"walk\""),
        )
        .unwrap();
        d
    }

    #[test]
    fn a_recording_is_held_out_and_given_back_in_goal_toml() {
        let d = workspace();
        let root = d.0.as_path();
        assert_eq!(set(root, "walk/play", true).unwrap(), ["walk/play"]);
        assert_eq!(set(root, "walk/play", true).unwrap(), ["walk/play"]);
        let text = std::fs::read_to_string(root.join(GOAL)).unwrap();
        assert!(text.contains("description = \"walk\"\n") && text.ends_with("held_out = [\"walk/play\"]\n"), "{text}");
        assert_eq!(listed(root), ["walk/play"]);
        assert!(set(root, "walk/gone", true).is_err());
        assert!(set(root, "walk/play", false).unwrap().is_empty());
        assert!(!std::fs::read_to_string(root.join(GOAL)).unwrap().contains("\nheld_out = ["));
    }

    #[test]
    fn the_agents_calls_that_name_a_held_out_play_are_refused() {
        let d = workspace();
        let root = d.0.as_path();
        let args = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(refused(root, "scenario_show", &args(json!({"id": "walk", "name": "play"}))), None);
        set(root, "walk/play", true).unwrap();
        for (tool, a) in [
            ("scenario_show", json!({"id": "walk", "name": "play"})),
            ("run", json!({"recording": "walk/play", "frames": 10})),
            ("read", json!({"path": "scenarios/walk/play.jsonl"})),
            ("code_coverage_diff", json!({"a": ["scenarios/walk/play.coverage.json"], "b": []})),
        ] {
            let why = refused(root, tool, &args(a)).unwrap_or_default();
            assert!(why.starts_with(&format!("{tool}: walk/play is held out")), "{why}");
        }
        for a in [json!({"id": "walk", "name": "short"}), json!({"recording": "walk/short"}), json!({"id": "walk"})] {
            assert_eq!(refused(root, "scenario_show", &args(a)), None);
        }
    }

    #[test]
    fn a_held_out_plays_answer_leaves_the_play_out() {
        let d = workspace();
        let root = d.0.as_path();
        let report = json!({"status": "done", "note": "went left", "run_dir": "/r", "input": {"events": [1]},
            "reproduce": {"arguments": {}}, "recording": {"written": true, "recording": "scenarios/walk/play.jsonl", "ticks": 400}});
        assert_eq!(answer(root, report.clone(), false), report);
        let v = answer(root, report.clone(), true);
        assert_eq!(v["recording"], "walk/play");
        assert_eq!((v["recorded"].clone(), v["person_note"].clone()), (json!(true), json!("went left")));
        assert!(v.get("input").is_none() && v.get("reproduce").is_none() && !v.to_string().contains("400"), "{v}");
        // Held out later, from psx web: the same.
        set(root, "walk/play", true).unwrap();
        assert!(answer(root, report, false).get("input").is_none());
    }
}

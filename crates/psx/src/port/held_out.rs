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
    // A work recording that starts inside it shows the agent what it holds.
    // So does one that starts inside one of those, which runs it first.
    let mut inside: Vec<String> = Vec::new();
    let mut next = if on { vec![name.to_string()] } else { Vec::new() };
    while let Some(n) = next.pop() {
        for r in starting_in(root, scenario, &n) {
            if !inside.contains(&r) {
                next.push(r.split_once('/').map_or(r.clone(), |(_, n)| n.to_string()));
                inside.push(r);
            }
        }
    }
    inside.sort();
    let inside: Vec<String> = inside.into_iter().filter(|r| !held_out.contains(r)).collect();
    if !inside.is_empty() {
        return Err(Error::invalid(format!(
            "{rec}: {} start inside it (from), so holding it out would still show the agent part of it: delete them, or keep it a work recording",
            inside.join(", ")
        ))
        .with("starting_inside", json!(inside)));
    }
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

/// Commits the person's recording `rec` (`scenario/name`, its .jsonl and
/// .coverage.json) and goal.toml alone, so the agent meets them committed
/// rather than as changes in its way (port check wants a clean tree).
/// Into `v`: `committed`, the commit, or `commit_problem`, why git
/// refused; nothing outside a repository or with nothing to commit.
pub fn commit(root: &Path, rec: &str, message: &str, v: &mut Value) {
    let (jsonl, coverage) = (format!("{SCENARIOS}/{rec}.jsonl"), format!("{SCENARIOS}/{rec}.coverage.json"));
    match super::status::Git::commit_alone(root, &[&jsonl, &coverage, GOAL], message) {
        Ok(Some(c)) => v["committed"] = json!(c),
        Ok(None) => {}
        Err(e) => v["commit_problem"] = json!(e),
    }
}

/// The scenario's recordings that start inside `name` (their `from`), as
/// `scenario/name`.
fn starting_in(root: &Path, scenario: &str, name: &str) -> Vec<String> {
    use std::io::BufRead;
    let dir = root.join(SCENARIOS).join(scenario);
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter(|p| {
            let mut first = String::new();
            let read = std::fs::File::open(p).map(|f| std::io::BufReader::new(f).read_line(&mut first));
            let header = read.ok().and_then(|_| serde_json::from_str::<Value>(&first).ok());
            let from = header.as_ref().and_then(|h| h["producer"]["details"]["from"].as_str());
            from.and_then(|f| psx_scenario::scenario::Continues::parse(f).ok()).is_some_and(|c| c.recording == name)
        })
        .filter_map(|p| Some(format!("{scenario}/{}", p.file_stem()?.to_string_lossy())))
        .collect();
    out.sort();
    out
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
    fn the_persons_play_is_committed_alone() {
        let d = workspace();
        let root = d.0.as_path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        // Outside a repository: nothing.
        let mut v = json!({});
        commit(root, "walk/play", "play", &mut v);
        assert_eq!(v, json!({}));
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("src.rs"), "a").unwrap();
        git(&["add", "src.rs"]);
        git(&["commit", "-q", "-m", "start"]);
        // The agent's work in progress, staged and not, stays as it was.
        std::fs::write(root.join("src.rs"), "b").unwrap();
        std::fs::write(root.join("staged.rs"), "c").unwrap();
        git(&["add", "staged.rs"]);
        set(root, "walk/play", true).unwrap();
        commit(root, "walk/play", "Hold out walk/play", &mut v);
        assert!(v["committed"].is_string(), "{v}");
        let files = git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.lines().collect::<Vec<_>>(), ["goal.toml", "scenarios/walk/play.jsonl"]);
        let status = git(&["status", "--porcelain"]);
        assert!(status.contains(" M src.rs") && status.contains("A  staged.rs"), "{status}");
        assert!(!status.contains("play.jsonl") && !status.contains("goal.toml"), "{status}");
        // Again with nothing new: no commit.
        let mut again = json!({});
        commit(root, "walk/play", "again", &mut again);
        assert_eq!(again, json!({}));
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

    /// A work recording that starts inside a play keeps it from being held
    /// out: it would still show the agent part of the play.
    #[test]
    fn a_play_others_start_inside_is_not_held_out() {
        let d = workspace();
        let root = d.0.as_path();
        let header =
            |from: &str| format!("{{\"producer\": {{\"name\": \"psx\", \"details\": {{\"from\": \"{from}\"}}}}}}\n");
        std::fs::write(root.join("scenarios/walk/near.jsonl"), header("play@24")).unwrap();
        std::fs::write(root.join("scenarios/walk/after.jsonl"), header("short")).unwrap();
        std::fs::write(root.join("scenarios/walk/later.jsonl"), header("near")).unwrap();
        let e = set(root, "walk/play", true).unwrap_err();
        assert!(e.message.starts_with("walk/play: walk/later, walk/near start inside it"), "{}", e.message);
        std::fs::remove_file(root.join("scenarios/walk/later.jsonl")).unwrap();
        assert!(listed(root).is_empty());
        // Once it is gone, the play is held out.
        std::fs::remove_file(root.join("scenarios/walk/near.jsonl")).unwrap();
        assert_eq!(set(root, "walk/play", true).unwrap(), ["walk/play"]);
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

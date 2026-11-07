//! `port update` moving a workspace from features to the goal's recordings:
//! each `catalogue/F-*.md` entry's questions go to `catalogue/questions.md`,
//! its wont_reach to `catalogue/wont_reach.toml`, its assets to the
//! scenarios its recordings are of, and its held-out recordings the person
//! played to goal.toml's `held_out`; the agent's own become work. goal.toml
//! loses `features`, and the entries and `.psx/goal_recordings.json` go.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{CATALOGUE, Confirmed, GOAL, Goal, QUESTIONS, SCENARIOS, WONT_REACH};
use crate::Error;

/// The ledger of recordings features named, from before.
const GOAL_RECORDINGS: &str = ".psx/goal_recordings.json";

/// A feature's entry as it was: only what moves is read.
#[derive(Debug, Default, Deserialize)]
struct Entry {
    #[serde(default)]
    recordings: Recordings,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    wont_reach: BTreeMap<String, String>,
    #[serde(default)]
    questions: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Recordings {
    #[serde(default)]
    work: Vec<String>,
    #[serde(default)]
    held_out: Vec<String>,
}

/// `text` with the top-level `key` taken out (its value over several lines
/// too) and, given `value`, set to it before the first table.
fn set_key(text: &str, key: &str, value: Option<&toml::Value>) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let (mut depth, mut in_table) = (0i32, false);
    for line in text.lines() {
        if depth > 0 {
            depth += line.matches(['[', '{']).count() as i32 - line.matches([']', '}']).count() as i32;
            continue;
        }
        let t = line.trim_start();
        if t.starts_with('[') {
            in_table = true;
        }
        let k = t.split(['=', ' ', '\t']).next().unwrap_or("");
        if !in_table && k == key {
            let v = t.split_once('=').map(|x| x.1).unwrap_or("");
            depth = v.matches(['[', '{']).count() as i32 - v.matches([']', '}']).count() as i32;
            continue;
        }
        lines.push(line);
    }
    let line;
    if let Some(v) = value {
        line = format!("{key} = {v}");
        let at = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
        lines.insert(at, &line);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn read(root: &Path, rel: &str) -> crate::Result<Option<String>> {
    match std::fs::read_to_string(root.join(rel)) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(root.join(rel), e)),
    }
}

fn write(root: &Path, rel: &str, text: &str) -> crate::Result<()> {
    let p = root.join(rel);
    crate::write_atomic(&p, text.as_bytes()).map_err(|e| Error::io(&p, e))
}

/// Move the workspace at `root` off features; what moved, or null when
/// there was nothing to move. `dry_run` writes nothing.
pub fn run(root: &Path, dry_run: bool) -> crate::Result<Value> {
    let Some(goal_text) = read(root, GOAL)? else { return Ok(Value::Null) };
    let goal: Goal = toml::from_str(&goal_text).map_err(|e| Error::invalid(format!("{GOAL}: {}", e.message())))?;
    let dir = root.join(CATALOGUE);
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    files.retain(|n| n.starts_with("F-") && n.ends_with(".md"));
    files.sort();
    let ledger = root.join(GOAL_RECORDINGS).is_file();
    let confirmed_features = goal.confirmed.as_ref().is_some_and(|c| c.features.is_some());
    if files.is_empty() && goal.features.is_none() && !ledger && !confirmed_features {
        return Ok(Value::Null);
    }
    // The goal's entries move; one outside it (not listed) goes with
    // nothing moved: its questions are not the goal's.
    let listed: Option<Vec<String>> = match &goal.features {
        Some(toml::Value::Array(a)) => Some(a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
        _ => None,
    };
    let mut entries = Vec::new();
    let mut outside = Vec::new();
    for name in &files {
        let id = name.trim_end_matches(".md");
        if listed.as_ref().is_some_and(|l| !l.iter().any(|x| x == id)) {
            outside.push(format!("{CATALOGUE}/{name}"));
            continue;
        }
        let rel = format!("{CATALOGUE}/{name}");
        let text = read(root, &rel)?.unwrap_or_default();
        let (front, _) = super::split_front(&text).map_err(|why| Error::invalid(format!("{rel}: {why}")))?;
        let e: Entry = toml::from_str(&front).map_err(|e| Error::invalid(format!("{rel}: {}", e.message())))?;
        entries.push((rel, e));
    }
    // The person's plays: held out already, confirmed as theirs, what a
    // play request recorded, or held out in the ledger the goal's
    // features kept.
    let mut played: Vec<String> = crate::handoff::queue::list(root).iter().filter_map(|r| r.recording()).collect();
    if let Some(l) = std::fs::read(root.join(GOAL_RECORDINGS))
        .ok()
        .and_then(|b| serde_json::from_slice::<BTreeMap<String, String>>(&b).ok())
    {
        played.extend(l.into_iter().filter(|(_, k)| k == "held_out").map(|(r, _)| r));
    }
    let confirmed_plays: Vec<String> = goal.confirmed.as_ref().and_then(|c| c.held_out.clone()).unwrap_or_default();
    let mut held_out = goal.held_out.clone();
    let mut to_work = Vec::new();
    for r in entries.iter().flat_map(|(_, e)| &e.recordings.held_out) {
        if held_out.contains(r) {
            continue;
        }
        if played.contains(r) || confirmed_plays.contains(r) {
            held_out.push(r.clone());
        } else if !to_work.contains(r) {
            to_work.push(r.clone());
        }
    }
    let mut wont_reach: BTreeMap<String, String> = match read(root, WONT_REACH)? {
        Some(t) => toml::from_str(&t).map_err(|e| Error::invalid(format!("{WONT_REACH}: {}", e.message())))?,
        None => BTreeMap::new(),
    };
    let mut questions: Vec<String> = Vec::new();
    let mut assets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, e) in &entries {
        for (k, why) in &e.wont_reach {
            wont_reach.entry(k.clone()).or_insert_with(|| why.clone());
        }
        questions.extend(e.questions.iter().filter(|q| !q.trim().is_empty()).cloned());
        let all: Vec<&String> = e.recordings.work.iter().chain(&e.recordings.held_out).collect();
        for s in super::scenarios_of(all) {
            let names = assets.entry(s).or_default();
            for n in &e.assets {
                if !names.contains(n) {
                    names.push(n.clone());
                }
            }
        }
    }
    assets.retain(|_, n| !n.is_empty());
    let out = json!({
        "entries_removed": files,
        "outside_the_goal": outside,
        "questions": questions.len(),
        "wont_reach": wont_reach.len(),
        "scenario_assets": assets,
        "held_out": held_out,
        "held_out_to_work": to_work,
    });
    if dry_run {
        return Ok(out);
    }
    // goal.toml: no features, the person's plays held out and confirmed.
    let mut text = set_key(&goal_text, "features", None);
    // The opening comment, when it tells of features, becomes the template's.
    let head = text.lines().take_while(|l| l.trim_start().starts_with('#')).count();
    if text.lines().take(head).any(|l| l.contains("feature")) {
        let template: Vec<&str> = super::GOAL_TOML.lines().take_while(|l| l.starts_with('#')).collect();
        let rest: Vec<&str> = text.lines().skip(head).collect();
        text = format!("{}\n{}\n", template.join("\n"), rest.join("\n"));
    }
    let plays = toml::Value::from(held_out.clone());
    text = set_key(&text, "held_out", (!held_out.is_empty()).then_some(&plays));
    if let Some(c) = &goal.confirmed {
        let c = Confirmed {
            features: None,
            held_out: Some(
                held_out.iter().filter(|r| confirmed_plays.contains(r) || played.contains(r)).cloned().collect(),
            ),
            ..c.clone()
        };
        text = super::confirm::with_confirmation(&text, &c);
    }
    let check: Goal =
        toml::from_str(&text).map_err(|e| Error::invalid(format!("{GOAL} rewritten unreadably: {}", e.message())))?;
    if check.features.is_some() || check.held_out != held_out {
        return Err(Error::invalid(format!("{GOAL}: could not take features out; nothing written")));
    }
    for (s, names) in &assets {
        let rel = format!("{SCENARIOS}/{s}.toml");
        let Some(t) = read(root, &rel)? else { continue };
        let mut all: Vec<String> = toml::from_str::<toml::Table>(&t)
            .ok()
            .and_then(|v| v.get("assets").and_then(|a| a.as_array().cloned()))
            .unwrap_or_default()
            .iter()
            .filter_map(|n| n.as_str().map(String::from))
            .collect();
        all.extend(names.iter().filter(|n| !all.contains(n)).cloned().collect::<Vec<_>>());
        write(root, &rel, &set_key(&t, "assets", Some(&toml::Value::from(all))))?;
    }
    if !wont_reach.is_empty() {
        let mut t = String::from(
            "# Branches of the code to port no recording can reach, by address, range or\n# test as port next gives it, each with why; the person confirms them.\n",
        );
        t += &toml::to_string(&wont_reach).map_err(|e| Error::invalid(e.to_string()))?;
        write(root, WONT_REACH, &t)?;
    }
    if !questions.is_empty() {
        let mut t = read(root, QUESTIONS)?
            .unwrap_or_else(|| "# Open questions about the original, one per `- ` line.\n\n".into());
        if !t.ends_with('\n') {
            t.push('\n');
        }
        for q in &questions {
            t += &format!("- {}\n", q.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        write(root, QUESTIONS, &t)?;
    }
    write(root, GOAL, &text)?;
    // The READMEs init wrote, where they still tell of features.
    if read(root, "catalogue/README.md")?.is_some_and(|t| t.contains("F-0001.md")) {
        write(root, "catalogue/README.md", super::init::CATALOGUE_README)?;
    }
    let old = "A feature's entry names the assets its\nrecordings need";
    if let Some(t) = read(root, "formats/README.md")?.filter(|t| t.contains(old)) {
        write(root, "formats/README.md", &t.replace(old, "A scenario's toml names the assets its\nrecordings need"))?;
    }
    for rel in entries.iter().map(|(r, _)| r).chain(&outside) {
        let p = root.join(rel);
        std::fs::remove_file(&p).map_err(|e| Error::io(&p, e))?;
    }
    if ledger {
        let p = root.join(GOAL_RECORDINGS);
        std::fs::remove_file(&p).map_err(|e| Error::io(&p, e))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::Port;
    use super::super::tests::workspace;
    use super::*;

    #[test]
    fn a_workspace_with_features_moves_to_the_goals_recordings() {
        let entry = "+++\nid = \"F-0001\"\ntitle = \"Walk\"\narea = \"a\"\ncode = [\"0x80010000\"]\nassets = [\"/CROC.WAD\"]\nquestions = [\"what is the slope limit?\"]\n[recordings]\nwork = [\"walk/a\"]\nheld_out = [\"walk/b\", \"walk/c\", \"walk/d\"]\n[wont_reach]\n\"0x80010010\" = \"debug only\"\n+++\nWalking.\n";
        let goal = "# The goal, as features.\ndescription = \"walk\"\nfeatures = [\n  \"F-0001\",\n]\nconfirmed = { at = \"t\", features = [\"F-0001\"], held_out = [\"walk/c\"], wont_reach = { \"F-0001\" = { \"0x80010010\" = \"debug only\" } } }\n";
        let (d, _) = workspace(
            goal,
            &[
                ("catalogue/F-0001.md", entry),
                ("catalogue/README.md", "One file per feature: `F-0001.md`, ...\n"),
                ("scenarios/walk.toml", "kind = \"walk\"\n\n[params]\nx = 1\n"),
                ("scenarios/walk/a.jsonl", ""),
                ("scenarios/walk/b.jsonl", ""),
                ("scenarios/walk/c.jsonl", ""),
                ("catalogue/F-0002.md", "+++\nid = \"F-0002\"\nquestions = [\"not the goal's\"]\n+++\n"),
                (".psx/goal_recordings.json", "{\"walk/d\": \"held_out\"}"),
                ("scenarios/walk/d.jsonl", ""),
            ],
        );
        let out = run(&d.0, false).unwrap();
        assert_eq!(out["held_out"], json!(["walk/c", "walk/d"]));
        assert_eq!(out["outside_the_goal"], json!(["catalogue/F-0002.md"]));
        assert_eq!(out["held_out_to_work"], json!(["walk/b"]));
        let port = Port::load(&d.0).unwrap();
        // Only the inventory, which this workspace has none of, is missing.
        assert!(port.problems.iter().all(|p| p.problem.contains("no inventory")), "{:?}", port.problems);
        assert_eq!(port.recordings.work, ["walk/a", "walk/b"]);
        assert_eq!(port.recordings.held_out, ["walk/c", "walk/d"]);
        assert_eq!(port.questions, ["what is the slope limit?"]);
        assert_eq!(port.wont_reach["0x80010010"], "debug only");
        assert!(port.wont_reach_agreed("0x80010010", "debug only"));
        assert_eq!(port.scenario_assets["walk"], ["/CROC.WAD"]);
        assert!(matches!(port.confirmation(), super::super::Confirmation::Covers { .. }));
        let text = std::fs::read_to_string(d.0.join(GOAL)).unwrap();
        assert!(
            text.starts_with(super::super::GOAL_TOML.lines().next().unwrap()) && !text.contains("feature"),
            "{text}"
        );
        assert!(!std::fs::read_to_string(d.0.join("catalogue/README.md")).unwrap().contains("F-0001.md"));
        assert!(!d.0.join("catalogue/F-0001.md").exists() && !d.0.join(GOAL_RECORDINGS).exists());
        // Moved once, nothing more moves.
        assert_eq!(run(&d.0, false).unwrap(), Value::Null);
    }

    #[test]
    fn a_key_over_several_lines_is_taken_out_whole() {
        let t = "a = 1\nfeatures = [\n  \"x\",\n]\nb = 2\n[t]\nfeatures = 3\n";
        assert_eq!(set_key(t, "features", None), "a = 1\nb = 2\n[t]\nfeatures = 3\n");
        let v = toml::Value::from(vec!["y".to_string()]);
        assert_eq!(set_key("a = 1\n[t]\n", "held_out", Some(&v)), "a = 1\nheld_out = [\"y\"]\n[t]\n");
    }
}

//! `port update` moving a workspace to the current form. From features:
//! each `catalogue/F-*.md` entry's assets go to the scenarios its
//! recordings are of, and its held-out recordings the person played to
//! goal.toml's `held_out`; the agent's own become work. The script stretches
//! its code cited that the port's code does not go to
//! `game/script_stretches.md`, so they stay cited. From when psx decided
//! when a goal was done: the entries' and `catalogue/questions.md`'s
//! questions, and the entries' and `catalogue/wont_reach.toml`'s branches no
//! recording reaches, go to STATUS.md. goal.toml loses `features` and
//! `confirmed`; the entries, those two files, `.psx/goal_recordings.json`
//! and `port next`'s `.psx/port/next.json` go.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{CATALOGUE, GOAL, Goal, QUESTIONS, SCENARIOS, STATUS, WONT_REACH};
use crate::Error;
use crate::script_cover::Stretch;

/// Where the script stretches the entries cited go.
const STRETCHES: &str = "game/script_stretches.md";

/// The ledger of recordings features named, from before.
const GOAL_RECORDINGS: &str = ".psx/goal_recordings.json";

/// `port next`'s loop guard, from before.
const GUARD: &str = ".psx/port/next.json";

/// STATUS.md as `port init` writes it.
const STATUS_MD: &str = include_str!("../../../../templates/base/STATUS.md");

/// A feature's entry as it was: only what moves is read.
#[derive(Debug, Default, Deserialize)]
struct Entry {
    #[serde(default)]
    code: Vec<String>,
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
pub(super) fn set_key(text: &str, key: &str, value: Option<&toml::Value>) -> String {
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

/// Move the workspace at `root` to the current form; what moved, or null
/// when there was nothing to move. `dry_run` writes nothing.
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
    let old: Vec<&str> = [WONT_REACH, QUESTIONS, GUARD].into_iter().filter(|f| root.join(f).is_file()).collect();
    if files.is_empty() && goal.features.is_none() && goal.confirmed.is_none() && !ledger && old.is_empty() {
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
    let confirmed_plays: Vec<String> = goal
        .confirmed
        .as_ref()
        .and_then(|c| c.get("held_out"))
        .and_then(|h| h.as_array())
        .map(|a| a.iter().filter_map(|r| r.as_str().map(String::from)).collect())
        .unwrap_or_default();
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
    let mut questions: Vec<String> = read(root, QUESTIONS)?
        .map(|t| t.lines().filter_map(|l| l.trim().strip_prefix("- ").map(|q| q.trim().to_string())).collect())
        .unwrap_or_default();
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
    // Script stretches the entries cited and the port's code does not:
    // only the entries said the port follows them.
    let cited = super::Cited::read(root);
    let mut stretches: Vec<String> = Vec::new();
    for c in entries.iter().flat_map(|(_, e)| &e.code) {
        let Some(s) = Stretch::parse(c) else { continue };
        if !cited.stretches.iter().any(|x| x.contains(&s)) && !stretches.contains(c) {
            stretches.push(c.clone());
        }
    }
    let out = json!({
        "entries_removed": files,
        "removed": old,
        "outside_the_goal": outside,
        "questions": questions.len(),
        "wont_reach": wont_reach.len(),
        "scenario_assets": assets,
        "held_out": held_out,
        "held_out_to_work": to_work,
        "script_stretches": stretches,
    });
    if dry_run {
        return Ok(out);
    }
    // goal.toml: no features or confirmation, the person's plays held out.
    let mut text = set_key(&set_key(&goal_text, "features", None), "confirmed", None);
    // The opening comment, when it tells of features or the confirmation,
    // becomes the template's.
    let head = text.lines().take_while(|l| l.trim_start().starts_with('#')).count();
    if text.lines().take(head).any(|l| l.contains("feature") || l.contains("confirm")) {
        let template: Vec<&str> = super::GOAL_TOML.lines().take_while(|l| l.starts_with('#')).collect();
        let rest: Vec<&str> = text.lines().skip(head).collect();
        text = format!("{}\n{}\n", template.join("\n"), rest.join("\n"));
    }
    let plays = toml::Value::from(held_out.clone());
    text = set_key(&text, "held_out", (!held_out.is_empty()).then_some(&plays));
    let check: Goal =
        toml::from_str(&text).map_err(|e| Error::invalid(format!("{GOAL} rewritten unreadably: {}", e.message())))?;
    if check.features.is_some() || check.confirmed.is_some() || check.held_out != held_out {
        return Err(Error::invalid(format!("{GOAL}: could not take features and confirmed out; nothing written")));
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
    if !wont_reach.is_empty() || !questions.is_empty() {
        let mut t = read(root, STATUS)?.unwrap_or_else(|| STATUS_MD.into());
        if !t.ends_with('\n') {
            t.push('\n');
        }
        let line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        if !questions.is_empty() {
            t += "\n## Open questions\n\nMoved here from catalogue/questions.md by psx port update.\n\n";
            for q in &questions {
                t += &format!("- {}\n", line(q));
            }
        }
        if !wont_reach.is_empty() {
            t += "\n## Branches no recording reaches\n\nMoved here from catalogue/wont_reach.toml by psx port update.\n\n";
            for (k, why) in &wont_reach {
                t += &format!("- {k}: {}\n", line(why));
            }
        }
        write(root, STATUS, &t)?;
    }
    if !stretches.is_empty() {
        let mut t = String::from(
            "# Script stretches the port follows\n\nThe catalogue's feature entries cited these; psx port update kept them here\nso they stay cited. Cite each where the port's code follows it and take it\nout of this list.\n\n",
        );
        for s in &stretches {
            t += &format!("- {s}\n");
        }
        let dir = root.join("game");
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        write(root, STRETCHES, &t)?;
    }
    write(root, GOAL, &text)?;
    // The READMEs init wrote, where they still tell of features.
    if read(root, "catalogue/README.md")?.is_some_and(|t| t.contains("F-0001.md")) {
        write(root, "catalogue/README.md", super::init::CATALOGUE_README)?;
    }
    let readme = "A feature's entry names the assets its\nrecordings need";
    if let Some(t) = read(root, "formats/README.md")?.filter(|t| t.contains(readme)) {
        write(
            root,
            "formats/README.md",
            &t.replace(readme, "A scenario's toml names the assets its\nrecordings need"),
        )?;
    }
    for rel in entries.iter().map(|(r, _)| r).chain(&outside) {
        let p = root.join(rel);
        std::fs::remove_file(&p).map_err(|e| Error::io(&p, e))?;
    }
    for rel in old.iter().copied().chain(ledger.then_some(GOAL_RECORDINGS)) {
        let p = root.join(rel);
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
        let entry = "+++\nid = \"F-0001\"\ntitle = \"Walk\"\narea = \"a\"\ncode = [\"0x80010000\", \"croc+0x10..0x20\"]\nassets = [\"/CROC.WAD\"]\nquestions = [\"what is the slope limit?\"]\n[recordings]\nwork = [\"walk/a\"]\nheld_out = [\"walk/b\", \"walk/c\", \"walk/d\"]\n[wont_reach]\n\"0x80010010\" = \"debug only\"\n\"croc+0x18\" = \"never\"\n+++\nWalking.\n";
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
        let status = std::fs::read_to_string(d.0.join(STATUS)).unwrap();
        assert!(status.starts_with("# Status") && status.contains("- what is the slope limit?\n"), "{status}");
        assert!(status.contains("- 0x80010010: debug only\n") && status.contains("- croc+0x18: never\n"), "{status}");
        assert_eq!(out["script_stretches"], json!(["croc+0x10..0x20"]));
        assert!(std::fs::read_to_string(d.0.join(STRETCHES)).unwrap().contains("- croc+0x10..0x20\n"));
        assert_eq!(port.scenario_assets["walk"], ["/CROC.WAD"]);
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
    fn a_workspace_from_port_next_moves_its_questions_and_branches_to_status() {
        let goal = "# The person confirms it with `psx port confirm --yes`.\ndescription = \"walk\"\nheld_out = [\"walk/p\"]\nconfirmed = { at = \"t\", held_out = [\"walk/p\"] }\n";
        let (d, _) = workspace(
            goal,
            &[
                (WONT_REACH, "\"0x80010010\" = \"debug only\"\n"),
                (QUESTIONS, "# Open questions\n\n- what resets the timer?\n"),
                (STATUS, "# Status\n\n## Next, and why\n\nThe jump.\n"),
                (GUARD, "{}"),
                ("scenarios/walk/p.jsonl", ""),
            ],
        );
        let out = run(&d.0, false).unwrap();
        assert_eq!(out["removed"], json!([WONT_REACH, QUESTIONS, GUARD]));
        let port = Port::load(&d.0).unwrap();
        assert!(port.problems.is_empty(), "{:?}", port.problems);
        assert_eq!(port.recordings.held_out, ["walk/p"]);
        let status = std::fs::read_to_string(d.0.join(STATUS)).unwrap();
        assert!(status.starts_with("# Status\n\n## Next, and why\n\nThe jump.\n"), "{status}");
        assert!(status.contains("## Open questions") && status.contains("- what resets the timer?\n"), "{status}");
        assert!(status.contains("- 0x80010010: debug only\n"), "{status}");
        assert_eq!(
            std::fs::read_to_string(d.0.join(GOAL)).unwrap(),
            "# What the person wants from this port, in their words.\ndescription = \"walk\"\nheld_out = [\"walk/p\"]\n"
        );
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

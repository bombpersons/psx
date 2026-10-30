//! What a port's goal needs next (design section 9): the facts, read once
//! per call; the stages they give each feature in the goal's closure; and
//! the items, each due on a fact and done on a fact the work leaves behind.
//! `port status` shows the facts, `port next` gives the one item due.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::check;
use super::verdict::{self, Kind, State};
use super::{Confirmation, Port, Stage};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// The workspace's commit.
#[derive(Debug, Clone, Default)]
pub struct Git {
    /// None before the first commit, or outside a repository.
    pub head: Option<String>,
    /// No uncommitted changes.
    pub clean: bool,
    /// A hash of the uncommitted changes, so the loop guard sees work in
    /// progress.
    pub changes: String,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).current_dir(root).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Git {
    pub fn read(root: &Path) -> Git {
        let head = git(root, &["rev-parse", "--verify", "-q", "HEAD"]).map(|s| s.trim().to_string());
        let status = git(root, &["status", "--porcelain"]).unwrap_or_default();
        let diff = git(root, &["diff", "HEAD"]).unwrap_or_default();
        let changes = format!("{:x}", Sha256::digest(format!("{status}\n{diff}")));
        Git { head, clean: status.trim().is_empty(), changes }
    }
}

/// One feature of the closure, as the facts give it.
#[derive(Debug, Clone)]
pub struct Feature {
    pub id: String,
    pub in_goal: bool,
    pub catalogued: bool,
    /// Stage checks that come with later steps of the work (mechanic
    /// capture, plugins and scenario launchers, the structure review) read
    /// false until psx can make them.
    pub captured: bool,
    /// What stops its mechanics from counting as captured.
    pub capture: Vec<String>,
    /// What stops its scenarios from counting: none listed, a problem, no
    /// passing determinism check.
    pub scenarios: Vec<String>,
    pub implemented: bool,
    /// The launchers and quantity getters its plugin has not registered, by
    /// the last check at HEAD.
    pub unimplemented: Vec<String>,
    pub reviewed: bool,
    pub verdicts: BTreeMap<Kind, State>,
}

impl Feature {
    fn passes(&self, k: Kind) -> bool {
        self.verdicts.get(&k) == Some(&State::Pass)
    }

    /// The last stage whose check holds, with every one before it; None
    /// when the feature has no catalogue entry.
    pub fn stage(&self) -> Option<Stage> {
        if !self.catalogued {
            return None;
        }
        let checks = [
            self.captured,
            self.implemented,
            self.passes(Kind::Replay) && self.passes(Kind::Scenario),
            self.reviewed,
            self.passes(Kind::Sandbox),
        ];
        let held = checks.iter().take_while(|c| **c).count();
        Some(Stage::ALL[held])
    }

    fn failing(&self) -> Vec<(Kind, &Vec<String>)> {
        self.verdicts
            .iter()
            .filter_map(|(k, s)| match s {
                State::Fail { failures } => Some((*k, failures)),
                _ => None,
            })
            .collect()
    }
}

/// Up to three lines, and how many more there are.
fn first_lines(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().take(3).cloned().collect();
    if lines.len() > 3 {
        out.push(format!("and {} more", lines.len() - 3));
    }
    out
}

/// Everything the items are decided on.
pub struct Facts {
    pub port: Port,
    pub git: Git,
    /// The closure in dependency order.
    pub features: Vec<Feature>,
    /// The f32 drift of each exact mechanic, from the last `port check`.
    pub drift: Value,
}

impl Facts {
    pub fn gather(root: &Path) -> crate::Result<Facts> {
        let port = Port::load(root)?;
        let git = Git::read(root);
        let goal = port.goal_ids();
        let features = port
            .closure()
            .into_iter()
            .map(|id| {
                let verdicts =
                    Kind::ALL.into_iter().map(|k| (k, verdict::state(root, &id, k, git.head.as_deref()))).collect();
                let e = port.entries.get(&id);
                let capture = e.map(|e| check::feature_uncaptured(root, e)).unwrap_or_default();
                let scenarios = e.map(|e| crate::scenario::feature_unready(root, e)).unwrap_or_default();
                let unimplemented = e.map(|e| check::unimplemented(root, e, git.head.as_deref())).unwrap_or_default();
                Feature {
                    in_goal: goal.contains(&id),
                    catalogued: e.is_some(),
                    captured: e.is_some() && capture.is_empty() && scenarios.is_empty(),
                    capture,
                    scenarios,
                    implemented: e.is_some() && unimplemented.is_empty(),
                    unimplemented,
                    reviewed: false,
                    verdicts,
                    id,
                }
            })
            .collect();
        let drift = check::drift(root, git.head.as_deref());
        Ok(Facts { port, git, features, drift })
    }

    pub fn stage(&self, id: &str) -> Option<Stage> {
        self.features.iter().find(|f| f.id == id).and_then(Feature::stage)
    }

    /// Every feature of the closure extracted. (The gap list joins this
    /// when attribution exists.)
    pub fn complete(&self) -> bool {
        self.port.confirmation() != Confirmation::Missing
            && !self.features.is_empty()
            && self.features.iter().all(|f| f.stage() == Some(Stage::Extracted))
    }

    /// The item due now.
    pub fn next(&self) -> Item {
        let confirmed = matches!(self.port.confirmation(), Confirmation::Covers { .. });
        // Until the person confirms the goal, the agent only investigates
        // and catalogues; the person is asked once the list has no problems.
        if !self.port.problems.is_empty() {
            let n = self.port.problems.len();
            return Item::agent(
                "Catalogue",
                None,
                format!("the goal or catalogue has {n} problems: {}", problems_line(&self.port)),
                "the goal and catalogue have no problems",
                "fix each problem (psx port status lists them); catalogue features the goal needs as catalogue/F-NNNN.md (catalogue/README.md)",
            );
        }
        if !confirmed {
            let why = match self.port.confirmation() {
                Confirmation::Shrunk { missing, .. } => {
                    format!(
                        "features the person confirmed are no longer in the goal or needed by it: {}",
                        missing.join(", ")
                    )
                }
                _ => "the person has not confirmed the goal's list".into(),
            };
            return Item {
                item: "Confirm",
                who: "person",
                feature: None,
                why,
                done_when: "goal.toml's confirmation covers its list".into(),
                how: "the person reads psx port confirm and runs psx port confirm --yes; the agent waits, investigating and cataloguing only".into(),
            };
        }
        let order = |pred: &dyn Fn(&Feature) -> bool| self.features.iter().find(|f| pred(f));
        if let Some(f) = order(&|f| !f.failing().is_empty()) {
            let lines: Vec<String> = f
                .failing()
                .iter()
                .flat_map(|(k, fs)| fs.iter().take(3).map(move |l| format!("{}: {l}", k.name())))
                .collect();
            return Item::agent(
                "Fix",
                Some(&f.id),
                format!("a verdict at HEAD fails: {}", lines.join("; ")),
                "every verdict of the feature at HEAD passes",
                "fix the port's code, commit, and run psx port check on the feature again",
            );
        }
        let stage = |f: &Feature, s: &[Stage]| f.stage().is_some_and(|x| s.contains(&x));
        if let Some(f) = order(&|f| stage(f, &[Stage::Catalogued]) && !f.capture.is_empty()) {
            let lines = first_lines(&f.capture);
            return Item::agent(
                "Capture",
                Some(&f.id),
                format!("its mechanics are not captured: {}", lines.join("; ")),
                "it names its mechanics, each checks clean and has a captured trace with a clear read audit, and NOTES.md names every branch no trace took both ways",
                "name the steps the original runs for it under the catalogue entry's mechanics, write mechanics/<id>/ for each (mechanic.toml, binding.toml), run psx mechanic capture, bind or explain each read the audit lists, and list in NOTES.md the PC of each branch no trace took both ways (docs/mechanics.md)",
            );
        }
        if let Some(f) = order(&|f| stage(f, &[Stage::Catalogued])) {
            return Item::agent(
                "Scenario",
                Some(&f.id),
                format!("its scenarios are not ready: {}", first_lines(&f.scenarios).join("; ")),
                "it lists a scenario, and each it lists has a captured recording and passes psx scenario check",
                "bind glossary.toml's tick (the logic step) and quantities, write scenarios/<id>.toml from a psx states chain whose last run ends at a game moment (until), list it in the catalogue entry, run psx scenario record, then psx scenario check (docs/scenarios.md)",
            );
        }
        if let Some(f) = order(&|f| stage(f, &[Stage::Captured, Stage::Implemented])) {
            let mut states: Vec<String> = [Kind::Replay, Kind::Scenario]
                .iter()
                .map(|k| format!("{} {}", k.name(), f.verdicts.get(k).map(State::name).unwrap_or("missing")))
                .collect();
            if !f.unimplemented.is_empty() {
                states.push(first_lines(&f.unimplemented).join("; "));
            }
            return Item::agent(
                "Implement",
                Some(&f.id),
                format!("captured, not verified ({})", states.join(", ")),
                "its replay and scenario verdicts pass at HEAD",
                "write the feature's plugin from its mechanics, register its scenario launchers and quantities (psx_bevy::scenario), commit, and run psx port check on it",
            );
        }
        if let Some(f) = order(&|f| stage(f, &[Stage::Reviewed])) {
            let s = f.verdicts.get(&Kind::Sandbox).map(State::name).unwrap_or("missing");
            return Item::agent(
                "Extract",
                Some(&f.id),
                format!("reviewed, sandbox verdict {s}"),
                "its sandbox verdict passes at HEAD",
                "build examples/sandbox_<feature>.rs from hand-made assets only, commit, and run psx port check on it",
            );
        }
        if self.complete() {
            return Item {
                item: "Done",
                who: "person",
                feature: None,
                why: "every feature of the goal is extracted".into(),
                done_when: String::new(),
                how: "the goal is complete; the person may set a new one".into(),
            };
        }
        // What is left needs items that later steps of psx's own work add.
        let waiting: Vec<String> = self
            .features
            .iter()
            .filter_map(|f| {
                let (stage, item) = match f.stage()? {
                    Stage::Verified => ("verified", "Review"),
                    _ => return None,
                };
                Some(format!("{} is {stage}, needing {item}", f.id))
            })
            .collect();
        Item {
            item: "Unavailable",
            who: "person",
            feature: None,
            why: waiting.join("; "),
            done_when: String::new(),
            how: "psx cannot hand out these items yet: the commands they need are still being built".into(),
        }
    }

    /// `port status`.
    pub fn status(&self) -> Value {
        let mut areas: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &self.features {
            let e = self.port.entries.get(&f.id);
            let stage = f.stage().map(Stage::name).unwrap_or("not catalogued");
            *counts.entry(stage).or_default() += 1;
            let verdicts: BTreeMap<&str, &str> = f.verdicts.iter().map(|(k, s)| (k.name(), s.name())).collect();
            areas.entry(e.map(|e| e.area.clone()).unwrap_or_default()).or_default().push(json!({
                "id": f.id, "title": e.map(|e| e.title.as_str()), "stage": stage, "in_goal": f.in_goal,
                "depends": e.map(|e| &e.depends), "verdicts": verdicts,
                "mechanics": e.map(|e| &e.mechanics), "capture": f.capture,
                "scenarios": e.map(|e| &e.scenarios), "scenario": f.scenarios, "unimplemented": f.unimplemented,
            }));
        }
        json!({
            "description": self.port.goal.description,
            "features": self.port.goal.features.to_json(),
            "confirmation": self.port.confirmation_json(),
            "problems": self.port.problems,
            "stages": counts,
            "areas": areas,
            "complete": self.complete(),
            "git": {"head": self.git.head, "clean": self.git.clean},
            "drift": self.drift,
            "not_measured_yet": ["code accounted for", "gap list"],
        })
    }

    /// `port status --plain`: for the person.
    pub fn plain(&self) -> String {
        let mut s = String::new();
        let goal = &self.port.goal.description;
        s.push_str(&format!("Goal: {}\n", if goal.is_empty() { "(no description)" } else { goal }));
        s.push_str(&match self.port.confirmation() {
            Confirmation::Missing => "Not confirmed yet.\n".to_string(),
            Confirmation::Covers { at } => format!("Confirmed {at}.\n"),
            Confirmation::Shrunk { at, missing } => {
                format!("Confirmed {at}, but {} is no longer in the goal or needed by it.\n", missing.join(", "))
            }
        });
        let mut area = None;
        for f in &self.features {
            let e = self.port.entries.get(&f.id);
            let a = e.map(|e| e.area.as_str()).unwrap_or("");
            if area != Some(a) {
                s.push_str(&format!("\n{}\n", if a.is_empty() { "(no area)" } else { a }));
                area = Some(a);
            }
            let title = e.map(|e| e.title.as_str()).unwrap_or("(not catalogued)");
            let stage = f.stage().map(Stage::name).unwrap_or("-");
            let mark = if f.in_goal { "" } else { "  (needed by the goal)" };
            s.push_str(&format!("  {}  {stage:<12} {title}{mark}\n", f.id));
        }
        if !self.port.problems.is_empty() {
            s.push_str(&format!("\n{} problems in the goal or catalogue.\n", self.port.problems.len()));
        }
        let item = self.next();
        s.push_str(&format!("\nNext: {}", item.item));
        if let Some(f) = &item.feature {
            s.push_str(&format!(" {f}"));
        }
        s.push_str(&format!(" ({}): {}\n", item.who, item.why));
        s
    }

    /// What the loop guard compares: the item's facts and the work in the
    /// workspace.
    fn fingerprint(&self, item: &Item) -> String {
        let text = format!(
            "{}\n{}\n{:?}\n{}\n{}",
            serde_json::to_string(&item.to_json()).unwrap_or_default(),
            serde_json::to_string(&self.status()).unwrap_or_default(),
            self.git.head,
            self.git.changes,
            self.port.problems.len(),
        );
        format!("{:x}", Sha256::digest(text))
    }
}

fn problems_line(port: &Port) -> String {
    let mut l: Vec<String> = port.problems.iter().take(3).map(|p| format!("{}: {}", p.file, p.problem)).collect();
    if port.problems.len() > 3 {
        l.push(format!("and {} more", port.problems.len() - 3));
    }
    l.join("; ")
}

/// One thing to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub item: &'static str,
    /// agent or person.
    pub who: &'static str,
    pub feature: Option<String>,
    /// The fact that makes it due.
    pub why: String,
    /// The fact that will show it done.
    pub done_when: String,
    pub how: String,
}

impl Item {
    fn agent(item: &'static str, feature: Option<&str>, why: String, done_when: &str, how: &str) -> Item {
        Item {
            item,
            who: "agent",
            feature: feature.map(str::to_string),
            why,
            done_when: done_when.into(),
            how: how.into(),
        }
    }

    fn to_json(&self) -> Value {
        json!({"item": self.item, "who": self.who, "feature": self.feature, "why": self.why,
               "done_when": self.done_when, "how": self.how})
    }
}

/// How many calls in a row may hand out the same agent item with nothing
/// changed before the loop guard reports it instead.
const GUARD: u32 = 3;

/// `.psx/port/next.json`: the last item handed out, its fingerprint and
/// how many calls in a row gave it unchanged.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Guard {
    item: String,
    fingerprint: String,
    count: u32,
}

fn guarded(root: &Path, facts: &Facts, item: Item) -> crate::Result<Value> {
    let path = root.join(".psx/port/next.json");
    let key = format!("{} {}", item.item, item.feature.as_deref().unwrap_or(""));
    let fingerprint = facts.fingerprint(&item);
    let old: Guard = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let count = if old.item == key && old.fingerprint == fingerprint { old.count + 1 } else { 1 };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    let guard = Guard { item: key, fingerprint, count };
    crate::write_atomic(&path, &serde_json::to_vec_pretty(&guard).unwrap_or_default())
        .map_err(|e| Error::io(&path, e))?;
    let mut out = item.to_json();
    if item.who == "agent" && count >= GUARD {
        out = json!({
            "item": "Stuck", "who": "person", "stuck": out, "times": count,
            "why": format!("port next gave this item {count} times with nothing changed: stop and ask the person what is wrong"),
        });
    }
    Ok(out)
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortStatusArgs {
    /// Text for the person instead of JSON.
    #[arg(long)]
    #[serde(default)]
    pub plain: bool,
}

impl Op for PortStatusArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(Facts::gather(&ctx.root).map(|f| {
            let mut v = f.status();
            v["next"] = f.next().to_json();
            if self.plain { json!({"text": f.plain()}) } else { v }
        }))
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortNextArgs {}

impl Op for PortNextArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(Facts::gather(&ctx.root).and_then(|f| guarded(&ctx.root, &f, f.next())))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{feature, workspace};
    use super::*;

    fn facts(goal: &str, ids: &[&str]) -> (super::super::tests::tempdir::Dir, Facts) {
        let entries: Vec<(String, String)> = ids.iter().map(|id| (format!("{id}.md"), feature(id, ""))).collect();
        let entries: Vec<(&str, &str)> = entries.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (dir, _) = workspace(goal, &entries);
        let f = Facts::gather(&dir.0).unwrap();
        (dir, f)
    }

    const CONFIRMED: &str = "description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\nconfirmed = { at = \"t\", features = [\"F-0001\", \"F-0002\"] }\n";

    #[test]
    fn the_catalogue_comes_before_the_person_confirms() {
        let (_d, f) = facts("description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\n", &["F-0001"]);
        assert_eq!(f.next().item, "Catalogue");
        let (_d, f) = facts("description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\n", &["F-0001", "F-0002"]);
        let item = f.next();
        assert_eq!((item.item, item.who), ("Confirm", "person"));
        let (_d, f) = facts(
            &CONFIRMED.replace("features = [\"F-0001\", \"F-0002\"]\nc", "features = [\"F-0002\"]\nc"),
            &["F-0001", "F-0002"],
        );
        assert_eq!(f.next().item, "Confirm");
        assert!(f.next().why.contains("F-0001"));
    }

    #[test]
    fn a_confirmed_catalogue_asks_for_capture() {
        let (d, _) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        let walk = feature("F-0001", "").replace("depends: []", "depends: []\nmechanics: [demo.walk]");
        std::fs::write(d.0.join("catalogue/F-0001.md"), walk).unwrap();
        let f = Facts::gather(&d.0).unwrap();
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Capture", Some("F-0001")));
        assert_eq!(
            item.why,
            "its mechanics are not captured: mechanics/demo.walk: no mechanic here (a Rust crate with src/lib.rs, or mechanic.toml)"
        );
        assert_eq!(f.stage("F-0001"), Some(Stage::Catalogued));
        // A feature that names no mechanics is asked for them.
        std::fs::write(d.0.join("catalogue/F-0001.md"), feature("F-0001", "")).unwrap();
        let f = Facts::gather(&d.0).unwrap();
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Capture", Some("F-0001")));
        assert_eq!(
            item.why,
            "its mechanics are not captured: catalogue/F-0001.md: names no mechanics (find the steps the original runs for it and list their ids under mechanics)"
        );
        assert_eq!(f.stage("F-0002"), Some(Stage::Catalogued));
    }

    fn with(f: &mut Facts, id: &str, stage: Stage, verdicts: &[(Kind, State)]) {
        let x = f.features.iter_mut().find(|x| x.id == id).unwrap();
        x.captured = stage >= Stage::Captured;
        x.implemented = stage >= Stage::Implemented;
        x.reviewed = stage >= Stage::Reviewed;
        x.verdicts = verdicts.iter().cloned().collect();
    }

    #[test]
    fn items_follow_the_stages() {
        let (_d, mut f) = facts(CONFIRMED, &["F-0001", "F-0002"]);
        with(&mut f, "F-0001", Stage::Captured, &[]);
        with(&mut f, "F-0002", Stage::Captured, &[]);
        assert_eq!((f.next().item, f.next().feature.as_deref()), ("Implement", Some("F-0001")));
        let pass = [(Kind::Replay, State::Pass), (Kind::Scenario, State::Pass)];
        with(&mut f, "F-0001", Stage::Implemented, &pass);
        assert_eq!(f.stage("F-0001"), Some(Stage::Verified));
        with(
            &mut f,
            "F-0002",
            Stage::Implemented,
            &[(Kind::Replay, State::Fail { failures: vec!["jump step 4: vy".into()] })],
        );
        let item = f.next();
        assert_eq!((item.item, item.feature.as_deref()), ("Fix", Some("F-0002")));
        assert_eq!(item.why, "a verdict at HEAD fails: replay: jump step 4: vy");
        with(&mut f, "F-0002", Stage::Implemented, &[(Kind::Replay, State::Stale { commit: "a".into() })]);
        assert_eq!(f.next().item, "Implement");
        let mut all = pass.to_vec();
        with(&mut f, "F-0002", Stage::Reviewed, &all);
        assert_eq!(f.stage("F-0002"), Some(Stage::Reviewed));
        assert_eq!(f.next().item, "Extract");
        all.push((Kind::Sandbox, State::Pass));
        with(&mut f, "F-0002", Stage::Reviewed, &all);
        with(&mut f, "F-0001", Stage::Reviewed, &all);
        assert!(f.complete());
        assert_eq!(f.next().item, "Done");
    }

    #[test]
    fn verdicts_count_only_at_their_clean_commit() {
        let (d, _) = facts(CONFIRMED, &["F-0001"]);
        let v = |commit: &str, clean, passed| verdict::Verdict {
            feature: "F-0001".into(),
            kind: "replay".into(),
            commit: commit.into(),
            clean,
            at: "t".into(),
            passed,
            failures: vec!["x".into()],
        };
        let state = || verdict::state(&d.0, "F-0001", Kind::Replay, Some("abc"));
        assert_eq!(state(), State::Missing);
        verdict::write(&d.0, &v("abc", true, true));
        assert_eq!(state(), State::Pass);
        verdict::write(&d.0, &v("abc", true, false));
        assert_eq!(state(), State::Fail { failures: vec!["x".into()] });
        verdict::write(&d.0, &v("abc", false, true));
        assert_eq!(state(), State::Stale { commit: "abc".into() });
        verdict::write(&d.0, &v("old", true, true));
        assert_eq!(state(), State::Stale { commit: "old".into() });
        assert_eq!(verdict::state(&d.0, "F-0001", Kind::Replay, None), State::Stale { commit: "old".into() });
    }

    #[test]
    fn the_loop_guard_stops_an_unchanged_agent_item() {
        let (d, f) = facts("description = \"x\"\nfeatures = [\"F-0001\", \"F-0002\"]\n", &["F-0001"]);
        let call = || guarded(&d.0, &f, f.next()).unwrap();
        assert_eq!(call()["item"], "Catalogue");
        assert_eq!(call()["item"], "Catalogue");
        let third = call();
        assert_eq!((third["item"].as_str(), third["stuck"]["item"].as_str()), (Some("Stuck"), Some("Catalogue")));
        // Something changing hands the item out again.
        std::fs::write(d.0.join("catalogue/F-0002.md"), feature("F-0002", "")).unwrap();
        let f2 = Facts::gather(&d.0).unwrap();
        assert_eq!(guarded(&d.0, &f2, f2.next()).unwrap()["item"], "Confirm");
        // The person's items never trip it.
        for _ in 0..4 {
            assert_eq!(guarded(&d.0, &f2, f2.next()).unwrap()["item"], "Confirm");
        }
    }
}

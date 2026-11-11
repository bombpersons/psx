//! `port check`, which runs the scenarios against the port (the port's
//! program over the port protocol, `psx_scenario::runner`) and writes the
//! verdict.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::status::Facts;
use super::status::Git;
use super::verdict::{self, Verdict};
use super::{Port, assets};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// `psx_scenario::runner::Report`, as the runner writes it.
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
    pub failures: Vec<String>,
    /// Where each quantity first differs, with the original's value and
    /// the port's there: port_check gives the first, psx web plots them.
    #[serde(default)]
    pub differences: Vec<Difference>,
    /// When a quantity differed: the port's compared quantities, initially
    /// and after each tick (`scenario show --port`).
    #[serde(default)]
    pub values: Vec<BTreeMap<String, Value>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Difference {
    pub quantity: String,
    pub at: String,
    #[serde(default)]
    pub expected: Value,
    #[serde(default)]
    pub got: Option<Value>,
}

/// How long a check spends on its pointers, all together.
const POINTERS: std::time::Duration = std::time::Duration::from_secs(300);

/// Where recording `r` (`<scenario>/<name>`) first differs in the report:
/// its quantities in the order they first differ, and the tick.
fn differences(r: &str, report: &ScenarioReport) -> Option<(Vec<String>, u64)> {
    let (id, name) = r.split_once('/')?;
    let run = report.scenarios.iter().find(|s| s.id == id)?.recordings.iter().find(|x| x.name == name)?;
    let mut ds: Vec<(u64, &str)> =
        run.differences.iter().filter_map(|d| Some((super::pointer::tick_of(&d.at)?, d.quantity.as_str()))).collect();
    ds.sort();
    let at = ds.first()?.0;
    let mut qs: Vec<String> = Vec::new();
    for (_, q) in ds {
        if !qs.iter().any(|x| x == q) {
            qs.push(q.to_string());
        }
    }
    Some((qs, at))
}

/// Where recording `r` first differs: the tick, each quantity differing
/// there with the original's value and the port's, and when the others
/// first differ. What a person reading the two side by side would see first.
fn first_difference(r: &str, report: &ScenarioReport) -> Option<Value> {
    let (id, name) = r.split_once('/')?;
    let run = report.scenarios.iter().find(|s| s.id == id)?.recordings.iter().find(|x| x.name == name)?;
    let mut ds: Vec<(u64, &Difference)> =
        run.differences.iter().filter_map(|d| Some((super::pointer::tick_of(&d.at)?, d))).collect();
    ds.sort_by_key(|(t, _)| *t);
    let tick = ds.first()?.0;
    let at: Vec<Value> = ds
        .iter()
        .filter(|(t, _)| *t == tick)
        .map(|(_, d)| json!({"quantity": d.quantity, "original": d.expected, "port": d.got}))
        .collect();
    let later: Vec<Value> =
        ds.iter().filter(|(t, _)| *t > tick).map(|(t, d)| json!({"quantity": d.quantity, "tick": t})).collect();
    let mut v = json!({"tick": tick, "values": at});
    if !later.is_empty() {
        v["later"] = json!(later);
    }
    Some(v)
}

/// The port's values for recording `r` (`<scenario>/<name>`) from the last
/// psx port check, with its commit: kept only where a quantity differed.
pub(crate) fn port_values(root: &Path, r: &str) -> Result<(String, Vec<BTreeMap<String, Value>>), String> {
    let last = std::fs::read(root.join(LAST))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .ok_or("no psx port check has run here")?;
    let commit = last["commit"].as_str().unwrap_or("").to_string();
    let report = serde_json::from_value::<ScenarioReport>(last["scenarios"].clone())
        .map_err(|_| "the last psx port check ran no scenarios")?;
    let (id, name) = r.split_once('/').ok_or("not <scenario>/<name>")?;
    let run = report
        .scenarios
        .into_iter()
        .find(|s| s.id == id)
        .and_then(|s| s.recordings.into_iter().find(|x| x.name == name));
    match run {
        Some(x) if !x.values.is_empty() => Ok((commit, x.values)),
        Some(_) => Err(format!("the last psx port check ({commit:.7}) kept no port values for {r}: it matched")),
        None => Err(format!("the last psx port check ({commit:.7}) didn't run {r}")),
    }
}

/// Each recording named, with what the scenario run found:
/// its failures, its scenario's problems, or that it did not run.
fn recording_results<'a>(
    names: impl Iterator<Item = &'a String>,
    report: &ScenarioReport,
) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for name in names {
        let (id, rec) = name.split_once('/').unwrap_or((name.as_str(), ""));
        let failures = match report.scenarios.iter().find(|s| s.id == id) {
            None => vec![format!("not run: no scenarios/{id}.toml")],
            Some(s) if !s.problems.is_empty() => s.problems.clone(),
            Some(s) => match s.recordings.iter().find(|r| r.name == rec) {
                None => vec!["not run: the scenarios have no such recording".into()],
                Some(r) => r.failures.clone(),
            },
        };
        out.insert(name.clone(), failures);
    }
    out
}

/// What the port has not registered for `scenarios`, by the last check
/// when it ran at `head`: a launcher for each one's kind, and each quantity
/// they compare observed. Empty when every one is.
pub fn unimplemented(root: &Path, scenarios: &[String], head: Option<&str>) -> Vec<String> {
    let report = match last_report(root, head) {
        Ok(r) => r,
        Err(why) => return vec![why.into()],
    };
    let mut out = Vec::new();
    for id in scenarios {
        let Some(s) = crate::scenario::Loaded::load(root, id).scenario else {
            out.push(format!("{id}: no scenario file"));
            continue;
        };
        if !report.kinds.contains(&s.kind) {
            out.push(format!("{id}: no launcher for scenario kind {}", s.kind));
        }
        for q in s.compare.keys().filter(|q| !report.quantities.contains(q)) {
            out.push(format!("{id}: the port observes no {q} (app.observe)"));
        }
    }
    out
}

/// The scenario report of the last check, when it judges `head`.
fn last_report(root: &Path, head: Option<&str>) -> Result<ScenarioReport, &'static str> {
    let last = std::fs::read(root.join(LAST)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    // A check judges HEAD when only notes or the review changed since.
    let judged = |l: &Value| l["commit"].as_str().is_some_and(|c| verdict::judges(root, c, head));
    let Some(last) = last.filter(judged) else {
        return Err("no psx port check at HEAD");
    };
    serde_json::from_value::<ScenarioReport>(last["scenarios"].clone())
        .map_err(|_| "the last psx port check ran no scenarios")
}

/// Runs the importer (the manifest's `import`) on the workspace's disc into
/// `.psx/port/import/` (a file is imported when an output's provenance names
/// it) and its tests (`import_test`) with `PSX_IMPORT_REPORT` set (a file is
/// checked when a passing test marked it, as `import::checking` does).
fn run_importers(root: &Path, head: &str, archives: &[String]) -> crate::Result<assets::Report> {
    let mut r = assets::Report { commit: head.into(), clean: true, at: crate::stamp::utc_now(), ..Default::default() };
    let media = crate::context::resolve(root, None, None, None, false)
        .ok()
        .and_then(|r| r.media)
        .map(|m| std::path::absolute(&m).unwrap_or(m));
    let commands = super::manifest::Commands::read(root).map_err(Error::invalid)?;
    let errors = super::process::why;
    let run = |command: &[String], args: &[&str], env: Option<(&str, &Path)>| {
        let mut c = super::manifest::command(root, command, args);
        if let Some((k, v)) = env {
            c.env(k, v);
        }
        c.output().map_err(|e| Error::invalid(format!("{}: {e}", command[0])))
    };
    let out_dir = root.join(".psx/port/import");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| Error::io(&out_dir, e))?;
    match (&media, &commands.import) {
        (_, None) => r.failures.push(format!("no importer: {} names no import command", super::manifest::FILE)),
        (None, _) => r.failures.push("no media: the workspace's disc image (psx context --image ... --save)".into()),
        (Some(m), Some(import)) => {
            let disc = m.to_string_lossy().into_owned();
            let o = run(import, &[&disc, &out_dir.to_string_lossy()], None)?;
            if !o.status.success() {
                r.failures.push(format!("the importers on the disc: {}", errors(&o)));
            }
            r.imported = provenance_sources(&out_dir);
            if !archives.is_empty() {
                let mut args = vec!["index", disc.as_str()];
                args.extend(archives.iter().map(String::as_str));
                let o = run(import, &args, None)?;
                let listed: Value = serde_json::from_slice(&o.stdout).unwrap_or_default();
                if !o.status.success() || !listed.is_object() {
                    r.failures.push(format!("the importer's index: {}", errors(&o)));
                }
                for a in archives {
                    match &listed[a.as_str()] {
                        Value::Null => {}
                        v if v.get("error").is_some() => r
                            .failures
                            .push(format!("the importer's index of {a}: {}", v["error"].as_str().unwrap_or(""))),
                        v => match serde_json::from_value::<Vec<assets::IndexEntry>>(v["entries"].clone()) {
                            Ok(entries) => {
                                r.index.insert(a.clone(), entries);
                            }
                            Err(e) => r.failures.push(format!("the importer's index of {a}: {e}")),
                        },
                    }
                }
            }
        }
    }
    let marks = root.join(".psx/port/import-checked.txt");
    let _ = std::fs::remove_file(&marks);
    if let Some(test) = &commands.import_test {
        let o = run(test, &[], Some(("PSX_IMPORT_REPORT", &marks)))?;
        if !o.status.success() {
            r.failures.push(format!("the importers' tests: {}", errors(&o)));
        }
    }
    let text = std::fs::read_to_string(&marks).unwrap_or_default();
    let mut checked: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(assets::disc_path).collect();
    checked.sort();
    checked.dedup();
    r.checked = checked;
    Ok(r)
}

/// The disc files the import's `*.provenance.json` files name.
fn provenance_sources(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if p.to_string_lossy().ends_with(".provenance.json") {
                let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap_or_default()).unwrap_or_default();
                out.extend(v["source"].as_str().map(assets::disc_path));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `.psx/port/last.json`: the last check's scenario report and the commit
/// it ran at.
const LAST: &str = ".psx/port/last.json";

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortCheckArgs {
    /// Point at the original only for this scenario's failing recordings,
    /// and leave the importers; default every scenario.
    #[serde(default)]
    pub scenario: Option<String>,
    /// Also replay the work recordings on the original with the
    /// agent's C (decomp/) in place of the functions it defines: alone,
    /// those whose last code check passed on this C and names;
    /// `--decomp=all` every one; `--decomp=a,b` those named. The verdicts
    /// are the port's either way.
    #[arg(long, num_args = 0..=1, require_equals = true, value_delimiter = ',')]
    #[serde(default)]
    pub decomp: Option<Vec<String>>,
}

/// A test's report, or why the test wrote none.
type Ran<T> = Result<T, String>;

/// The scenarios against the port: built, started in protocol mode and
/// driven by psx_scenario's runner ([`super::process`]), with the report
/// written to `scenarios`. A port that could not be built or started gets
/// why.
fn run_tests(root: &Path, scenarios: &Path) -> crate::Result<Ran<ScenarioReport>> {
    let _ = std::fs::remove_file(scenarios);
    let process = match super::process::start(root) {
        Ok(p) => p,
        Err(why) => return Ok(Err(why)),
    };
    let report = psx_scenario::Scenarios::new(root, move || process.port()).report();
    let bytes = serde_json::to_vec_pretty(&report).unwrap_or_default();
    crate::write_atomic(scenarios, &bytes).map_err(|e| Error::io(scenarios, e))?;
    serde_json::from_slice(&bytes).map(Ok).map_err(|e| Error::invalid(format!("{}: {e}", scenarios.display())))
}

impl PortCheckArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let root = &ctx.root;
        let port = Port::load(root)?;
        let decomp = self.decomp.as_ref().map(|w| crate::decomp::Replacement::new(root, w)).transpose()?;
        let git = Git::read(root);
        let Some(head) = git.head.clone().filter(|_| git.clean) else {
            return Err(Error::invalid("commit first: a verdict counts only for a commit with no uncommitted changes"));
        };
        if let Some(id) = &self.scenario {
            if !port.recordings.all().any(|r| super::scenario_of(r) == id) {
                return Err(Error::invalid(format!("{id}: no recordings under scenarios/{id}/")));
            }
        }
        let scenarios_path = root.join(".psx/port/scenarios.json");
        std::fs::create_dir_all(scenarios_path.parent().unwrap()).map_err(|e| Error::io(&scenarios_path, e))?;
        let ran = run_tests(root, &scenarios_path)?;
        // The importers, when there is something to import: run on the disc
        // and their tests, judged at the same commit.
        let imports = (self.scenario.is_none()
            && (!port.goal.assets.is_none()
                || port.scenario_assets.values().any(|a| !a.is_empty())
                || !port.formats.is_empty()))
        .then(|| run_importers(root, &head, &assets::archive_files(root, &port)))
        .transpose()?;
        let after = Git::read(root);
        if after.head != git.head || !after.clean {
            return Err(Error::invalid(
                "the workspace changed while the check ran (a build may have changed a tracked file): commit and check again",
            ));
        }
        let kept =
            |p: &Path| serde_json::from_slice::<Value>(&std::fs::read(p).unwrap_or_default()).unwrap_or_default();
        let last = json!({"commit": head, "at": crate::stamp::utc_now(),
                          "scenarios": if ran.is_ok() { kept(&scenarios_path) } else { Value::Null }});
        let p = root.join(LAST);
        crate::write_atomic(&p, &serde_json::to_vec_pretty(&last).unwrap_or_default()).map_err(|e| Error::io(&p, e))?;
        let work = &port.recordings.work;
        let (failures, recordings) = match &ran {
            Ok(r) => (Vec::new(), recording_results(port.recordings.all(), r)),
            Err(why) => (vec![why.clone()], BTreeMap::new()),
        };
        let mut v = Verdict {
            goal: super::GOAL_ID.into(),
            commit: head.clone(),
            clean: true,
            at: crate::stamp::utc_now(),
            failures,
            recordings,
            differs: BTreeMap::new(),
            pointers: BTreeMap::new(),
        };
        let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Ok(r) = &ran {
            let failed: Vec<&String> = work.iter().filter(|rec| !v.of(rec).is_empty()).collect();
            for rec in failed {
                if let Some((qs, tick)) = differences(rec, r) {
                    v.differs.insert(rec.clone(), verdict::Differs { tick, quantities: qs.len() });
                    found.insert(rec.clone(), qs);
                }
            }
        }
        let failing = v.failing(work);
        // Where to look, for each failing work recording, the first given first.
        if ran.is_ok() {
            let to_point: Vec<(String, Vec<String>, u64)> = failing
                .iter()
                .filter(|r| self.scenario.as_ref().is_none_or(|s| super::scenario_of(r) == s))
                .filter_map(|r| Some((r.clone(), found.get(r)?.clone(), v.differs.get(r)?.tick)))
                .collect();
            v.pointers = super::pointer::pointers(ctx, &port, &to_point, POINTERS);
        }
        verdict::write(root, &v)?;
        // Held-out plays are counted, not shown: port status shows them once
        // the goal is reviewed.
        let results: BTreeMap<&String, &Vec<String>> =
            work.iter().filter_map(|r| Some((r, v.recordings.get(r)?))).collect();
        let passing = work.iter().filter(|r| v.of(r).is_empty()).count();
        let mut verdict = json!({"passed": failing.is_empty() && !work.is_empty() && v.failures.is_empty(),
                                 "work": {"total": work.len(), "pass": passing},
                                 "held_out": port.recordings.held_out.len()});
        if !v.failures.is_empty() {
            verdict["failures"] = json!(v.failures);
        }
        if let Some(r) = failing.first() {
            verdict["failing"] = json!(
                failing
                    .iter()
                    .take(10)
                    .map(|r| {
                        let mut f = json!({"recording": r, "differs": results.get(r)});
                        if let Some(first) = ran.as_ref().ok().and_then(|rep| first_difference(r, rep)) {
                            f["first"] = first;
                        }
                        f
                    })
                    .collect::<Vec<_>>()
            );
            if let Some(p) = v.pointers.get(r) {
                verdict["pointer"] = p.clone();
            }
        }
        let work_recordings: BTreeMap<String, Vec<String>> = {
            let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for r in work {
                if let Some((id, name)) = r.split_once('/') {
                    m.entry(id.to_string()).or_default().push(name.to_string());
                }
            }
            m
        };
        let mut out = json!({"commit": head, "verdict": verdict});
        if let Some((why, _)) = Facts::gather(root).ok().and_then(|f| f.review_due(root)) {
            out["review_due"] = json!(format!("{why}: port_status has the brief for a reviewer"));
        }
        if let Some(r) = imports {
            let p = root.join(assets::REPORT);
            crate::write_atomic(&p, &serde_json::to_vec_pretty(&r).unwrap_or_default())
                .map_err(|e| Error::io(&p, e))?;
            out["assets"] = json!({"imported": r.imported.len(), "checked": r.checked.len(), "failures": r.failures});
        }
        if let Some(d) = &decomp {
            let (mut ok, mut scenarios) = (true, Vec::new());
            for (id, names) in &work_recordings {
                let (same, mut c) =
                    crate::scenario::with_c(ctx, id, Some(names), d, std::time::Duration::from_secs(600))?;
                ok &= same;
                c["id"] = json!(id);
                scenarios.push(c);
            }
            out["decomp"] = json!({"ok": ok, "functions": d.functions.iter().map(|f| &f.name).collect::<Vec<_>>(),
                "left_out": d.left_out.iter().map(|(f, why)| json!({"function": f, "why": why})).collect::<Vec<_>>(),
                "scenarios": scenarios});
        }
        Ok(out)
    }
}

impl Op for PortCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_difference_gives_both_values_and_when_the_rest_differ() {
        let report: ScenarioReport = serde_json::from_value(json!({"scenarios": [{"id": "level", "recordings": [
            {"name": "route", "failures": ["differs"], "differences": [
                {"quantity": "x", "at": "tick 92", "expected": 559, "got": 484},
                {"quantity": "camera_x", "at": "tick 95", "expected": 10, "got": 12},
                {"quantity": "facing", "at": "tick 92", "expected": 7, "got": null}]}]}]}))
        .unwrap();
        let first = first_difference("level/route", &report).unwrap();
        assert_eq!(first["tick"], 92);
        assert_eq!(first["values"][0], json!({"quantity": "x", "original": 559, "port": 484}));
        assert_eq!(first["values"][1]["port"], Value::Null);
        assert_eq!(first["later"], json!([{"quantity": "camera_x", "tick": 95}]));
        assert_eq!(first_difference("level/other", &report), None);
    }

    #[test]
    fn each_named_recording_gets_what_the_run_found() {
        let report: ScenarioReport = serde_json::from_value(json!({"scenarios": [
            {"id": "jump", "recordings": [{"name": "stomp", "failures": []},
                                          {"name": "long", "failures": ["differs from the recording in croc_y"],
                                           "differences": [{"quantity": "croc_y", "at": "tick 9"}, {"quantity": "croc_x", "at": "tick 4"}]}]},
            {"id": "swim", "problems": ["no launcher registered for scenario kind water"]},
        ]}))
        .unwrap();
        let names: Vec<String> =
            ["jump/stomp", "jump/long", "jump/gone", "swim/lap", "walk/a"].iter().map(|s| s.to_string()).collect();
        let r = recording_results(names.iter(), &report);
        assert_eq!(r["jump/stomp"], Vec::<String>::new());
        assert_eq!(r["jump/long"], ["differs from the recording in croc_y"]);
        assert_eq!(r["jump/gone"], ["not run: the scenarios have no such recording"]);
        assert_eq!(r["swim/lap"], ["no launcher registered for scenario kind water"]);
        assert_eq!(r["walk/a"], ["not run: no scenarios/walk.toml"]);
        assert_eq!(differences("jump/long", &report), Some((vec!["croc_x".into(), "croc_y".into()], 4)));
    }
}

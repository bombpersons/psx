//! `port check`, which runs the features' scenarios against the port (the
//! port's program over the port protocol, `psx_scenario::runner`) and
//! writes their scenario verdicts.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::next::Git;
use super::verdict::{self, Verdict};
use super::{Entry, Port, assets};
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
}

/// Each recording the entry names, with what the scenario run found:
/// its failures, its scenario's problems, or that it did not run.
fn recording_results(e: &Entry, report: &ScenarioReport) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for name in e.recordings.all() {
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

/// What the port has not registered for the feature, by the last check when
/// it ran at `head`: a launcher for each of its scenarios' kinds, and each
/// quantity they compare observed. Empty when every one is.
pub fn unimplemented(root: &Path, e: &Entry, head: Option<&str>) -> Vec<String> {
    let report = match last_report(root, head) {
        Ok(r) => r,
        Err(why) => return vec![why.into()],
    };
    let mut out = Vec::new();
    for id in &e.scenarios() {
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

/// Runs the importers on the workspace's disc into `.psx/port/import/`
/// (a file is imported when an output's provenance names it) and their
/// tests with `PSX_IMPORT_REPORT` set (a file is checked when a passing
/// test marked it with `import::checking`).
fn run_importers(root: &Path, head: &str, archives: &[String]) -> crate::Result<assets::Report> {
    let mut r = assets::Report { commit: head.into(), clean: true, at: crate::stamp::utc_now(), ..Default::default() };
    let media = crate::context::resolve(root, None, None, None, false)
        .ok()
        .and_then(|r| r.media)
        .map(|m| std::path::absolute(&m).unwrap_or(m));
    let errors = |out: &std::process::Output| -> String {
        let text = String::from_utf8_lossy(&out.stderr);
        let e: Vec<&str> = text.lines().filter(|l| l.starts_with("error") || l.contains("panicked")).take(5).collect();
        if e.is_empty() { text.lines().last().unwrap_or("no output").to_string() } else { e.join("; ") }
    };
    let cargo = |args: &[&str], env: Option<(&str, &Path)>| {
        let mut c = std::process::Command::new("cargo");
        c.args(args).current_dir(root);
        if let Some((k, v)) = env {
            c.env(k, v);
        }
        c.output().map_err(|e| Error::invalid(format!("cargo: {e}")))
    };
    let out_dir = root.join(".psx/port/import");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| Error::io(&out_dir, e))?;
    match &media {
        None => r.failures.push("no media: the workspace's disc image (psx context --image ... --save)".into()),
        Some(m) => {
            let disc = m.to_string_lossy().into_owned();
            let o = cargo(&["run", "--locked", "-q", "-p", "import", "--", &disc, &out_dir.to_string_lossy()], None)?;
            if !o.status.success() {
                r.failures.push(format!("the importers on the disc: {}", errors(&o)));
            }
            r.imported = provenance_sources(&out_dir);
            if !archives.is_empty() {
                let mut args = vec!["run", "--locked", "-q", "-p", "import", "--", "index", disc.as_str()];
                args.extend(archives.iter().map(String::as_str));
                let o = cargo(&args, None)?;
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
    let o = cargo(&["test", "--locked", "--no-fail-fast", "-q", "-p", "import"], Some(("PSX_IMPORT_REPORT", &marks)))?;
    if !o.status.success() {
        r.failures.push(format!("the importers' tests: {}", errors(&o)));
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
    /// The feature (F-NNNN); default every catalogued feature of the goal's
    /// closure.
    #[serde(default)]
    pub feature: Option<String>,
    /// Also replay the features' work recordings on the original with the
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
        // The importers, when there is something to import: run on the disc
        // and their tests, judged at the same commit.
        let imports = (self.feature.is_none()
            && (!port.goal.assets.is_none()
                || port.entries.values().any(|e| !e.assets.is_empty())
                || !port.formats.is_empty()))
        .then(|| run_importers(root, &head, &assets::archive_files(root, &port)))
        .transpose()?;
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
        let mut work_recordings: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in &features {
            for r in &e.recordings.work {
                if let Some((id, name)) = r.split_once('/') {
                    work_recordings.entry(id.to_string()).or_default().push(name.to_string());
                }
            }
        }
        for e in features {
            let (failures, recordings) = match &ran {
                Ok(r) => (Vec::new(), recording_results(e, r)),
                Err(why) => (vec![why.clone()], BTreeMap::new()),
            };
            let v = Verdict {
                feature: e.id.clone(),
                commit: head.clone(),
                clean: true,
                at: crate::stamp::utc_now(),
                failures,
                recordings,
            };
            verdict::write(root, &v)?;
            // Held-out recordings are counted, not shown: port status shows
            // them once the feature is reviewed.
            let work: BTreeMap<&String, Vec<String>> = e.recordings.work.iter().map(|r| (r, v.of(r))).collect();
            let mut row = json!({"feature": v.feature, "passed": work.values().all(Vec::is_empty) && !work.is_empty(),
                                 "work": work, "held_out": e.recordings.held_out.len()});
            if !v.failures.is_empty() {
                row["failures"] = json!(v.failures);
            }
            verdicts.push(row);
        }
        let mut out = json!({"commit": head, "verdicts": verdicts});
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
    fn each_named_recording_gets_what_the_run_found() {
        let report: ScenarioReport = serde_json::from_value(json!({"scenarios": [
            {"id": "jump", "recordings": [{"name": "stomp", "failures": []},
                                          {"name": "long", "failures": ["tick 4 croc_y: expected 3, got 2"]}]},
            {"id": "swim", "problems": ["no launcher registered for scenario kind water"]},
        ]}))
        .unwrap();
        let mut e = Entry::default();
        e.recordings.work = vec!["jump/stomp".into(), "jump/long".into(), "jump/gone".into()];
        e.recordings.held_out = vec!["swim/lap".into(), "walk/a".into()];
        let r = recording_results(&e, &report);
        assert_eq!(r["jump/stomp"], Vec::<String>::new());
        assert_eq!(r["jump/long"], ["tick 4 croc_y: expected 3, got 2"]);
        assert_eq!(r["jump/gone"], ["not run: the scenarios have no such recording"]);
        assert_eq!(r["swim/lap"], ["no launcher registered for scenario kind water"]);
        assert_eq!(r["walk/a"], ["not run: no scenarios/walk.toml"]);
    }
}

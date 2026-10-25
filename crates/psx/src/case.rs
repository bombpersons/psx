//! `psx case`: recording a case (a start state, pad input and a field map
//! subject in; the case's JSON and observation CSV out, with the game's tick
//! column and a recipe that records it again), and checking published cases
//! by recording them again from their recipes.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

const ROLES: [&str; 4] = ["heldout", "calibration", "diagnostic", "exploratory"];

#[derive(clap::Args, Clone, Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct CaseRecordArgs {
    /// Case id, used for its file names (letters, digits, _ and -).
    pub name: String,
    /// The start state (state.bin): row 0.
    #[arg(long)]
    pub from_state: PathBuf,
    /// Fields to record: rows 0..=frames.
    #[arg(long)]
    pub frames: u32,
    /// Pad input, as run's.
    #[arg(long, conflicts_with = "replay")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<crate::replay::Events>,
    /// Pad input from a replay file.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<PathBuf>,
    /// The field map subject recorded, SUBJECT or SUBJECT[N]: its fields are the case's
    /// columns, by field name.
    #[arg(long)]
    pub subject: String,
    /// More columns: field map names (globals, SUBJECT.FIELD) or ADDR:FMT:NAME; repeatable.
    #[arg(long = "sample")]
    #[serde(default, rename = "sample")]
    pub samples: Vec<String>,
    /// A column that is input rather than compared; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub activation: Vec<String>,
    /// COLUMN or COLUMN=TOLERANCE to compare; default every other column, exactly. Repeatable.
    #[arg(long)]
    #[serde(default)]
    pub compare: Vec<String>,
    /// The spec the case supports, e.g. SPEC-0007.
    #[arg(long)]
    pub spec: String,
    /// heldout, calibration, diagnostic or exploratory.
    #[arg(long)]
    pub role: String,
    /// The level, by its imported name.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// A subject field whose row 0 value is its type.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// A note kept in the recipe.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Field map (default samples.toml).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<PathBuf>,
    /// Disc image or EXE (default: the state's).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<PathBuf>,
    /// Directory for the case files (default specs/cases).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases: Option<PathBuf>,
    /// Directory for the recipe (default notes/recordings).
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipes: Option<PathBuf>,
    /// Replace a case of this name.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CaseCheckArgs {
    /// Case ids; default every recipe in the recipes directory.
    #[serde(default)]
    pub names: Vec<String>,
    /// Directory of the case files (default specs/cases).
    #[arg(long)]
    pub cases: Option<PathBuf>,
    /// Directory of the recipes (default notes/recordings).
    #[arg(long)]
    pub recipes: Option<PathBuf>,
    /// Write the new recording over a case that differs.
    #[arg(long)]
    #[serde(default)]
    pub write: bool,
}

fn dirs(ctx: &Ctx, cases: &Option<PathBuf>, recipes: &Option<PathBuf>) -> (PathBuf, PathBuf) {
    let at = |d: &Option<PathBuf>, default: &str| d.clone().unwrap_or_else(|| ctx.root.join(default));
    (at(cases, "specs/cases"), at(recipes, "notes/recordings"))
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn read(p: &Path) -> crate::Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| Error::io(p, e))
}

fn write(p: &Path, bytes: &[u8]) -> crate::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    crate::write_atomic(p, bytes).map_err(|e| Error::io(p, e))
}

/// A CSV of plain cells (psx's samples: numbers and names, no quoting).
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn parse(text: &str) -> Table {
        let mut lines = text.lines().map(|l| l.split(',').map(str::to_string).collect::<Vec<_>>());
        Table { header: lines.next().unwrap_or_default(), rows: lines.collect() }
    }

    fn column(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }

    fn text(&self) -> String {
        let mut s = String::new();
        for line in std::iter::once(&self.header).chain(&self.rows) {
            s += &line.join(",");
            s.push('\n');
        }
        s
    }
}

/// A recording before it is written.
struct Recorded {
    case: Value,
    csv: String,
    recipe: Value,
    report: Value,
}

/// The rows whose tick rose, counted from row 0's tick column.
fn timing(t: &Table) -> Option<Value> {
    let i = t.column("tick")?;
    let ticks: Vec<u64> = t.rows.iter().map(|r| r[i].parse().unwrap_or(0)).collect();
    let total = ticks.last().copied().unwrap_or(0);
    let first = (1..ticks.len()).find(|&r| ticks[r] > ticks[r - 1]);
    let fields = ticks.len().saturating_sub(1) as u64;
    Some(json!({
        "ticks": total,
        "first_tick": first,
        "fields_per_tick": (total > 0).then(|| (fields as f64 / total as f64).round() as u64),
    }))
}

fn record(ctx: &Ctx, a: &CaseRecordArgs) -> crate::Result<Recorded> {
    if a.name.is_empty() || !a.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::invalid(format!("case name {:?}: letters, digits, _ and - only", a.name)));
    }
    if !ROLES.contains(&a.role.as_str()) {
        return Err(Error::invalid(format!("role {:?}: one of {}", a.role, ROLES.join(", "))));
    }
    let input: Value = match (&a.input, &a.replay) {
        (Some(e), _) => json!(e),
        (None, Some(p)) => {
            let e: crate::replay::Events =
                serde_json::from_slice(&read(p)?).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
            json!(e)
        }
        (None, None) => json!([]),
    };
    let mut sample = vec![a.subject.clone()];
    sample.extend(a.samples.iter().cloned());
    let run: crate::run::RunCmdArgs = serde_json::from_value(json!({
        "media": a.media, "from_state": a.from_state, "frames": a.frames,
        "input": input, "map": a.map, "sample": sample,
    }))
    .map_err(|e| Error::invalid(format!("case record: {e}")))?;
    let (complete, report) = run.run(ctx)?;
    if !complete {
        return Err(Error::new("run_incomplete", format!("the run ended early ({})", report["termination"]))
            .with("run_dir", &report["run_dir"]));
    }
    let samples = &report["samples"];
    let file = samples["file"].as_str().map(PathBuf::from).unwrap_or_default();
    let mut t = Table::parse(&String::from_utf8_lossy(&read(&file)?));
    // The subject's fields by their own names; the case names its subject.
    let prefix = format!("{}.", a.subject);
    for h in &mut t.header {
        if let Some(f) = h.strip_prefix(&prefix) {
            *h = f.to_string();
        }
    }
    for (i, h) in t.header.iter().enumerate() {
        if t.header[..i].contains(h) {
            return Err(Error::invalid(format!("two columns are named {h:?}: name one differently in the map")));
        }
    }
    let fixed = ["run_frame", "frame", "tick"];
    let known = |c: &str| t.column(c).is_some() && !fixed.contains(&c);
    let columns: Vec<&String> = t.header.iter().filter(|h| !fixed.contains(&h.as_str())).collect();
    let list = || columns.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ");
    for c in &a.activation {
        if !known(c) {
            return Err(Error::invalid(format!("activation {c:?} is not a column ({})", list())));
        }
    }
    let mut compare = serde_json::Map::new();
    for c in &a.compare {
        let (col, tol) = match c.split_once('=') {
            Some((col, tol)) => (col.trim(), tol.trim()),
            None => (c.trim(), "0"),
        };
        if !known(col) {
            return Err(Error::invalid(format!("compare {col:?} is not a column ({})", list())));
        }
        let tol: Value = match (tol.parse::<i64>(), tol.parse::<f64>()) {
            (Ok(n), _) => json!(n),
            (_, Ok(x)) => json!(x),
            _ => return Err(Error::invalid(format!("compare {c:?}: tolerance {tol:?} is not a number"))),
        };
        compare.insert(col.to_string(), tol);
    }
    if a.compare.is_empty() {
        for c in columns.iter().filter(|c| !a.activation.contains(c)) {
            compare.insert(c.to_string(), json!(0));
        }
    }
    let mut warnings = Vec::new();
    let timing = timing(&t);
    match &timing {
        None => warnings.push(
            "no tick column: the field map has no [tick], so the case cannot say which fields the ticks ran in (guide topic fields)"
                .to_string(),
        ),
        Some(v) if v["ticks"] == 0 => warnings.push("no tick ran: is the game paused, or [tick] wrong?".to_string()),
        Some(_) => {}
    }
    let changed = &samples["tick"]["changed_without_tick"];
    if changed.as_object().is_some_and(|m| !m.is_empty()) {
        warnings.push(
            "columns changed on rows whose tick did not (timing.changed_without_tick): note them in the case or move [tick] (guide topic fields)"
                .to_string(),
        );
    }
    let kind = match &a.kind {
        Some(k) => {
            let i = t.column(k).ok_or_else(|| Error::invalid(format!("kind {k:?} is not a column ({})", list())))?;
            Some(json!(t.rows.first().map(|r| r[i].clone()).unwrap_or_default()))
        }
        None => None,
    };
    let kind = kind.map(|k| k.as_str().and_then(|s| s.parse::<i64>().ok()).map_or(k, |n| json!(n)));
    let subject = match (a.subject.split_once('['), kind) {
        (None, None) => json!(a.subject),
        (split, kind) => {
            let (name, index) = match split {
                Some((n, rest)) => (n, rest.trim_end_matches(']').parse::<u64>().ok()),
                None => (a.subject.as_str(), None),
            };
            let mut s = json!({"name": name});
            if let Some(i) = index {
                s["index"] = json!(i);
            }
            if let Some(k) = kind {
                s["type"] = k;
            }
            s
        }
    };
    let mut case = json!({
        "id": a.name, "spec": a.spec, "role": a.role, "subject": subject,
        "start": {"from": "first_row"}, "input": input,
        "observations": format!("{}.csv", a.name), "compare": compare,
    });
    if let Some(l) = &a.level {
        case["level"] = json!(l);
    }
    if let Some(v) = &timing {
        for k in ["fields_per_tick", "first_tick"] {
            if !v[k].is_null() {
                case[k] = v[k].clone();
            }
        }
    }
    if !a.activation.is_empty() {
        case["activation"] = json!(a.activation);
    }
    let csv = t.text();
    // The recipe: these arguments with the input inlined, and what they ran on.
    let mut args = a.clone();
    (args.replay, args.cases, args.recipes, args.replace) = (None, None, None, false);
    args.input = serde_json::from_value(input).ok();
    let id = &report["identity"];
    let state = read(&a.from_state)?;
    let recipe = json!({
        "case": a.name,
        "args": args,
        "recorded_with": crate::context::version(),
        "state_sha256": sha256(&state),
        "map": samples["map"]["path"],
        "map_sha256": samples["map"]["path"].as_str().and_then(|p| std::fs::read(p).ok()).map(|b| sha256(&b)),
        "media_identity": id["media_identity"], "bios_sha256": id["bios_sha256"],
        "rows": t.rows.len(), "csv_sha256": sha256(csv.as_bytes()),
        "how": "psx case check re-records this case from args and compares it with the published CSV",
    });
    let mut out = json!({
        "rows": t.rows.len(), "columns": columns, "compare": case["compare"], "run_dir": report["run_dir"],
        "warnings": warnings,
    });
    if let Some(v) = timing {
        out["timing"] = v;
        out["timing"]["fields_by_ticks_run"] = samples["tick"]["fields_by_ticks_run"].clone();
        out["timing"]["changed_without_tick"] = changed.clone();
    }
    Ok(Recorded { case, csv, recipe, report: out })
}

fn save(r: &Recorded, name: &str, cases: &Path, recipes: &Path) -> crate::Result<Value> {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
    let (case, csv, recipe) =
        (cases.join(format!("{name}.json")), cases.join(format!("{name}.csv")), recipes.join(format!("{name}.json")));
    write(&csv, r.csv.as_bytes())?;
    write(&case, pretty(&r.case).as_bytes())?;
    write(&recipe, pretty(&r.recipe).as_bytes())?;
    Ok(json!({"case": case, "observations": csv, "recipe": recipe}))
}

impl Op for CaseRecordArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result((|| {
            let (cases, recipes) = dirs(ctx, &self.cases, &self.recipes);
            let existing = cases.join(format!("{}.json", self.name));
            if existing.exists() && !self.replace {
                return Err(Error::invalid(format!(
                    "{} exists: replace=true records over it (psx case check compares first)",
                    existing.display()
                )));
            }
            let r = record(ctx, &self)?;
            let mut v = save(&r, &self.name, &cases, &recipes)?;
            for (k, x) in r.report.as_object().into_iter().flatten() {
                v[k] = x.clone();
            }
            v["case_json"] = r.case;
            Ok(v)
        })())
    }
}

/// The cells where a new recording differs from the published one.
fn differences(old: &Table, new: &Table) -> Value {
    let missing: Vec<&String> = old.header.iter().filter(|h| new.column(h).is_none()).collect();
    let shared: Vec<(usize, usize, &String)> =
        old.header.iter().enumerate().filter_map(|(i, h)| new.column(h).map(|j| (i, j, h))).collect();
    let (mut count, mut first) = (0u64, Vec::new());
    for (r, (a, b)) in old.rows.iter().zip(&new.rows).enumerate() {
        for &(i, j, h) in &shared {
            if a.get(i) != b.get(j) {
                count += 1;
                if first.len() < 8 {
                    first.push(json!({"row": r, "column": h, "published": a.get(i), "recorded": b.get(j)}));
                }
            }
        }
    }
    json!({
        "cells": count, "first": first, "missing_columns": missing,
        "rows": {"published": old.rows.len(), "recorded": new.rows.len()},
        "same": count == 0 && missing.is_empty() && old.rows.len() == new.rows.len(),
    })
}

impl Op for CaseCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report((|| {
            let (cases, recipes) = dirs(ctx, &self.cases, &self.recipes);
            let mut names = self.names.clone();
            if names.is_empty() {
                let list = std::fs::read_dir(&recipes).map_err(|e| Error::io(&recipes, e))?;
                names = list
                    .filter_map(|e| e.ok()?.path().file_name()?.to_str()?.strip_suffix(".json").map(str::to_string))
                    .collect();
                names.sort();
            }
            let (mut same, mut results) = (0, Vec::new());
            for name in &names {
                let one = || -> crate::Result<Value> {
                    let recipe: Value = serde_json::from_slice(&read(&recipes.join(format!("{name}.json")))?)
                        .map_err(|e| Error::malformed(format!("recipe {name}: {e}")))?;
                    let args: CaseRecordArgs = serde_json::from_value(recipe["args"].clone())
                        .map_err(|e| Error::malformed(format!("recipe {name}: args: {e}")))?;
                    let mut warnings = Vec::new();
                    if recipe["state_sha256"].as_str() != Some(&sha256(&read(&args.from_state)?)) {
                        warnings.push("the start state changed since the case was recorded");
                    }
                    let r = record(ctx, &args)?;
                    let old = Table::parse(&String::from_utf8_lossy(&read(&cases.join(format!("{name}.csv")))?));
                    let mut d = differences(&old, &Table::parse(&r.csv));
                    let case: Value = serde_json::from_slice(&read(&cases.join(format!("{name}.json")))?)
                        .map_err(|e| Error::malformed(format!("case {name}: {e}")))?;
                    let keys = ["fields_per_tick", "first_tick", "input", "subject"];
                    let changed: Vec<&str> = keys.into_iter().filter(|k| case[k] != r.case[k]).collect();
                    d["same"] = json!(d["same"] == true && changed.is_empty());
                    d["case_keys_changed"] = json!(changed);
                    d["warnings"] = json!(warnings);
                    if self.write && d["same"] != true {
                        d["written"] = save(&r, name, &cases, &recipes)?;
                    }
                    Ok(d)
                };
                let mut v = one().unwrap_or_else(|e| json!({"same": false, "error": e.to_value()["error"]}));
                if v["same"] == true {
                    same += 1;
                    v = json!({"same": true});
                }
                v["case"] = json!(name);
                results.push(v);
            }
            let all = same == results.len();
            Ok((
                all,
                json!({
                    "status": if all { "complete" } else { "differs" },
                    "cases": results.len(), "same": same, "differ": results.len() - same,
                    "results": results,
                }),
            ))
        })())
    }
}

#[cfg(test)]
mod tests {
    use super::{Table, differences, timing};

    #[test]
    fn timing_comes_from_the_tick_column() {
        let t = Table::parse("run_frame,frame,tick,x\n0,10,0,5\n1,11,0,5\n2,12,1,6\n3,13,1,6\n4,14,2,7\n5,15,4,9\n");
        let v = timing(&t).unwrap();
        assert_eq!((v["ticks"].clone(), v["first_tick"].clone()), (4.into(), 2.into()));
        // Five fields, four ticks (one a catch-up): steady play is a tick a field.
        assert_eq!(v["fields_per_tick"], 1);
        assert!(timing(&Table::parse("run_frame,frame,x\n0,1,2\n")).is_none());
    }

    #[test]
    fn differences_name_cells_by_column_in_any_order() {
        let old = Table::parse("run_frame,frame,x,y,tick\n0,1,5,6,0\n1,2,5,7,1\n");
        let same = Table::parse("run_frame,frame,tick,y,x,extra\n0,1,0,6,5,9\n1,2,1,7,5,9\n");
        assert_eq!(differences(&old, &same)["same"], true);
        let moved = Table::parse("run_frame,frame,tick,x\n0,1,0,5\n1,2,1,4\n");
        let d = differences(&old, &moved);
        assert_eq!((d["same"].clone(), d["cells"].clone()), (false.into(), 1.into()));
        assert_eq!(d["missing_columns"], serde_json::json!(["y"]));
        assert_eq!(d["first"][0]["column"], "x");
    }
}

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
    /// NAME=VALUE[@FRAME|@A..B|@A..], run's poke by map name: input that is not the pad,
    /// shown from row FRAME+1; each written field is an activation column. Repeatable.
    #[arg(long = "poke")]
    #[serde(default, rename = "poke", skip_serializing_if = "Vec::is_empty")]
    pub pokes: Vec<String>,
    /// A column that is input rather than compared; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub activation: Vec<String>,
    /// COLUMN or COLUMN=TOLERANCE to compare; default every other column, exactly. Repeatable.
    #[arg(long)]
    #[serde(default)]
    pub compare: Vec<String>,
    /// SPEC:COLUMNS[:ROWS], what a spec claims (COLUMNS a,b or *; ROWS A..B, A.. or
    /// A..until COND, COND in until's syntax on columns, & between); repeatable.
    #[arg(long)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<String>,
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
    /// Case ids; default every recipe.
    #[serde(default)]
    pub names: Vec<String>,
    /// Directory of the case files (default specs/cases).
    #[arg(long)]
    pub cases: Option<PathBuf>,
    /// Directory of the recipes (default notes/recordings).
    #[arg(long)]
    pub recipes: Option<PathBuf>,
    /// Replace the cases that differ.
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

/// The rows whose tick rose, counted from row 0's tick column, and the
/// timing vectors: per row, the ticks completed since the row before and the
/// field they ran in and read (row r's ticks read field r-1).
fn timing(t: &Table) -> Option<Value> {
    let i = t.column("tick")?;
    let ticks: Vec<u64> = t.rows.iter().map(|r| r[i].parse().unwrap_or(0)).collect();
    let total = ticks.last().copied().unwrap_or(0);
    let first = (1..ticks.len()).find(|&r| ticks[r] > ticks[r - 1]);
    let fields = ticks.len().saturating_sub(1) as u64;
    let run: Vec<u64> =
        (0..ticks.len()).map(|r| if r == 0 { 0 } else { ticks[r].saturating_sub(ticks[r - 1]) }).collect();
    let reads: Vec<Option<usize>> = run.iter().enumerate().map(|(r, &n)| (n > 0).then(|| r - 1)).collect();
    Some(json!({
        "ticks": total,
        "first_tick": first,
        "fields_per_tick": (total > 0).then(|| (fields as f64 / total as f64).round() as u64),
        "vectors": {
            "rule": "row r's ticks_run ticks completed since row r-1; they ran during field reads_field (r-1) and read its pad",
            "ticks_run": run,
            "reads_field": reads,
        },
    }))
}

/// A cell as a number: decimal, fixed point or 0x hex.
fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok().map(|n| n as f64),
        None => s.parse().ok(),
    }
}

/// The first row from `from` where every condition (until's syntax, on the
/// table's columns) holds.
fn first_holding(t: &Table, conds: &str, from: usize) -> Result<Option<usize>, String> {
    let mut parsed = Vec::new();
    for c in conds.split('&') {
        let (name, op, want) = crate::until::split(c)?;
        let i = t.column(name).ok_or_else(|| format!("until {c:?}: {name:?} is not a column"))?;
        let want = number(want).ok_or_else(|| format!("until {c:?}: {want:?} is not a number"))?;
        parsed.push((i, op, want));
    }
    Ok((from..t.rows.len()).find(|&r| {
        parsed.iter().all(|&(i, op, want)| t.rows[r].get(i).and_then(|v| number(v)).is_some_and(|v| op.test(v, want)))
    }))
}

/// `coverage` from SPEC:COLUMNS[:ROWS]: per spec, the compared columns it
/// claims and the rows, first and last inclusive (an until's last row is the
/// first where it holds).
fn coverage(t: &Table, specs: &[String], compared: &[String]) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    let last = t.rows.len().saturating_sub(1);
    for s in specs {
        let mut parts = s.splitn(3, ':');
        let (spec, cols, rows) = (parts.next().unwrap_or("").trim(), parts.next(), parts.next());
        let cols = cols.ok_or_else(|| format!("coverage {s:?}: expected SPEC:COLUMNS[:ROWS]"))?;
        let columns: Vec<String> = match cols.trim() {
            "*" => compared.to_vec(),
            c => c.split(',').map(|c| c.trim().to_string()).collect(),
        };
        for c in &columns {
            if !compared.contains(c) {
                return Err(format!("coverage {s:?}: {c:?} is not a compared column ({})", compared.join(", ")));
            }
        }
        let mut claim = json!({"columns": columns});
        let (a, b) = match rows.map(str::trim) {
            None | Some("") => (0, last),
            Some(r) => {
                let (a, b) = r
                    .split_once("..")
                    .ok_or_else(|| format!("coverage {s:?}: rows {r:?}: expected A..B, A.. or A..until COND"))?;
                let a: usize = match a.trim() {
                    "" => 0,
                    n => n.parse().map_err(|_| format!("coverage {s:?}: row {n:?} is not a number"))?,
                };
                let b = match b.trim() {
                    "" => last,
                    u if u.starts_with("until") => {
                        let cond = u["until".len()..].trim();
                        claim["until"] = json!(cond);
                        first_holding(t, cond, a)
                            .map_err(|e| format!("coverage {s:?}: {e}"))?
                            .ok_or_else(|| format!("coverage {s:?}: {cond:?} never holds from row {a}"))?
                    }
                    n => n.parse().map_err(|_| format!("coverage {s:?}: row {n:?} is not a number"))?,
                };
                (a, b)
            }
        };
        if a > b || b > last {
            return Err(format!("coverage {s:?}: rows {a}..{b} are not within 0..{last}"));
        }
        claim["rows"] = json!([a, b]);
        if out.insert(spec.to_string(), claim).is_some() {
            return Err(format!("coverage: {spec} is claimed twice"));
        }
    }
    Ok(Value::Object(out))
}

/// JSON as serde's pretty printer writes it, but with arrays of plain
/// values (numbers, names, null) on one line, so long vectors stay short.
fn pretty(v: &Value) -> String {
    let mut s = String::new();
    write_json(v, "  ", 0, &mut s);
    s + "\n"
}

/// `pretty`'s writer, indenting by `unit`, starting `indent` levels in.
fn write_json(v: &Value, unit: &str, indent: usize, out: &mut String) {
    fn go(v: &Value, unit: &str, indent: usize, out: &mut String) {
        let pad = unit.repeat(indent + 1);
        match v {
            Value::Array(a) if a.iter().all(|x| !x.is_array() && !x.is_object()) => {
                *out += &serde_json::to_string(v).unwrap_or_default().replace(',', ", ");
            }
            Value::Array(a) if !a.is_empty() => {
                out.push_str("[\n");
                for (i, x) in a.iter().enumerate() {
                    out.push_str(&pad);
                    go(x, unit, indent + 1, out);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                *out += &format!("{}]", unit.repeat(indent));
            }
            Value::Object(m) if !m.is_empty() => {
                out.push_str("{\n");
                for (i, (k, x)) in m.iter().enumerate() {
                    *out += &format!("{pad}{}: ", Value::String(k.clone()));
                    go(x, unit, indent + 1, out);
                    out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
                }
                *out += &format!("{}}}", unit.repeat(indent));
            }
            _ => *out += &serde_json::to_string(v).unwrap_or_default(),
        }
    }
    go(v, unit, indent, out)
}

/// A case file's text with keys added at its end, in the file's own indent
/// and key order, the rest of the text as it was.
fn add_keys(text: &str, keys: &[(&str, &Value)]) -> Option<String> {
    let end = text.rfind('}')?;
    let body = text[..end].trim_end();
    // The indent of the first key, or two spaces.
    let unit = text
        .lines()
        .nth(1)
        .map(|l| &l[..l.len() - l.trim_start().len()])
        .filter(|u| !u.is_empty() && body.contains('"'))
        .unwrap_or("  ");
    let mut out = body.to_string();
    for (i, (k, v)) in keys.iter().enumerate() {
        out += if i == 0 && !body.ends_with('{') {
            ",\n"
        } else if i == 0 {
            "\n"
        } else {
            ",\n"
        };
        out += &format!("{unit}{}: ", Value::String(k.to_string()));
        write_json(v, unit, 1, &mut out);
    }
    out.push('\n');
    out += &text[end..];
    Some(out)
}

/// The case keys record writes; save keeps any others a case already has.
const CASE_KEYS: [&str; 14] = [
    "id",
    "spec",
    "role",
    "level",
    "subject",
    "start",
    "input",
    "activation",
    "observations",
    "compare",
    "fields_per_tick",
    "first_tick",
    "timing",
    "coverage",
];

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
    // Each written field is a column (the subject's own fields already are).
    let written: Vec<String> =
        a.pokes.iter().map(|p| p.split(['=', '@']).next().unwrap_or("").trim().to_string()).collect();
    let prefix = format!("{}.", a.subject);
    for w in &written {
        if !w.starts_with(&prefix) && !sample.contains(w) {
            sample.push(w.clone());
        }
    }
    let run: crate::run::RunCmdArgs = serde_json::from_value(json!({
        "media": a.media, "from_state": a.from_state, "frames": a.frames,
        "input": input, "map": a.map, "sample": sample, "poke": a.pokes,
    }))
    .map_err(|e| Error::invalid(format!("case record: {e}")))?;
    let (complete, report) = run.run(ctx)?;
    if !complete {
        return Err(Error::new("run_incomplete", format!("the run ended early ({})", report["termination"]))
            .with("run_dir", &report["run_dir"]));
    }
    // By name only: the case says what was written, never where.
    let pokes = report["pokes"].as_array().cloned().unwrap_or_default();
    if let Some(p) = pokes.iter().find(|p| p["name"].is_null()) {
        return Err(Error::invalid(format!(
            "poke {}: case record writes by field map name, so the case can say what it sets (name it in the map)",
            p["spec"]
        )));
    }
    let samples = &report["samples"];
    let file = samples["file"].as_str().map(PathBuf::from).unwrap_or_default();
    let mut t = Table::parse(&String::from_utf8_lossy(&read(&file)?));
    // The subject's fields by their own names; the case names its subject.
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
    let mut activation = a.activation.clone();
    for w in &written {
        let c = w.strip_prefix(&prefix).unwrap_or(w).to_string();
        if !activation.contains(&c) {
            activation.push(c);
        }
    }
    for c in &activation {
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
        for c in columns.iter().filter(|c| !activation.contains(c)) {
            compare.insert(c.to_string(), json!(0));
        }
    }
    let mut warnings = Vec::new();
    for c in compare.keys().filter(|c| activation.contains(c)) {
        warnings
            .push(format!("{c} is written (an activation) and compared: compare it only if the game changes it too"));
    }
    for p in pokes.iter().filter(|p| !p["skipped"].is_null()) {
        warnings.push(format!(
            "poke {} wrote nothing on {} run frames (from {}): its pointer was null or led outside memory",
            p["spec"], p["skipped"], p["first_skipped_run_frame"]
        ));
    }
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
        case["timing"] = v["vectors"].clone();
    }
    if !activation.is_empty() {
        case["activation"] = json!(activation);
    }
    let compared: Vec<String> = compare.keys().cloned().collect();
    if !a.coverage.is_empty() {
        let c = coverage(&t, &a.coverage, &compared).map_err(Error::invalid)?;
        let claims = |col: &str| {
            c.as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .any(|v| v["columns"].as_array().is_some_and(|a| a.iter().any(|x| x == col)))
        };
        let unclaimed: Vec<&str> = compared.iter().map(String::as_str).filter(|col| !claims(col)).collect();
        if !unclaimed.is_empty() {
            warnings.push(format!(
                "no spec's coverage claims {}: claim them or say which spec they wait for",
                unclaimed.join(", ")
            ));
        }
        case["coverage"] = c;
    }
    let csv = t.text();
    // The recipe: these arguments with the input inlined, and what they ran on.
    let mut args = a.clone();
    (args.replay, args.cases, args.recipes, args.replace) = (None, None, None, false);
    // Paths in the project relative to it, so the project can move.
    let store = |p: &Path| crate::moved::store(&ctx.root, p);
    args.from_state = store(&args.from_state);
    args.map = args.map.as_deref().map(store);
    args.media = args.media.as_deref().map(store);
    args.input = serde_json::from_value(input).ok();
    let id = &report["identity"];
    let state = read(&a.from_state)?;
    let recipe = json!({
        "case": a.name,
        "args": args,
        "recorded_with": crate::context::version(),
        "emulator": crate::context::emulator(),
        "state_sha256": sha256(&state),
        "map": samples["map"]["path"].as_str().map(|p| store(Path::new(p))),
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
    let (case, csv, recipe) =
        (cases.join(format!("{name}.json")), cases.join(format!("{name}.csv")), recipes.join(format!("{name}.json")));
    // Keys someone added to the case by hand (notes, say) stay.
    let mut new = r.case.clone();
    if let Ok(Value::Object(old)) = std::fs::read(&case).map(|b| serde_json::from_slice(&b).unwrap_or(Value::Null)) {
        for (k, v) in old {
            if !CASE_KEYS.contains(&k.as_str()) {
                new[k] = v;
            }
        }
    }
    write(&csv, r.csv.as_bytes())?;
    write(&case, pretty(&new).as_bytes())?;
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
                    let mut args: CaseRecordArgs = serde_json::from_value(recipe["args"].clone())
                        .map_err(|e| Error::malformed(format!("recipe {name}: args: {e}")))?;
                    // A recipe from before the project moved names its old folder.
                    let root = &ctx.root;
                    args.from_state = crate::moved::find(root, &args.from_state);
                    args.map = args.map.map(|p| crate::moved::find(root, &p));
                    args.media = args.media.map(|p| crate::moved::find(root, &p));
                    let mut warnings = Vec::new();
                    if recipe["state_sha256"].as_str() != Some(&sha256(&read(&args.from_state)?)) {
                        warnings.push("the start state changed since the case was recorded");
                    }
                    let r = record(ctx, &args)?;
                    let old = Table::parse(&String::from_utf8_lossy(&read(&cases.join(format!("{name}.csv")))?));
                    let mut d = differences(&old, &Table::parse(&r.csv));
                    let case: Value = serde_json::from_slice(&read(&cases.join(format!("{name}.json")))?)
                        .map_err(|e| Error::malformed(format!("case {name}: {e}")))?;
                    let keys = ["fields_per_tick", "first_tick", "input", "subject", "timing", "coverage"];
                    // A key this psx writes that the published case lacks
                    // (made by an older psx) is added, not a difference.
                    let added: Vec<&str> =
                        keys.into_iter().filter(|k| case.get(k).is_none() && !r.case[k].is_null()).collect();
                    let changed: Vec<&str> =
                        keys.into_iter().filter(|k| !added.contains(k) && case[k] != r.case[k]).collect();
                    d["same"] = json!(d["same"] == true && changed.is_empty());
                    d["case_keys_changed"] = json!(changed);
                    d["warnings"] = json!(warnings);
                    if !added.is_empty() {
                        d["case_keys_to_add"] = json!(added);
                    }
                    // When and on which emulator it was last checked, and whether
                    // it held (psx protocol next reads this).
                    let mut recipe = recipe;
                    // Paths in the project, written back relative to it.
                    for k in ["from_state", "map", "media"] {
                        if let Some(p) = recipe["args"][k].as_str().map(|p| crate::moved::find(&ctx.root, Path::new(p)))
                        {
                            recipe["args"][k] = json!(crate::moved::store(&ctx.root, &p));
                        }
                    }
                    recipe["checked"] = json!({"emulator": crate::context::emulator(), "time": crate::publish::utc_now(),
                        "same": d["same"] == true});
                    write(&recipes.join(format!("{name}.json")), pretty(&recipe).as_bytes())?;
                    if self.write && d["same"] != true {
                        d["written"] = save(&r, name, &cases, &recipes)?;
                    } else if self.write && !added.is_empty() {
                        // The cells match: add the keys and leave the files
                        // as they are otherwise.
                        let path = cases.join(format!("{name}.json"));
                        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
                        let new: Vec<(&str, &Value)> = added.iter().map(|k| (*k, &r.case[*k])).collect();
                        let out = add_keys(&text, &new)
                            .ok_or_else(|| Error::malformed(format!("case {name}: not a JSON object")))?;
                        write(&path, out.as_bytes())?;
                        d["written"] = json!({"case": path, "added": added});
                    }
                    Ok(d)
                };
                let mut v = one().unwrap_or_else(|e| json!({"same": false, "error": e.to_value()["error"]}));
                if v["same"] == true {
                    same += 1;
                    v = json!({"same": true, "case_keys_to_add": v.get("case_keys_to_add"), "written": v.get("written")});
                    v.as_object_mut().into_iter().for_each(|m| m.retain(|_, x| !x.is_null()));
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
    use super::{Table, add_keys, coverage, differences, pretty, timing};
    use serde_json::json;

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
    fn timing_vectors_give_each_rows_ticks_and_the_field_they_read() {
        // The brief's example: tick 0, 2, 2, 2, 3 runs 2, 0, 0, 1 ticks on
        // rows 1 to 4, reading fields 0, -, -, 3.
        let t = Table::parse("run_frame,frame,tick\n0,0,0\n1,1,2\n2,2,2\n3,3,2\n4,4,3\n");
        let v = &timing(&t).unwrap()["vectors"];
        assert_eq!(v["ticks_run"], json!([0, 2, 0, 0, 1]));
        assert_eq!(v["reads_field"], json!([null, 0, null, null, 3]));
    }

    #[test]
    fn coverage_resolves_rows_and_checks_columns() {
        let t = Table::parse("run_frame,frame,tick,x,state\n0,0,0,1,0\n1,1,1,2,0\n2,2,2,3,2\n3,3,3,3,2\n");
        let cols = vec!["x".to_string(), "state".to_string()];
        let c = coverage(&t, &["SPEC-1:x:0..until state=2".into(), "SPEC-2:*:2..".into()], &cols).unwrap();
        assert_eq!(c["SPEC-1"], json!({"columns": ["x"], "rows": [0, 2], "until": "state=2"}));
        assert_eq!(c["SPEC-2"], json!({"columns": ["x", "state"], "rows": [2, 3]}));
        assert_eq!(coverage(&t, &["S:x".into()], &cols).unwrap()["S"]["rows"], json!([0, 3]));
        assert_eq!(coverage(&t, &["S:x:1..until x>=3&state!=0".into()], &cols).unwrap()["S"]["rows"], json!([1, 2]));
        let err = |s: &str| coverage(&t, &[s.into()], &cols).unwrap_err();
        assert!(err("S:y").contains("not a compared column"));
        assert!(err("S:x:0..until state=9").contains("never holds"));
        assert!(err("S:x:0..9").contains("not within"));
        assert!(err("S").contains("SPEC:COLUMNS"));
    }

    #[test]
    fn added_keys_keep_the_files_layout() {
        let text = "{\n \"id\": \"a\",\n \"input\": [\n  {\n   \"frame\": 0\n  }\n ]\n}";
        let t = json!({"ticks_run": [0, 1], "rule": "r"});
        let out = add_keys(text, &[("timing", &t)]).unwrap();
        assert_eq!(
            out,
            "{\n \"id\": \"a\",\n \"input\": [\n  {\n   \"frame\": 0\n  }\n ],\n \"timing\": {\n  \"rule\": \"r\",\n  \"ticks_run\": [0, 1]\n }\n}"
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!((v["id"].clone(), v["timing"].clone()), (json!("a"), t.clone()));
        assert_eq!(
            add_keys("{}\n", &[("timing", &t)]).unwrap(),
            "{\n  \"timing\": {\n    \"rule\": \"r\",\n    \"ticks_run\": [0, 1]\n  }\n}\n"
        );
    }

    #[test]
    fn pretty_keeps_plain_arrays_on_one_line() {
        let v = json!({"a": [1, null, 2], "b": [{"x": ["L", "R"]}], "c": {}, "d": []});
        let s = pretty(&v);
        assert_eq!(
            s,
            "{\n  \"a\": [1, null, 2],\n  \"b\": [\n    {\n      \"x\": [\"L\", \"R\"]\n    }\n  ],\n  \"c\": {},\n  \"d\": []\n}\n"
        );
        assert_eq!(serde_json::from_str::<serde_json::Value>(&s).unwrap(), v);
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

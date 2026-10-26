//! Case files: recordings of the original as `psx case record` writes them
//! (`NAME.json` and its observations, `NAME.csv`), replayed in the rebuild
//! and compared row by row.
//!
//! A game writes one runner per subject: it spawns the subject from the
//! case's start and reads each compared column back in the case's units.
//! [`Case::runner`] does the rest, so each case is then one line:
//!
//! ```
//! use bevy::prelude::*;
//! use psx_bevy::prelude::*;
//! use psx_bevy::testing::{Case, headless_app};
//!
//! #[derive(Component)]
//! struct Pos(f64);
//!
//! fn walk(pad: Res<ControllerState>, mut q: Query<&mut Pos>) {
//!     for mut p in &mut q {
//!         p.0 += pad.direction().x as f64;
//!     }
//! }
//!
//! // In a test: Case::load("specs/cases/walk.json").
//! let case = Case::parse(
//!     "walk",
//!     r#"{"id": "walk", "spec": "DEMO-0001", "role": "heldout", "subject": "pos",
//!         "start": {"from": "first_row"}, "input": [{"frame": 0, "buttons": ["RIGHT"], "until": 2}],
//!         "observations": "walk.csv", "compare": {"x": 0}}"#,
//!     "run_frame,frame,tick,x\n0,100,0,5\n1,101,1,6\n2,102,2,7\n3,103,3,7\n",
//! )
//! .unwrap();
//! let mut app = headless_app(TickRate::every_field(Region::Ntsc));
//! app.add_systems(FixedUpdate, walk.in_set(GameSystems::Game));
//! let pos = app.world_mut().spawn(Pos(case.first("x"))).id();
//! let report = case.runner().run(&mut app, |world, column| match column {
//!     "x" => Some(world.get::<Pos>(pos)?.0),
//!     _ => None,
//! });
//! report.assert_passed();
//! ```
//!
//! Timing, as `psx case record` writes it: row 0 is the subject before any
//! tick, and the `tick` column counts the ticks completed since row 0. Before
//! row `r` the runner runs `tick[r] - tick[r - 1]` ticks (none on a repeated
//! row, two when the game caught up), each with the pad held during field
//! `r - 1`. It checks the ticks it ran against the case's `timing` vectors
//! before judging any behaviour. A case without a `tick` column (from an
//! older psx) runs from `first_tick` and `fields_per_tick`.
//!
//! Coverage: a case's `coverage` names, per spec, the columns and rows it
//! claims. [`CaseRun::implementing`] lists the specs the rebuild implements;
//! the runner compares only their claims, and reports the rest as waiting
//! for their specs. Without `coverage` the whole case belongs to its `spec`
//! (an exploratory case without one is always compared). A `diagnostic` case
//! is reported but never fails.
use crate::testing::trajectory::{Comparison, Replay, Trajectory, compare_from};
use crate::testing::{TickApp, TickInput};
use bevy::prelude::*;
use serde_json::Value;
use std::fmt;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// One case: its JSON and its observations.
#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    json: Value,
    header: Vec<String>,
    /// Every observed row (one per field); NaN for a cell that is not a number.
    rows: Vec<Vec<f64>>,
    /// The rows as recorded, before any [`Case::truncate`].
    recorded: usize,
}

/// Where a case starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The subject as row 0 shows it: set every field from [`Case::first`].
    FirstRow,
    /// A published start state, `specs/states/NAME.json`, which describes
    /// how to reach it in game terms.
    State(String),
}

/// What one spec claims of a case: these columns over these rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// `None` for an exploratory case without a spec.
    pub spec: Option<String>,
    pub columns: Vec<String>,
    pub rows: RangeInclusive<usize>,
}

impl fmt::Display for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spec = self.spec.as_deref().unwrap_or("the case");
        write!(f, "{spec}: {} over rows {}..={}", self.columns.join(", "), self.rows.start(), self.rows.end())
    }
}

/// A field the case's recording wrote (case record's poke): input that is
/// not the pad, set at the start of each field in `from..=to` (no `to`: to
/// the end), so the ticks of those fields see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub column: String,
    pub value: f64,
    pub from: u64,
    pub to: Option<u64>,
}

impl Written {
    pub fn due(&self, field: u64) -> bool {
        field >= self.from && self.to.is_none_or(|t| field <= t)
    }
}

/// The ticks the runner runs before one row, and the field whose pad they read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowTicks {
    pub ticks: u64,
    pub reads_field: Option<u64>,
}

impl Case {
    /// Reads `NAME.json` (the `.json` may be left off) and the observations
    /// it names, beside it.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let mut path = path.as_ref().to_path_buf();
        if path.extension().is_none_or(|e| e != "json") {
            path.as_mut_os_string().push(".json");
        }
        let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
        let text = read(&path)?;
        let json: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let csv = json["observations"].as_str().ok_or(format!("{}: no observations", path.display()))?;
        let csv = read(&path.with_file_name(csv))?;
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        Self::parse(&name, &text, &csv).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A case from its JSON and CSV text.
    pub fn parse(name: &str, json: &str, csv: &str) -> Result<Self, String> {
        let json: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if !json.is_object() {
            return Err("a case is a JSON object".into());
        }
        let mut lines = csv.lines().filter(|l| !l.trim().is_empty());
        let header: Vec<String> = lines.next().ok_or("empty observations")?.split(',').map(String::from).collect();
        let mut rows = Vec::new();
        for (i, line) in lines.enumerate() {
            let row: Vec<f64> = line.split(',').map(|c| number(c).unwrap_or(f64::NAN)).collect();
            if row.len() != header.len() {
                return Err(format!("observation row {i}: {} cells for {} columns", row.len(), header.len()));
            }
            rows.push(row);
        }
        if rows.is_empty() {
            return Err("no observation rows".into());
        }
        let recorded = rows.len();
        let case = Self { name: name.into(), json, header, rows, recorded };
        for (column, _) in case.compared()? {
            if !case.has(&column) {
                return Err(format!("compare names {column:?}, which the observations do not have"));
            }
        }
        Ok(case)
    }

    /// The whole case JSON, for keys a game adds (a level, an activation).
    pub fn json(&self) -> &Value {
        &self.json
    }

    pub fn spec(&self) -> Option<&str> {
        self.json["spec"].as_str()
    }

    /// heldout, calibration, diagnostic or exploratory.
    pub fn role(&self) -> &str {
        self.json["role"].as_str().unwrap_or("")
    }

    /// The subject: a name, or `{name, index, type}` for an element of an array.
    pub fn subject(&self) -> &Value {
        &self.json["subject"]
    }

    pub fn level(&self) -> Option<&str> {
        self.json["level"].as_str()
    }

    pub fn start(&self) -> Result<Start, String> {
        let start = &self.json["start"];
        match (start["from"].as_str(), start["state"].as_str()) {
            (Some("first_row") | None, None) => Ok(Start::FirstRow),
            (Some("state") | None, Some(name)) => Ok(Start::State(name.into())),
            _ => Err(format!("{}: start {start} is neither from first_row nor a state", self.name)),
        }
    }

    /// The case's input, field by field.
    pub fn input(&self) -> Result<Replay, String> {
        Replay::parse(&self.json["input"].to_string()).map_err(|e| format!("{}: input: {e}", self.name))
    }

    /// The columns that are input rather than observation (an object's
    /// activation): give them to the rebuild in [`CaseRun::before_tick`].
    pub fn activation(&self) -> Vec<&str> {
        match &self.json["activation"] {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        }
    }

    /// The fields the recording wrote, with the values it wrote: a row shows
    /// a field after the tick, so these, not the rows, say what was given.
    /// [`CaseRun::write`] gives them to the rebuild.
    pub fn writes(&self) -> Result<Vec<Written>, String> {
        let bad = |w: &Value| format!("{}: write {w} is not {{column, value, fields: {{from, to}}}}", self.name);
        let list = match &self.json["writes"] {
            Value::Null => return Ok(Vec::new()),
            Value::Array(a) => a,
            w => return Err(bad(w)),
        };
        list.iter()
            .map(|w| {
                Ok(Written {
                    column: w["column"].as_str().ok_or_else(|| bad(w))?.to_string(),
                    value: w["value"].as_f64().ok_or_else(|| bad(w))?,
                    from: w["fields"]["from"].as_u64().ok_or_else(|| bad(w))?,
                    to: w["fields"]["to"].as_u64(),
                })
            })
            .collect()
    }

    /// The compared columns with their tolerances, in the case's order.
    pub fn compared(&self) -> Result<Vec<(String, f64)>, String> {
        let compare = self.json["compare"].as_object().ok_or(format!("{}: no compare map", self.name))?;
        compare
            .iter()
            .map(|(k, v)| Ok((k.clone(), v.as_f64().ok_or(format!("{}: compare {k:?} is not a number", self.name))?)))
            .collect()
    }

    /// The observations' columns.
    pub fn columns(&self) -> &[String] {
        &self.header
    }

    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    pub fn has(&self, column: &str) -> bool {
        self.header.iter().any(|c| c == column)
    }

    pub fn get(&self, row: usize, column: &str) -> Option<f64> {
        let i = self.header.iter().position(|c| c == column)?;
        self.rows.get(row).map(|r| r[i])
    }

    /// A cell, panicking with the case's name if it is not there.
    #[track_caller]
    pub fn value(&self, row: usize, column: &str) -> f64 {
        self.get(row, column).unwrap_or_else(|| {
            panic!("{}: no {column:?} at row {row} (columns {:?}, {} rows)", self.name, self.header, self.rows.len())
        })
    }

    /// Row 0's value: the subject's starting state.
    #[track_caller]
    pub fn first(&self, column: &str) -> f64 {
        self.value(0, column)
    }

    /// Only the first `rows` rows, for a case whose later rows need a spec
    /// with no coverage to say so. Claims are cut to fit.
    pub fn truncate(&mut self, rows: usize) {
        self.rows.truncate(rows.max(1));
    }

    /// Ticks completed by each row: the `tick` column, or steady play from
    /// `first_tick` and `fields_per_tick`.
    pub fn ticks_by_row(&self) -> Result<Vec<u64>, String> {
        if self.has("tick") {
            let ticks: Vec<u64> = (0..self.rows.len()).map(|r| self.value(r, "tick") as u64).collect();
            if let Some(r) = (1..ticks.len()).find(|&r| ticks[r] < ticks[r - 1]) {
                return Err(format!("{}: tick goes down at row {r}", self.name));
            }
            return Ok(ticks.iter().map(|t| t - ticks[0]).collect());
        }
        let (first, every) = (self.json["first_tick"].as_u64(), self.json["fields_per_tick"].as_u64());
        let (Some(first), Some(every)) = (first, every.filter(|&e| e > 0)) else {
            return Err(format!("{}: no tick column, and no first_tick and fields_per_tick", self.name));
        };
        Ok((0..self.rows.len() as u64).map(|r| if r < first { 0 } else { (r - first) / every + 1 }).collect())
    }

    /// The runner's schedule: per row, the ticks run since the row before and
    /// the field they read (row `r`'s read field `r - 1`).
    pub fn schedule(&self) -> Result<Vec<RowTicks>, String> {
        let ticks = self.ticks_by_row()?;
        Ok((0..ticks.len())
            .map(|r| {
                let n = if r == 0 { 0 } else { ticks[r] - ticks[r - 1] };
                RowTicks { ticks: n, reads_field: (n > 0).then(|| r as u64 - 1) }
            })
            .collect())
    }

    /// Checks ticks the runner ran (one entry per row) against the case's
    /// timing vectors. A case without them (from an older psx) is checked
    /// against its own tick column.
    pub fn check_timing(&self, ran: &[RowTicks]) -> Result<(), String> {
        let timing = &self.json["timing"];
        let expected: Vec<RowTicks> = match (timing["ticks_run"].as_array(), timing["reads_field"].as_array()) {
            (Some(run), Some(reads)) => {
                if run.len() != self.recorded || reads.len() != self.recorded {
                    return Err(format!(
                        "{}: timing has {} and {} entries for {} rows",
                        self.name,
                        run.len(),
                        reads.len(),
                        self.recorded
                    ));
                }
                run.iter()
                    .zip(reads)
                    .map(|(n, f)| RowTicks { ticks: n.as_u64().unwrap_or(0), reads_field: f.as_u64() })
                    .collect()
            }
            _ => self.schedule()?,
        };
        let expected = &expected[..ran.len().min(expected.len())];
        if let Some(r) = (0..expected.len()).find(|&r| expected[r] != ran[r]) {
            return Err(format!(
                "{}: runner timing differs from the case at row {r}: ran {} tick(s) reading field {:?}, the case's timing says {} reading {:?}. Fix the runner before any behaviour",
                self.name, ran[r].ticks, ran[r].reads_field, expected[r].ticks, expected[r].reads_field
            ));
        }
        Ok(())
    }

    /// Each spec's claim, the compared columns no spec claims, in the case's
    /// order. Rows beyond the case (after [`Case::truncate`]) are cut off.
    pub fn claims(&self) -> Result<(Vec<Claim>, Vec<String>), String> {
        let compared: Vec<String> = self.compared()?.into_iter().map(|(c, _)| c).collect();
        let last = self.rows.len() - 1;
        let Some(coverage) = self.json.get("coverage").and_then(Value::as_object) else {
            let spec = self.spec().map(String::from);
            return Ok((vec![Claim { spec, columns: compared, rows: 0..=last }], Vec::new()));
        };
        let mut claims = Vec::new();
        for (spec, claim) in coverage {
            let bad = |what: &str| format!("{}: coverage {spec}: {what}", self.name);
            let columns: Vec<String> = claim["columns"]
                .as_array()
                .ok_or_else(|| bad("no columns"))?
                .iter()
                .map(|c| c.as_str().map(String::from).ok_or_else(|| bad("a column is not a name")))
                .collect::<Result<_, _>>()?;
            if let Some(c) = columns.iter().find(|c| !compared.contains(c)) {
                return Err(bad(&format!("{c:?} is not compared")));
            }
            let rows =
                claim["rows"].as_array().map(|r| (r.first().and_then(Value::as_u64), r.get(1).and_then(Value::as_u64)));
            let (a, b) = match rows {
                Some((Some(a), Some(b))) if a <= b => (a as usize, b as usize),
                None => (0, last),
                _ => return Err(bad("rows is not [first, last]")),
            };
            if a <= last {
                claims.push(Claim { spec: Some(spec.clone()), columns, rows: a..=b.min(last) });
            }
        }
        let unclaimed = compared
            .into_iter()
            .filter(|c| !coverage.values().any(|v| v["columns"].as_array().is_some_and(|a| a.iter().any(|x| x == c))))
            .collect();
        Ok((claims, unclaimed))
    }

    /// The case's runner: [`CaseRun::run`] replays it in an app.
    pub fn runner(&self) -> CaseRun<'_> {
        CaseRun { case: self, implementing: None, before: None, write: None }
    }
}

/// A cell as a number: decimal or 0x hex.
fn number(cell: &str) -> Option<f64> {
    let cell = cell.trim();
    match cell.strip_prefix("0x").or_else(|| cell.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok().map(|v| v as f64),
        None => cell.parse().ok(),
    }
}

type BeforeTick<'a> = Box<dyn FnMut(&mut App, usize) + 'a>;
type WriteField<'a> = Box<dyn FnMut(&mut App, &str, f64) + 'a>;

/// How to replay a case: which specs the rebuild implements, and what to do
/// before each tick.
pub struct CaseRun<'a> {
    case: &'a Case,
    implementing: Option<Vec<String>>,
    before: Option<BeforeTick<'a>>,
    write: Option<WriteField<'a>>,
}

impl<'a> CaseRun<'a> {
    /// The specs the rebuild implements: only their claims are compared, and
    /// the rest are reported as waiting. Without it every claim is compared.
    pub fn implementing(mut self, specs: &[&str]) -> Self {
        self.implementing = Some(specs.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Called before each tick with the row the tick ends on, for input that
    /// is not the pad: `case.value(row, column)` for each of
    /// [`Case::activation`]'s columns.
    pub fn before_tick(mut self, f: impl FnMut(&mut App, usize) + 'a) -> Self {
        self.before = Some(Box::new(f));
        self
    }

    /// Sets a column the recording wrote ([`Case::writes`]) to a value, in
    /// the case's units: called before the first tick that sees the write
    /// (a write in a field with no tick waits for the next one). Needed for
    /// a case with writes.
    pub fn write(mut self, f: impl FnMut(&mut App, &str, f64) + 'a) -> Self {
        self.write = Some(Box::new(f));
        self
    }

    /// Replays the case in `app`, whose world already holds the subject at
    /// its start, reading each compared column with `read` (in the case's
    /// units; `None` for a column the rebuild cannot read, which panics with
    /// its name). Replays only as far as the last compared row.
    pub fn run(mut self, app: &mut App, mut read: impl FnMut(&World, &str) -> Option<f64>) -> CaseReport {
        let case = self.case;
        let tolerances = case.compared().unwrap_or_else(|e| panic!("{e}"));
        let (claims, unclaimed) = case.claims().unwrap_or_else(|e| panic!("{e}"));
        let implemented = |c: &Claim| match (&self.implementing, &c.spec) {
            (Some(specs), Some(spec)) => specs.contains(spec),
            _ => true,
        };
        let (compared, waiting): (Vec<Claim>, Vec<Claim>) = claims.into_iter().partition(implemented);
        let columns: Vec<&str> = tolerances
            .iter()
            .map(|(c, _)| c.as_str())
            .filter(|c| compared.iter().any(|claim| claim.columns.iter().any(|x| x == c)))
            .collect();
        let last = compared.iter().map(|c| *c.rows.end()).max();
        let schedule = case.schedule().unwrap_or_else(|e| panic!("{e}"));
        let replay = case.input().unwrap_or_else(|e| panic!("{e}"));
        let writes = case.writes().unwrap_or_else(|e| panic!("{e}"));
        if !writes.is_empty() && self.write.is_none() {
            let columns: Vec<&str> = writes.iter().map(|w| w.column.as_str()).collect();
            panic!("{}: the recording wrote {columns:?}: give them to the rebuild with CaseRun::write", case.name);
        }
        // Written since the last tick, by column (the latest value wins).
        let mut pending: Vec<(&str, f64)> = Vec::new();

        let read_row = |world: &World, read: &mut dyn FnMut(&World, &str) -> Option<f64>| -> Vec<f64> {
            columns
                .iter()
                .map(|&c| {
                    read(world, c).unwrap_or_else(|| {
                        panic!(
                            "{}: the rebuild cannot read column {c:?}: add it to the case runner's reader",
                            case.name
                        )
                    })
                })
                .collect()
        };
        let mut rebuild = Trajectory::new(&columns);
        let mut ran = vec![RowTicks { ticks: 0, reads_field: None }];
        let mut ticks = vec![0u64];
        if last.is_some() {
            rebuild.push(read_row(app.world(), &mut read));
        }
        for row in 1..=last.unwrap_or(0) {
            let mut done = RowTicks { ticks: 0, reads_field: None };
            for w in writes.iter().filter(|w| w.due(row as u64 - 1)) {
                pending.retain(|(c, _)| *c != w.column);
                pending.push((&w.column, w.value));
            }
            for _ in 0..schedule[row].ticks {
                // The ticks before row r ran during field r - 1 and read its pad.
                let field = row as u64 - 1;
                if let Some(write) = self.write.as_mut() {
                    for (column, value) in pending.drain(..) {
                        write(app, column, value);
                    }
                }
                if let Some(before) = self.before.as_mut() {
                    before(app, row);
                }
                app.run_input(&TickInput { buttons: replay.held(field).to_vec(), mouse: replay.mouse(field) });
                done = RowTicks { ticks: done.ticks + 1, reads_field: Some(field) };
            }
            ran.push(done);
            ticks.push(ticks[row - 1] + done.ticks);
            rebuild.push(read_row(app.world(), &mut read));
        }
        let timing = case.check_timing(&ran);

        let mut original = Trajectory::new(&columns);
        for row in 0..rebuild.ticks() {
            original.push(columns.iter().map(|c| case.value(row, c)).collect());
        }
        let comparisons = compared
            .into_iter()
            .map(|claim| {
                let cut = |t: &Trajectory| {
                    let mut out = Trajectory::new(&claim.columns.iter().map(String::as_str).collect::<Vec<_>>());
                    for row in 0..=*claim.rows.end() {
                        out.push(claim.columns.iter().map(|c| t.get(row, c).unwrap_or(f64::NAN)).collect());
                    }
                    out
                };
                let limits: Vec<(&str, f64)> = tolerances
                    .iter()
                    .filter(|(c, _)| claim.columns.contains(c))
                    .map(|(c, t)| (c.as_str(), *t))
                    .collect();
                let comparison = compare_from(&cut(&original), &cut(&rebuild), &limits, *claim.rows.start())
                    .unwrap_or_else(|e| panic!("{e}"));
                (claim, comparison)
            })
            .collect();
        let frames = (0..rebuild.ticks()).map(|r| case.get(r, "frame").unwrap_or(r as f64)).collect();
        CaseReport {
            name: case.name.clone(),
            json: case.json.clone(),
            diagnostic: case.role() == "diagnostic",
            timing,
            compared: comparisons,
            waiting,
            unclaimed,
            rebuild,
            frames,
            ticks,
        }
    }
}

/// What a case's replay found. Its `Display` is a report for test output.
#[derive(Debug, Clone)]
pub struct CaseReport {
    pub name: String,
    json: Value,
    /// A diagnostic case is reported, never failed.
    pub diagnostic: bool,
    /// The runner's own check against the case's timing vectors.
    pub timing: Result<(), String>,
    pub compared: Vec<(Claim, Comparison)>,
    /// Claims of specs the rebuild does not implement yet.
    pub waiting: Vec<Claim>,
    /// Compared columns no spec's coverage claims.
    pub unclaimed: Vec<String>,
    rebuild: Trajectory,
    frames: Vec<f64>,
    ticks: Vec<u64>,
}

impl CaseReport {
    /// The timing check and every compared claim passed. A diagnostic case
    /// passes whatever it found.
    pub fn passed(&self) -> bool {
        self.diagnostic || (self.timing.is_ok() && self.compared.iter().all(|(_, c)| c.passed()))
    }

    /// Prints what is waiting and panics with the full report unless
    /// [`CaseReport::passed`].
    #[track_caller]
    pub fn assert_passed(&self) {
        if !self.waiting.is_empty() || !self.unclaimed.is_empty() || self.compared.is_empty() || self.diagnostic {
            eprintln!("{self}");
        }
        assert!(self.passed(), "case mismatch\n{self}");
    }

    /// Per spec compared, whether its claims held (and the timing check).
    pub fn verdicts(&self) -> Vec<(&str, bool)> {
        let mut out: Vec<(&str, bool)> = Vec::new();
        for (claim, comparison) in &self.compared {
            let Some(spec) = claim.spec.as_deref() else { continue };
            let ok = self.timing.is_ok() && comparison.passed();
            match out.iter_mut().find(|(s, _)| *s == spec) {
                Some(v) => v.1 &= ok,
                None => out.push((spec, ok)),
            }
        }
        out
    }

    /// The rebuild's run in the case's own form, for comparing with the
    /// original outside the tests: `NAME.csv` (run_frame, frame, tick and
    /// the compared columns, as far as the replay went) and `NAME.json` (the
    /// case's JSON, pointing at it), in `dir`; and its verdict per spec,
    /// `NAME.result.json` ([`write_result`]), which `psx protocol next`
    /// reads for the specs the rebuild passes.
    pub fn write_run(&self, dir: impl AsRef<Path>) -> Result<PathBuf, String> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let kind = match self.json["role"].as_str() {
            Some("diagnostic") => "diagnostic",
            Some("corpus") => "corpus",
            _ => "case",
        };
        write_result(dir, &self.name, kind, &self.verdicts())?;
        let mut csv = format!("run_frame,frame,tick,{}\n", self.rebuild.columns().join(","));
        for row in 0..self.rebuild.ticks() {
            let cells: Vec<String> = self
                .rebuild
                .columns()
                .iter()
                .map(|c| self.rebuild.get(row, c).unwrap_or(f64::NAN).to_string())
                .collect();
            csv += &format!("{row},{},{},{}\n", self.frames[row], self.ticks[row], cells.join(","));
        }
        let mut json = self.json.clone();
        json["observations"] = format!("{}.csv", self.name).into();
        json["recorded_from"] = "rebuild".into();
        let write = |path: PathBuf, text: String| {
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display())).map(|_| path)
        };
        write(dir.join(format!("{}.csv", self.name)), csv)?;
        let text = serde_json::to_string_pretty(&json).unwrap_or_default() + "\n";
        write(dir.join(format!("{}.json", self.name)), text)
    }
}

/// A test's verdict per spec, `NAME.result.json` in `dir`: what `psx
/// protocol next` counts as a spec verified (kind `case`: its cases pass)
/// and integrated (`corpus`: whole recorded play-throughs replay;
/// `round_trip`: a format reads and writes back byte for byte). Write one
/// from any test that is evidence for a spec, in the target's temporary
/// folder (`env!("CARGO_TARGET_TMPDIR")`), where psx looks.
pub fn write_result(dir: impl AsRef<Path>, name: &str, kind: &str, specs: &[(&str, bool)]) -> Result<PathBuf, String> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let specs: serde_json::Map<String, Value> = specs.iter().map(|(s, ok)| (s.to_string(), Value::Bool(*ok))).collect();
    let v = serde_json::json!({"name": name, "kind": kind, "time": time, "specs": specs});
    let path = dir.join(format!("{name}.result.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default() + "\n")
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

impl fmt::Display for CaseReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verdict = if self.passed() { "pass" } else { "FAIL" };
        let role = if self.diagnostic { " (diagnostic: reported, never failed)" } else { "" };
        writeln!(f, "case {}: {verdict}{role}", self.name)?;
        if let Err(e) = &self.timing {
            writeln!(f, "timing: {e}")?;
        }
        if self.compared.is_empty() {
            writeln!(f, "nothing compared: every claim waits for a spec")?;
        }
        for (claim, comparison) in &self.compared {
            writeln!(f, "{claim}")?;
            for line in comparison.to_string().lines() {
                writeln!(f, "  {line}")?;
            }
        }
        for claim in &self.waiting {
            writeln!(f, "waiting for {claim}")?;
        }
        if !self.unclaimed.is_empty() {
            writeln!(f, "claimed by no spec: {}", self.unclaimed.join(", "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Button, ControllerState};
    use crate::prelude::{GameSystems, Region, TickRate};
    use crate::testing::headless_app;

    /// A counter that rises by one each tick Right is held.
    #[derive(Component)]
    struct Count(f64);

    fn count(pad: Res<ControllerState>, mut q: Query<&mut Count>) {
        for mut c in &mut q {
            if pad.held(Button::Right) {
                c.0 += 1.0;
            }
        }
    }

    // Tick 0, 2, 2, 2, 3, 4: a catch-up, two repeats, then steady. Right is
    // held during fields 0 and 3 only.
    const CSV: &str =
        "run_frame,frame,tick,x,y\n0,50,0,0,9\n1,51,2,2,9\n2,52,2,2,9\n3,53,2,2,9\n4,54,3,3,9\n5,55,4,3,1\n";

    fn case(extra: &str) -> Case {
        let json = format!(
            r#"{{"id": "c", "spec": "SPEC-1", "role": "heldout", "subject": "s", "start": {{"from": "first_row"}},
                "input": [{{"frame": 0, "buttons": ["RIGHT"], "until": 1}}, {{"frame": 3, "buttons": ["RIGHT"], "until": 4}}],
                "observations": "c.csv", "compare": {{"x": 0, "y": 0}},
                "timing": {{"ticks_run": [0, 2, 0, 0, 1, 1], "reads_field": [null, 0, null, null, 3, 4]}}{extra}}}"#
        );
        Case::parse("c", &json, CSV).unwrap()
    }

    fn run(case: &Case, specs: Option<&[&str]>, y: f64) -> CaseReport {
        let mut app = headless_app(TickRate::every_field(Region::Ntsc));
        app.add_systems(FixedUpdate, count.in_set(GameSystems::Game));
        let e = app.world_mut().spawn(Count(case.first("x"))).id();
        let mut runner = case.runner();
        if let Some(specs) = specs {
            runner = runner.implementing(specs);
        }
        runner.run(&mut app, |w, c| match c {
            "x" => Some(w.get::<Count>(e)?.0),
            "y" => Some(y),
            _ => None,
        })
    }

    #[test]
    fn replays_each_rows_ticks_with_the_pad_of_the_field_before() {
        let case = case("");
        assert_eq!(case.schedule().unwrap().iter().map(|r| r.ticks).collect::<Vec<_>>(), [0, 2, 0, 0, 1, 1]);
        // y is 9 throughout in the rebuild, 1 on the last row in the original.
        let report = run(&case, None, 9.0);
        assert!(report.timing.is_ok(), "{report}");
        assert!(!report.passed());
        let (claim, comparison) = &report.compared[0];
        assert_eq!((claim.spec.as_deref(), claim.rows.clone()), (Some("SPEC-1"), 0..=5));
        assert_eq!(comparison.columns[0].first_divergence, None, "x follows the pad: {report}");
        assert_eq!(comparison.columns[1].first_divergence, Some(5));
    }

    #[test]
    fn gives_the_rebuild_what_the_recording_wrote_before_the_tick_that_sees_it() {
        // Written in field 2 (no tick: it waits for row 4's) and from field 4 on.
        let writes = r#", "writes": [{"column": "x", "value": 7, "fields": {"from": 2, "to": 2}},
            {"column": "y", "value": 1, "fields": {"from": 4, "to": null}}]"#;
        let case = case(writes);
        assert_eq!(case.writes().unwrap()[1], Written { column: "y".into(), value: 1.0, from: 4, to: None });
        let mut app = headless_app(TickRate::every_field(Region::Ntsc));
        app.add_systems(FixedUpdate, count.in_set(GameSystems::Game));
        let e = app.world_mut().spawn(Count(case.first("x"))).id();
        let given = std::cell::RefCell::new(Vec::new());
        let report = case
            .runner()
            .write(|app, column, value| {
                given.borrow_mut().push((column.to_string(), value, app.world().get::<Count>(e).unwrap().0));
            })
            .run(&mut app, |w, c| match c {
                "x" => Some(w.get::<Count>(e)?.0),
                "y" => Some(9.0),
                _ => None,
            });
        // x was 2 when field 2's write reached the tick of row 4; y once, before row 5's.
        assert_eq!(given.into_inner(), [("x".to_string(), 7.0, 2.0), ("y".to_string(), 1.0, 3.0)], "{report}");
    }

    #[test]
    #[should_panic(expected = "give them to the rebuild with CaseRun::write")]
    fn a_case_with_writes_needs_a_writer() {
        let case = case(r#", "writes": [{"column": "y", "value": 1, "fields": {"from": 4, "to": null}}]"#);
        run(&case, None, 9.0);
    }

    #[test]
    fn checks_itself_against_the_timing_vectors() {
        let json = r#"{"id": "c", "role": "heldout", "input": [], "observations": "c.csv", "compare": {"x": 0},
            "timing": {"ticks_run": [0, 1, 1, 0, 1, 1], "reads_field": [null, 0, 1, null, 3, 4]}}"#;
        let case = Case::parse("c", json, CSV).unwrap();
        let report = run(&case, None, 0.0);
        let error = report.timing.clone().unwrap_err();
        assert!(error.contains("row 1: ran 2 tick(s)"), "{error}");
        assert!(!report.passed());
    }

    #[test]
    fn compares_only_the_claims_of_implemented_specs() {
        let coverage = r#", "coverage": {"SPEC-1": {"columns": ["x"], "rows": [1, 3]}, "SPEC-2": {"columns": ["y"]}}"#;
        let covered = case(coverage);
        let report = run(&covered, Some(&["SPEC-1"]), 9.0);
        assert!(report.passed(), "{report}");
        assert_eq!(report.waiting.len(), 1);
        assert_eq!(report.waiting[0].to_string(), "SPEC-2: y over rows 0..=5");
        // Replayed only as far as SPEC-1's last row.
        assert_eq!(report.rebuild.ticks(), 4);
        // A verdict per spec compared; the one waiting has none.
        assert_eq!(report.verdicts(), [("SPEC-1", true)]);
        let all = run(&covered, None, 9.0);
        assert!(!all.passed());
        assert_eq!(all.verdicts(), [("SPEC-1", true), ("SPEC-2", false)]);

        let mut cut = case("");
        cut.truncate(3);
        let report = run(&cut, None, 9.0);
        assert!(report.passed() && report.rebuild.ticks() == 3, "{report}");

        let partial = case(r#", "coverage": {"SPEC-1": {"columns": ["x"], "rows": [0, 5]}}"#);
        assert_eq!(run(&partial, None, 0.0).unclaimed, ["y"]);
        let bad = case(r#", "coverage": {"SPEC-1": {"columns": ["z"]}}"#);
        assert!(bad.claims().unwrap_err().contains("\"z\" is not compared"));
    }

    #[test]
    fn diagnostic_cases_never_fail_and_runs_write_in_the_cases_form() {
        let mut case = case("");
        case.json["role"] = "diagnostic".into();
        let report = run(&case, None, 9.0);
        assert!(report.passed() && report.to_string().contains("diagnostic"));
        let dir = std::env::temp_dir().join(format!("psx-bevy-case-{}", std::process::id()));
        let json = report.write_run(&dir).unwrap();
        let again = Case::load(&json).unwrap();
        assert_eq!(again.ticks_by_row().unwrap(), case.ticks_by_row().unwrap());
        assert_eq!(again.get(4, "x"), Some(3.0));
        assert_eq!(again.get(5, "frame"), Some(55.0));
        assert!(run(&again, None, 9.0).passed());
        let result: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{}.result.json", case.name))).unwrap())
                .unwrap();
        // What it found, though it never fails: psx counts no diagnostic verdict.
        assert_eq!((result["kind"].as_str(), result["specs"]["SPEC-1"].as_bool()), (Some("diagnostic"), Some(false)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn older_cases_without_a_tick_column_run_in_steady_play() {
        let json = r#"{"id": "o", "input": [], "observations": "o.csv", "compare": {"x": 0},
            "first_tick": 1, "fields_per_tick": 2}"#;
        let case = Case::parse("o", json, "run_frame,frame,x\n0,0,0\n1,1,0\n2,2,0\n3,3,0\n4,4,0\n").unwrap();
        assert_eq!(case.ticks_by_row().unwrap(), [0, 1, 1, 2, 2]);
        assert!(run(&case, None, 0.0).passed());
        assert_eq!(case.start().unwrap(), Start::FirstRow);
        let state = Case::parse("s", r#"{"start": {"state": "gym"}, "compare": {"x": 0}}"#, "x\n1\n").unwrap();
        assert_eq!(state.start().unwrap(), Start::State("gym".into()));
        assert!(Case::parse("b", r#"{"compare": {"z": 0}}"#, "x\n1\n").unwrap_err().contains("\"z\""));
    }
}

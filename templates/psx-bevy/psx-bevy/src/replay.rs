//! Step replay: a mechanic's step, the port's own code, run against the
//! traces captured from the original (`mechanics/<id>/traces/`), with no
//! emulator. psx's `docs/mechanics.md` defines the files; `psx port check`
//! runs the game's `tests/mechanics.rs` and records the verdicts.
//!
//! A mechanic's port is a pure step over its state, inputs, params and
//! world. An adapter ([`Replay`]) moves trace fields to the port's types and
//! back, and answers the world's calls from the trace ([`Calls`]):
//!
//! ```
//! use psx_bevy::prelude::*;
//! use psx_bevy::replay::{Calls, Fields, Record, Replay};
//!
//! // The port's step (in the game, a feature's module).
//! #[derive(Default)]
//! pub struct Walk<M: Mode> {
//!     pub speed: Num<M, i32, 12>,
//! }
//!
//! pub trait Floor {
//!     fn height(&mut self, x: i32) -> i32;
//! }
//!
//! /// 0x80043234: `v = v - (v >> 2) + target`.
//! pub fn walk<M: Mode>(s: &mut Walk<M>, target: Num<M, i32, 12>, forward: bool, floor: &mut impl Floor) {
//!     if forward {
//!         floor.height(0);
//!         s.speed = s.speed - s.speed.div_pow2_floor(2) + target;
//!     }
//! }
//!
//! // The adapter (in the game's tests/mechanics.rs).
//! struct WalkTrace;
//! struct World<'a>(&'a mut Calls);
//!
//! impl Floor for World<'_> {
//!     fn height(&mut self, x: i32) -> i32 {
//!         self.0.query("world.floor-height@1", "height", |r| r.put("x", &x)).get("y")
//!     }
//! }
//!
//! impl<M: Mode> Replay<M> for WalkTrace {
//!     const MECHANIC: &'static str = "croc.walk";
//!     type Params = Num<M, i32, 12>;
//!     type Inputs = bool;
//!     type State = Walk<M>;
//!
//!     fn params(p: &Fields) -> Result<Self::Params, String> {
//!         p.get("target")
//!     }
//!     fn inputs(i: &Fields) -> Result<bool, String> {
//!         i.get("forward")
//!     }
//!     fn load(s: &mut Walk<M>, f: &Fields) -> Result<(), String> {
//!         f.set("speed", &mut s.speed)
//!     }
//!     fn record(s: &Walk<M>, out: &mut Record) {
//!         out.put("speed", &s.speed);
//!     }
//!     fn step(target: &Self::Params, s: &mut Walk<M>, forward: &bool, calls: &mut Calls) {
//!         walk(s, *target, *forward, &mut World(calls));
//!     }
//! }
//!
//! // #[test] fn mechanics() { Replays::new(root).exact::<WalkTrace>().run(); }
//! ```
//!
//! - **Exact** mechanics ([`Replays::exact`]) run in [`Exact`] from the
//!   trace's initial state through every step, taking each external change
//!   (`state_before`), and must match every field of every step and every
//!   call. Each also runs in [`Float`] for the **drift report**: from the
//!   recorded state, again every second of steps (the mechanic's rate),
//!   with each field's worst error after 1, K/4, K/2 and K steps in raw
//!   units. The drift report never fails a replay.
//! - **Faithful** mechanics ([`Replays::faithful`]) start every step from
//!   the recorded state and are compared within `mechanic.toml`'s
//!   `tolerance`; calls must still match exactly.
//!
//! An adapter that reads or records the wrong fields fails the replay with
//! the field's name: every param, input and observed state field is moved
//! both ways.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use psx_fixed::{Angle, Exact, Float, Mode, Num, Word};
use psx_mechanics::mechanic::{Exactness, Per};
use psx_mechanics::trace::{Call, Full, SourceKind};
use psx_mechanics::{Interface, Mechanic, Trace};
use serde::Serialize;

pub use psx_mechanics::{Value, Values};

/// A value as the port holds it: read from a trace, and compared with one.
pub trait Field: Sized {
    fn from_value(v: &Value) -> Result<Self, String>;
    fn to_raw(&self) -> Raw;
}

/// A value as the port computed it, in the trace's raw units.
#[derive(Clone, Debug, PartialEq)]
pub enum Raw {
    /// Exactly: an integer (a fixed's underlying integer), a bool or a
    /// variant's name.
    Exact(Value),
    /// An `f32` port's number in raw units: the real value × 2^frac_bits.
    Approx(f64),
    /// A table's elements.
    List(Vec<Raw>),
}

impl Raw {
    /// A variant, for an enum's [`Field::to_raw`].
    pub fn variant(name: &str) -> Raw {
        Raw::Exact(Value::Variant(name.into()))
    }
}

/// A variant's name, for an enum's [`Field::from_value`].
pub fn variant(v: &Value) -> Result<&str, String> {
    match v {
        Value::Variant(n) => Ok(n),
        v => Err(format!("{} is not a variant", describe(v))),
    }
}

pub(crate) fn describe(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn word_name<W: Word>() -> String {
    format!("{}{}", if W::SIGNED { "i" } else { "u" }, W::BITS)
}

fn int(v: &Value) -> Result<i128, String> {
    match v {
        Value::Int(n) => Ok(*n),
        v => Err(format!("{} is not an integer", describe(v))),
    }
}

/// The raw integer, refused when it does not fit the port's word `W`.
fn in_word<W: Word>(v: &Value) -> Result<i64, String> {
    let n = int(v)?;
    if n < W::MIN as i128 || n > W::MAX as i128 {
        return Err(format!("{n} does not fit the port's {}", word_name::<W>()));
    }
    Ok(n as i64)
}

impl<M: Mode, W: Word, const F: u32> Field for Num<M, W, F> {
    fn from_value(v: &Value) -> Result<Self, String> {
        in_word::<W>(v).map(Num::from_raw)
    }

    fn to_raw(&self) -> Raw {
        // Exact: the word's value / 2^F, which f64 holds exactly.
        let raw = self.to_f64() * (1u64 << F) as f64;
        if M::EXACT { Raw::Exact(Value::Int(raw as i128)) } else { Raw::Approx(raw) }
    }
}

impl<M: Mode, W: Word, const BITS: u32> Field for Angle<M, W, BITS> {
    fn from_value(v: &Value) -> Result<Self, String> {
        in_word::<W>(v).map(Angle::from_units)
    }

    fn to_raw(&self) -> Raw {
        self.turns().to_raw()
    }
}

impl Field for bool {
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Bool(b) => Ok(*b),
            v => Err(format!("{} is not true or false", describe(v))),
        }
    }

    fn to_raw(&self) -> Raw {
        Raw::Exact(Value::Bool(*self))
    }
}

macro_rules! ints {
    ($($t:ty),*) => {$(
        impl Field for $t {
            fn from_value(v: &Value) -> Result<Self, String> {
                let n = int(v)?;
                <$t>::try_from(n).map_err(|_| format!("{n} does not fit the port's {}", stringify!($t)))
            }

            fn to_raw(&self) -> Raw {
                Raw::Exact(Value::Int(*self as i128))
            }
        }
    )*};
}

ints!(i8, u8, i16, u16, i32, u32, i64, u64);

impl<T: Field> Field for Vec<T> {
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::List(l) => {
                l.iter().enumerate().map(|(i, x)| T::from_value(x).map_err(|e| format!("[{i}]: {e}"))).collect()
            }
            v => Err(format!("{} is not a table", describe(v))),
        }
    }

    fn to_raw(&self) -> Raw {
        Raw::List(self.iter().map(Field::to_raw).collect())
    }
}

impl<T: Field, const N: usize> Field for [T; N] {
    fn from_value(v: &Value) -> Result<Self, String> {
        let l = Vec::<T>::from_value(v)?;
        let n = l.len();
        l.try_into().map_err(|_| format!("a table of {n}, the port's holds {N}"))
    }

    fn to_raw(&self) -> Raw {
        Raw::List(self.iter().map(Field::to_raw).collect())
    }
}

/// Named values from a trace (params, inputs, a state, a response), read
/// into the port's types. It remembers which were read, so the replay can
/// name a field the adapter leaves out.
pub struct Fields<'a> {
    values: &'a Values,
    read: RefCell<BTreeSet<String>>,
}

impl<'a> Fields<'a> {
    fn new(values: &'a Values) -> Fields<'a> {
        Fields { values, read: RefCell::default() }
    }

    /// The field `name`.
    pub fn get<T: Field>(&self, name: &str) -> Result<T, String> {
        let v = self.values.get(name).ok_or_else(|| format!("{name} is not in the trace"))?;
        self.read.borrow_mut().insert(name.into());
        T::from_value(v).map_err(|e| format!("{name}: {e}"))
    }

    /// Sets `into` to the field `name` when the trace has it (a state's
    /// unobserved fields keep the port's value).
    pub fn set<T: Field>(&self, name: &str, into: &mut T) -> Result<(), String> {
        if self.values.contains_key(name) {
            *into = self.get(name)?;
        }
        Ok(())
    }

    /// The fields the adapter did not read.
    fn unread(&self) -> Vec<String> {
        let read = self.read.borrow();
        self.values.keys().filter(|k| !read.contains(*k)).cloned().collect()
    }
}

/// A state or a request as the port computed it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record(BTreeMap<String, Raw>);

impl Record {
    pub fn put<T: Field>(&mut self, name: &str, v: &T) {
        self.0.insert(name.into(), v.to_raw());
    }
}

/// How a port's value compares with the trace's.
pub(crate) enum Diff {
    Equal,
    /// Numbers, this far apart in raw units.
    Off(f64),
    /// Not comparable as numbers: a bool, a variant, a table's length.
    Other,
}

pub(crate) fn diff(port: &Raw, trace: &Value) -> Diff {
    match (port, trace) {
        (Raw::Exact(a), b) if a == b => Diff::Equal,
        (Raw::Exact(Value::Int(a)), Value::Int(b)) => Diff::Off((a - b).unsigned_abs() as f64),
        (Raw::Approx(a), Value::Int(b)) if *a == *b as f64 => Diff::Equal,
        (Raw::Approx(a), Value::Int(b)) => Diff::Off((a - *b as f64).abs()),
        (Raw::List(a), Value::List(b)) if a.len() == b.len() => {
            a.iter().zip(b).fold(Diff::Equal, |worst, (x, y)| match (worst, diff(x, y)) {
                (Diff::Other, _) | (_, Diff::Other) => Diff::Other,
                (Diff::Off(p), Diff::Off(q)) => Diff::Off(p.max(q)),
                (Diff::Off(p), Diff::Equal) | (Diff::Equal, Diff::Off(p)) => Diff::Off(p),
                (Diff::Equal, Diff::Equal) => Diff::Equal,
            })
        }
        _ => Diff::Other,
    }
}

pub(crate) fn show(r: &Raw) -> String {
    match r {
        Raw::Exact(v) => describe(v),
        Raw::Approx(x) => format!("{x}"),
        Raw::List(l) => format!("[{}]", l.iter().map(show).collect::<Vec<_>>().join(", ")),
    }
}

/// Compares a record with the trace's values: problems (a missing,
/// undeclared or unequal field) and each number's error.
fn compare(
    what: &str,
    port: &Record,
    trace: &Values,
    skip: &[String],
    allowed: &dyn Fn(&str, f64) -> bool,
    errors: &mut Vec<(String, f64)>,
) -> Vec<String> {
    let mut p = Vec::new();
    for (k, want) in trace {
        let Some(got) = port.0.get(k) else {
            p.push(format!("{what} {k}: the adapter does not record it"));
            continue;
        };
        match diff(got, want) {
            Diff::Equal => errors.push((k.clone(), 0.0)),
            Diff::Off(d) => {
                errors.push((k.clone(), d));
                if !allowed(k, d) {
                    p.push(format!("{what} {k}: expected {}, got {}", describe(want), show(got)));
                }
            }
            Diff::Other => p.push(format!("{what} {k}: expected {}, got {}", describe(want), show(got))),
        }
    }
    for k in port.0.keys().filter(|k| !trace.contains_key(*k) && !skip.contains(k)) {
        p.push(format!("{what} {k}: not a field of the trace"));
    }
    p
}

/// The world as the trace recorded it, for one step: the port's calls are
/// checked against the original's, in order, and queries get the
/// original's responses.
pub struct Calls {
    recorded: Vec<Call>,
    next: usize,
    problems: Rc<RefCell<Vec<String>>>,
    /// Exact requests (replay) or measured ones (the drift run).
    measure: bool,
    errors: Vec<(String, f64)>,
}

impl Calls {
    fn new(recorded: Vec<Call>, measure: bool) -> Calls {
        Calls { recorded, next: 0, problems: Rc::default(), measure, errors: Vec::new() }
    }

    fn problem(&self, p: String) {
        self.problems.borrow_mut().push(p);
    }

    /// The next recorded call, when it is `interface`'s `call` with this
    /// request.
    fn take(&mut self, interface: &str, call: &str, request: impl FnOnce(&mut Record), query: bool) -> Option<Values> {
        let n = self.next;
        self.next += 1;
        let Some(c) = self.recorded.get(n) else {
            self.problem(format!("call {n}: the port called {interface} {call}, the original made {n} calls"));
            return None;
        };
        if c.interface != interface || c.call != call {
            self.problem(format!(
                "call {n}: the port called {interface} {call}, the original {} {}",
                c.interface, c.call
            ));
            return None;
        }
        let mut r = Record::default();
        request(&mut r);
        let measure = self.measure;
        let mut errors = Vec::new();
        let what = format!("call {n} ({interface} {call}): request");
        for p in compare(&what, &r, &c.request, &[], &|_, _| measure, &mut errors) {
            self.problem(p);
        }
        self.errors.extend(errors.into_iter().map(|(k, d)| (format!("{interface} {call} {k}"), d)));
        match (&c.response, query) {
            (Some(r), true) => Some(r.clone()),
            (None, false) => Some(Values::new()),
            (_, true) => {
                self.problem(format!("call {n}: the port queried {interface} {call}, a command"));
                None
            }
            (_, false) => {
                self.problem(format!("call {n}: the port commanded {interface} {call}, a query"));
                None
            }
        }
    }

    /// A query: `request` puts the request's fields, and the answer gives
    /// the original's response.
    pub fn query(&mut self, interface: &str, call: &str, request: impl FnOnce(&mut Record)) -> Answer {
        let values = self.take(interface, call, request, true).unwrap_or_default();
        Answer {
            values,
            problems: self.problems.clone(),
            at: format!("call {} ({interface} {call}): response", self.next - 1),
        }
    }

    /// A command: `request` puts its fields.
    pub fn command(&mut self, interface: &str, call: &str, request: impl FnOnce(&mut Record)) {
        self.take(interface, call, request, false);
    }

    /// The step is over: problems, with any calls the port did not make.
    fn finish(self) -> Compared {
        if let Some(c) = self.recorded.get(self.next) {
            self.problem(format!(
                "the port made {} calls, the original {}: next {} {}",
                self.next,
                self.recorded.len(),
                c.interface,
                c.call
            ));
        }
        let p = self.problems.borrow().clone();
        (p, self.errors)
    }
}

/// A query's response from the trace.
pub struct Answer {
    values: Values,
    problems: Rc<RefCell<Vec<String>>>,
    at: String,
}

impl Answer {
    /// The response field `name`. When the call did not match the trace's,
    /// the replay has already failed and this is the type's default.
    pub fn get<T: Field + Default>(&self, name: &str) -> T {
        if self.values.is_empty() && !self.problems.borrow().is_empty() {
            return T::default();
        }
        Fields::new(&self.values).get(name).unwrap_or_else(|e| {
            self.problems.borrow_mut().push(format!("{}: {e}", self.at));
            T::default()
        })
    }
}

/// A mechanic's adapter: the port's types for its params, inputs and state,
/// and its step. Implement it for every [`Mode`] (exact mechanics run in
/// both) or the one a faithful port uses.
pub trait Replay<M: Mode> {
    /// The mechanic's id, as in `mechanic.toml`.
    const MECHANIC: &'static str;
    type Params;
    type Inputs;
    type State: Default;

    /// Every param, from the trace's header.
    fn params(p: &Fields) -> Result<Self::Params, String>;
    /// Every input, for one step.
    fn inputs(i: &Fields) -> Result<Self::Inputs, String>;
    /// Sets the state's fields that `f` holds ([`Fields::set`]): every
    /// observed field.
    fn load(state: &mut Self::State, f: &Fields) -> Result<(), String>;
    /// Every observed state field.
    fn record(state: &Self::State, out: &mut Record);
    /// One step: the port's own step, given the world as [`Calls`].
    fn step(params: &Self::Params, state: &mut Self::State, inputs: &Self::Inputs, calls: &mut Calls);
}

/// The replay's results, written to `$PSX_REPLAY_REPORT` for psx.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub mechanics: Vec<MechanicReport>,
    /// Mechanics in `mechanics/` with no adapter registered.
    pub unregistered: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MechanicReport {
    pub id: String,
    /// `exact` or `faithful`, as registered.
    pub exactness: String,
    /// What stops the mechanic from replaying at all.
    pub problems: Vec<String>,
    pub traces: Vec<TraceReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<Drift>,
}

impl MechanicReport {
    pub fn passed(&self) -> bool {
        self.problems.is_empty() && self.traces.iter().all(|t| t.failure.is_none())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TraceReport {
    /// The file's name without `.trace.jsonl`.
    pub name: String,
    /// From the original (`source.kind = captured`): evidence.
    pub captured: bool,
    pub steps: usize,
    /// The first divergence: the step and field, expected and actual.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// The `f32` run of an exact mechanic.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Drift {
    /// Steps between restarts from the recorded state.
    pub k: usize,
    /// Runs from the recorded state.
    pub windows: usize,
    /// Runs that left the original's path (a bool or variant differs, or
    /// the calls do): how many, and the first few.
    pub diverged: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub first_diverged: Vec<String>,
    /// Per state field (and request field, `<interface> <call> <field>`):
    /// the worst error, in raw units.
    pub fields: BTreeMap<String, Growth>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Growth {
    pub worst: f64,
    /// The worst error n steps after a restart, for n = 1, K/4, K/2 and K.
    pub after: Vec<After>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct After {
    pub steps: usize,
    pub worst: f64,
}

/// A mechanic as the workspace holds it.
struct Loaded {
    mechanic: Mechanic,
    traces: Vec<(String, Result<Trace, String>)>,
}

type Run = Box<dyn Fn(&Loaded) -> MechanicReport>;

/// The game's registered adapters, replayed against the workspace's
/// traces.
pub struct Replays {
    root: PathBuf,
    runs: Vec<(&'static str, Run)>,
}

impl Replays {
    /// The workspace at `root` (the folder holding `mechanics/`).
    pub fn new(root: impl Into<PathBuf>) -> Replays {
        Replays { root: root.into(), runs: Vec::new() }
    }

    /// An exact mechanic: replayed in [`Exact`], with the drift run in
    /// [`Float`].
    pub fn exact<A: Replay<Exact> + Replay<Float> + 'static>(mut self) -> Replays {
        let id = <A as Replay<Exact>>::MECHANIC;
        self.runs.push((id, Box::new(|l: &Loaded| run_exact::<A>(l))));
        self
    }

    /// A faithful mechanic, in the mode its port uses.
    pub fn faithful<A: Replay<M> + 'static, M: Mode>(mut self) -> Replays {
        self.runs.push((A::MECHANIC, Box::new(|l: &Loaded| run_faithful::<A, M>(l))));
        self
    }

    /// Replays every registered mechanic.
    pub fn report(&self) -> Report {
        let dir = self.root.join("mechanics");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().join("mechanic.toml").is_file())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        on_disk.sort();
        let interfaces = load_interfaces(&self.root);
        let mut report = Report::default();
        let mut seen = BTreeSet::new();
        for (id, run) in &self.runs {
            if !seen.insert(*id) {
                report.mechanics.push(MechanicReport {
                    id: id.to_string(),
                    problems: vec!["registered twice".into()],
                    ..Default::default()
                });
                continue;
            }
            let loaded = interfaces.clone().and_then(|i| load(&dir.join(id), id, i));
            report.mechanics.push(match loaded {
                Ok(l) => run(&l),
                Err(e) => MechanicReport { id: id.to_string(), problems: vec![e], ..Default::default() },
            });
        }
        report.unregistered = on_disk.into_iter().filter(|m| !seen.contains(m.as_str())).collect();
        report
    }

    /// Replays every registered mechanic, prints the results, writes them
    /// to `$PSX_REPLAY_REPORT` when it is set, and panics when one fails.
    pub fn run(self) {
        let report = self.report();
        if let Some(p) = std::env::var_os("PSX_REPLAY_REPORT") {
            std::fs::write(&p, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
        let mut failed = Vec::new();
        for m in &report.mechanics {
            let steps: usize = m.traces.iter().map(|t| t.steps).sum();
            println!(
                "{} ({}): {} traces, {steps} steps, {}",
                m.id,
                m.exactness,
                m.traces.len(),
                if m.passed() { "pass" } else { "FAIL" }
            );
            for p in &m.problems {
                failed.push(format!("{}: {p}", m.id));
            }
            for t in &m.traces {
                if let Some(f) = &t.failure {
                    failed.push(format!("{} {}: {f}", m.id, t.name));
                }
            }
            if let Some(d) = &m.drift {
                let worst = d.fields.iter().map(|(k, g)| format!("{k} {}", g.worst)).collect::<Vec<_>>().join(", ");
                println!(
                    "  f32 drift (restart every {} steps, {} of {} runs diverged): {worst}",
                    d.k, d.diverged, d.windows
                );
            }
        }
        if !report.unregistered.is_empty() {
            println!("no adapter: {}", report.unregistered.join(", "));
        }
        assert!(failed.is_empty(), "replay failed:\n{}", failed.join("\n"));
    }
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn load_interfaces(root: &Path) -> Result<Vec<Interface>, String> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(root.join("interfaces")) else { return Ok(out) };
    let mut paths: Vec<PathBuf> =
        dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
    paths.sort();
    for p in paths {
        out.push(Interface::parse(&read(&p)?).map_err(|e| format!("{}: {e}", p.display()))?);
    }
    Ok(out)
}

fn load(dir: &Path, id: &str, interfaces: Vec<Interface>) -> Result<Loaded, String> {
    let file = dir.join("mechanic.toml");
    if !file.is_file() {
        return Err(format!("no mechanics/{id}/mechanic.toml"));
    }
    let mechanic = Mechanic::parse(&read(&file)?).map_err(|e| format!("mechanic.toml: {e}"))?;
    let problems = mechanic.check(&interfaces);
    if !problems.is_empty() {
        return Err(format!("mechanic.toml: {}", problems.join("; ")));
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir.join("traces"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".trace.jsonl")).collect())
        .unwrap_or_default();
    paths.sort();
    let traces = paths
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().trim_end_matches(".trace.jsonl").to_string();
            let t = read(&p).and_then(|s| Trace::parse(&s)).and_then(|t| {
                let problems = t.check(&mechanic, &interfaces);
                if problems.is_empty() { Ok(t) } else { Err(problems.join("; ")) }
            });
            (name, t)
        })
        .collect();
    Ok(Loaded { mechanic, traces })
}

/// The observed state fields of a full state.
fn observed(t: &Trace, s: &Values) -> Values {
    s.iter().filter(|(k, _)| !t.header.unobserved.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Reads every field of `values` with `f`, or names what the adapter left
/// out.
fn read_all<T>(what: &str, values: &Values, f: impl FnOnce(&Fields) -> Result<T, String>) -> Result<T, String> {
    let fields = Fields::new(values);
    let t = f(&fields).map_err(|e| format!("{what}: {e}"))?;
    match fields.unread().as_slice() {
        [] => Ok(t),
        u => Err(format!("{what}: the adapter does not read {}", u.join(", "))),
    }
}

/// A step's problems, and each number's error in raw units.
type Compared = (Vec<String>, Vec<(String, f64)>);

/// The adapter run over one trace's steps.
struct Runner<'t, A: Replay<M>, M: Mode> {
    trace: &'t Trace,
    params: A::Params,
    state: A::State,
}

impl<'t, A: Replay<M>, M: Mode> Runner<'t, A, M> {
    fn new(trace: &'t Trace) -> Result<Self, String> {
        let params = read_all("params", &trace.header.params, A::params)?;
        let mut state = A::State::default();
        read_all("initial_state", &trace.header.initial_state, |f| A::load(&mut state, f))?;
        Ok(Runner { trace, params, state })
    }

    fn reset(&mut self, before: &Values) -> Result<(), String> {
        let before = observed(self.trace, before);
        read_all("state", &before, |f| A::load(&mut self.state, f))
    }

    /// One step: its problems, and each number's error.
    fn step(&mut self, f: &Full, measure: bool, allowed: &dyn Fn(&str, f64) -> bool) -> Result<Compared, String> {
        let inputs = read_all("inputs", &f.inputs, A::inputs)?;
        let mut calls = Calls::new(f.calls.clone(), measure);
        A::step(&self.params, &mut self.state, &inputs, &mut calls);
        let (mut problems, mut errors) = calls.finish();
        let mut out = Record::default();
        A::record(&self.state, &mut out);
        let after = observed(self.trace, &f.after);
        problems.extend(compare("state", &out, &after, &self.trace.header.unobserved, allowed, &mut errors));
        Ok((problems, errors))
    }
}

fn trace_report(name: &str, t: &Trace) -> TraceReport {
    TraceReport {
        name: name.into(),
        captured: t.header.source.kind == SourceKind::Captured,
        steps: t.steps.len(),
        failure: None,
    }
}

fn check_exactness(m: &Mechanic, want: Exactness) -> Vec<String> {
    let name = |e| if e == Exactness::Exact { "exact" } else { "faithful" };
    if m.exactness == want {
        Vec::new()
    } else {
        vec![format!("registered as {}, mechanic.toml says {}", name(want), name(m.exactness))]
    }
}

/// Replays each trace, `each_step_from_trace` for a faithful mechanic.
fn replay<A: Replay<M>, M: Mode>(
    l: &Loaded,
    each_step_from_trace: bool,
    allowed: &dyn Fn(&str, f64) -> bool,
) -> Vec<TraceReport> {
    l.traces
        .iter()
        .map(|(name, t)| {
            let t = match t {
                Ok(t) => t,
                Err(e) => return TraceReport { name: name.clone(), failure: Some(e.clone()), ..Default::default() },
            };
            let mut r = trace_report(name, t);
            r.failure = (|| {
                let mut run = Runner::<A, M>::new(t)?;
                for (i, f) in t.expand().iter().enumerate() {
                    if each_step_from_trace || f.external {
                        run.reset(&f.before).map_err(|e| format!("step {i}: {e}"))?;
                    }
                    let (problems, _) = run.step(f, false, allowed).map_err(|e| format!("step {i}: {e}"))?;
                    if let Some(p) = problems.first() {
                        return Err(format!("step {i}: {p}"));
                    }
                }
                Ok(())
            })()
            .err();
            r
        })
        .collect()
}

fn run_exact<A: Replay<Exact> + Replay<Float>>(l: &Loaded) -> MechanicReport {
    let problems = check_exactness(&l.mechanic, Exactness::Exact);
    let traces = if problems.is_empty() { replay::<A, Exact>(l, false, &|_, _| false) } else { Vec::new() };
    let drift = problems.is_empty().then(|| drift::<A>(l));
    MechanicReport { id: l.mechanic.id.clone(), exactness: "exact".into(), problems, traces, drift }
}

fn run_faithful<A: Replay<M>, M: Mode>(l: &Loaded) -> MechanicReport {
    let problems = check_exactness(&l.mechanic, Exactness::Faithful);
    let tolerance: BTreeMap<String, f64> = l
        .mechanic
        .tolerance
        .iter()
        .filter_map(|(k, v)| match v {
            Value::Int(n) => Some((k.clone(), *n as f64)),
            _ => None,
        })
        .collect();
    let allowed = move |k: &str, d: f64| d <= tolerance.get(k).copied().unwrap_or(0.0);
    let traces = if problems.is_empty() { replay::<A, M>(l, true, &allowed) } else { Vec::new() };
    MechanicReport { id: l.mechanic.id.clone(), exactness: "faithful".into(), problems, traces, drift: None }
}

/// The run's restart interval: a second of steps, or 60 for a mechanic
/// that runs on events.
fn drift_k(m: &Mechanic) -> usize {
    match (m.rate.per, m.rate.hz) {
        (Per::Tick | Per::Vblank, Some(hz)) if hz >= 1.0 => hz.round() as usize,
        _ => 60,
    }
}

fn drift<A: Replay<Float>>(l: &Loaded) -> Drift {
    let k = drift_k(&l.mechanic);
    let points: BTreeSet<usize> = [1, k / 4, k / 2, k].into_iter().filter(|n| *n >= 1).collect();
    let mut d = Drift { k, ..Default::default() };
    let note = |d: &mut Drift, what: String| {
        d.diverged += 1;
        if d.first_diverged.len() < 5 {
            d.first_diverged.push(what);
        }
    };
    for (name, t) in &l.traces {
        let Ok(t) = t else { continue };
        let mut run = match Runner::<A, Float>::new(t) {
            Ok(r) => r,
            Err(e) => {
                note(&mut d, format!("{name}: {e}"));
                continue;
            }
        };
        let mut since = 0;
        for (i, f) in t.expand().iter().enumerate() {
            if since == 0 || f.external {
                if since == 0 {
                    d.windows += 1;
                }
                if let Err(e) = run.reset(&f.before) {
                    note(&mut d, format!("{name} step {i}: {e}"));
                    break;
                }
            }
            since += 1;
            match run.step(f, true, &|_, _| true) {
                Err(e) => {
                    note(&mut d, format!("{name} step {i}: {e}"));
                    break;
                }
                Ok((problems, errors)) => {
                    for (field, e) in errors {
                        let g = d.fields.entry(field).or_default();
                        g.worst = g.worst.max(e);
                        if points.contains(&since) {
                            match g.after.iter_mut().find(|a| a.steps == since) {
                                Some(a) => a.worst = a.worst.max(e),
                                None => {
                                    g.after.push(After { steps: since, worst: e });
                                    g.after.sort_by_key(|a| a.steps);
                                }
                            }
                        }
                    }
                    if let Some(p) = problems.first() {
                        note(&mut d, format!("{name} step {i}: {p}"));
                        since = 0;
                    }
                }
            }
            if since == k {
                since = 0;
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const MECHANIC: &str = r#"
id = "demo.walk"
version = 1
title = "Walking"
summary = "Speed closes a quarter of the gap to four times the target each tick."
rate = { per = "tick", hz = 4 }
requires = ["world.floor@1"]

[state.speed]
type = "fixed"
signed = true
int_bits = 20
frac_bits = 12

[state.mode]
type = "enum"
variants = ["idle", "walk"]

[inputs.forward]
type = "bool"

[params.target]
type = "fixed"
signed = true
int_bits = 20
frac_bits = 12
"#;

    const FLOOR: &str = r#"
id = "world.floor"
version = 1
title = "Floor"

[calls.height]
kind = "query"
request.x = { type = "int", bits = 32, signed = true }
response.y = { type = "int", bits = 32, signed = true }
"#;

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    enum Mode2 {
        #[default]
        Idle,
        Walk,
    }

    impl Field for Mode2 {
        fn from_value(v: &Value) -> Result<Self, String> {
            match variant(v)? {
                "idle" => Ok(Mode2::Idle),
                "walk" => Ok(Mode2::Walk),
                n => Err(format!("no variant {n}")),
            }
        }
        fn to_raw(&self) -> Raw {
            Raw::variant(if *self == Mode2::Idle { "idle" } else { "walk" })
        }
    }

    #[derive(Default)]
    struct State<M: Mode> {
        speed: Num<M, i32, 12>,
        mode: Mode2,
    }

    trait Floor {
        fn height(&mut self, x: i32) -> i32;
    }

    /// 0x80043234: `v = v - (v >> 2) + target`, then the floor under the
    /// integer part of the speed.
    fn walk<M: Mode>(s: &mut State<M>, target: Num<M, i32, 12>, forward: bool, floor: &mut impl Floor) {
        if forward {
            s.mode = Mode2::Walk;
            s.speed = s.speed - s.speed.div_pow2_floor(2) + target;
            floor.height(s.speed.trunc_int());
        } else {
            s.mode = Mode2::Idle;
            s.speed = Num::zero();
        }
    }

    struct World<'a>(&'a mut Calls);

    impl Floor for World<'_> {
        fn height(&mut self, x: i32) -> i32 {
            self.0.query("world.floor@1", "height", |r| r.put("x", &x)).get("y")
        }
    }

    struct Walk;

    impl<M: Mode> Replay<M> for Walk {
        const MECHANIC: &'static str = "demo.walk";
        type Params = Num<M, i32, 12>;
        type Inputs = bool;
        type State = State<M>;

        fn params(p: &Fields) -> Result<Self::Params, String> {
            p.get("target")
        }
        fn inputs(i: &Fields) -> Result<bool, String> {
            i.get("forward")
        }
        fn load(s: &mut State<M>, f: &Fields) -> Result<(), String> {
            f.set("speed", &mut s.speed)?;
            f.set("mode", &mut s.mode)
        }
        fn record(s: &State<M>, out: &mut Record) {
            out.put("speed", &s.speed);
            out.put("mode", &s.mode);
        }
        fn step(target: &Self::Params, s: &mut State<M>, forward: &bool, calls: &mut Calls) {
            walk(s, *target, *forward, &mut World(calls));
        }
    }

    /// The trace an exact port of `walk` makes, from speed 0.
    fn trace(steps: &[bool], target: i32) -> String {
        let header = format!(
            r#"{{"format":"mechanic-trace","format_version":1,"mechanic":"demo.walk","mechanic_version":1,"source":{{"kind":"synthetic"}},"producer":{{"name":"test","version":"0"}},"params":{{"target":{target}}},"initial_state":{{"mode":"idle","speed":0}}}}"#
        );
        let mut lines = vec![header];
        let (mut v, mut last): (i32, Option<bool>) = (0, None);
        for (i, f) in steps.iter().enumerate() {
            let inputs = if last != Some(*f) { format!(r#","inputs":{{"forward":{f}}}"#) } else { String::new() };
            last = Some(*f);
            let (calls, state) = if *f {
                v = v - (v >> 2) + target;
                let mode = if i == 0 || !steps[i - 1] { r#""mode":"walk","# } else { "" };
                (
                    format!(
                        r#","calls":[{{"interface":"world.floor@1","call":"height","request":{{"x":{}}},"response":{{"y":7}}}}]"#,
                        v >> 12
                    ),
                    format!(r#","state":{{{mode}"speed":{v}}}"#),
                )
            } else {
                let s = if v != 0 { r#","state":{"mode":"idle","speed":0}"# } else { "" };
                v = 0;
                (String::new(), s.to_string())
            };
            lines.push(format!(r#"{{"step":{i}{inputs}{calls}{state}}}"#));
        }
        lines.join("\n") + "\n"
    }

    fn workspace(trace_text: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "psx-bevy-replay-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mechanics/demo.walk/traces")).unwrap();
        std::fs::create_dir_all(root.join("interfaces")).unwrap();
        std::fs::write(root.join("interfaces/world.floor.toml"), FLOOR).unwrap();
        std::fs::write(root.join("mechanics/demo.walk/mechanic.toml"), MECHANIC).unwrap();
        std::fs::write(root.join("mechanics/demo.walk/traces/run.trace.jsonl"), trace_text).unwrap();
        root
    }

    #[test]
    fn an_exact_port_replays_and_reports_its_drift() {
        let steps: Vec<bool> = (0..20).map(|i| i % 10 != 9).collect();
        let root = workspace(&trace(&steps, 0x8C01));
        let r = Replays::new(&root).exact::<Walk>().report();
        let m = &r.mechanics[0];
        assert!(m.passed(), "{m:?}");
        assert_eq!((m.traces[0].steps, m.traces[0].captured), (20, false));
        let d = m.drift.as_ref().unwrap();
        assert_eq!((d.k, d.windows), (4, 5));
        // f32 keeps the bits the original's shift drops.
        assert!(d.fields["speed"].worst > 0.0 && d.fields["speed"].worst < 4.0, "{d:?}");
        assert_eq!(d.fields["speed"].after.iter().map(|a| a.steps).collect::<Vec<_>>(), [1, 2, 4]);
        assert!(d.fields.contains_key("world.floor@1 height x"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_divergence_names_the_step_and_field() {
        let good = trace(&[true, true, true], 0x8C00);
        // Step 1's speed, one raw unit off.
        let second = good.lines().nth(2).unwrap();
        let v1 = 0x8C00 - (0x8C00 >> 2) + 0x8C00;
        let bad = good.replace(second, &second.replace(&format!(r#""speed":{v1}"#), &format!(r#""speed":{}"#, v1 + 1)));
        let root = workspace(&bad);
        let r = Replays::new(&root).exact::<Walk>().report();
        assert_eq!(
            r.mechanics[0].traces[0].failure.as_deref(),
            Some(format!("step 1: state speed: expected {}, got {v1}", v1 + 1).as_str())
        );
        // A call the port does not make the original's way.
        let bad = good.replacen(r#""request":{"x":8}"#, r#""request":{"x":9}"#, 1);
        std::fs::write(root.join("mechanics/demo.walk/traces/run.trace.jsonl"), &bad).unwrap();
        let r = Replays::new(&root).exact::<Walk>().report();
        assert_eq!(
            r.mechanics[0].traces[0].failure.as_deref(),
            Some("step 0: call 0 (world.floor@1 height): request x: expected 9, got 8")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registration_must_match_the_mechanic() {
        let root = workspace(&trace(&[true], 0x1000));
        let r = Replays::new(&root).faithful::<Walk, Float>().report();
        assert_eq!(r.mechanics[0].problems, ["registered as faithful, mechanic.toml says exact"]);
        let r = Replays::new(&root).report();
        assert_eq!(r.unregistered, ["demo.walk"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// An adapter that forgets a field is named.
    struct Forgetful;

    impl<M: Mode> Replay<M> for Forgetful {
        const MECHANIC: &'static str = "demo.walk";
        type Params = Num<M, i32, 12>;
        type Inputs = bool;
        type State = State<M>;

        fn params(p: &Fields) -> Result<Self::Params, String> {
            p.get("target")
        }
        fn inputs(i: &Fields) -> Result<bool, String> {
            i.get("forward")
        }
        fn load(s: &mut State<M>, f: &Fields) -> Result<(), String> {
            f.set("speed", &mut s.speed)
        }
        fn record(s: &State<M>, out: &mut Record) {
            out.put("speed", &s.speed);
        }
        fn step(target: &Self::Params, s: &mut State<M>, forward: &bool, calls: &mut Calls) {
            walk(s, *target, *forward, &mut World(calls));
        }
    }

    #[test]
    fn an_adapter_moves_every_field() {
        let root = workspace(&trace(&[true], 0x1000));
        let r = Replays::new(&root).exact::<Forgetful>().report();
        assert_eq!(r.mechanics[0].traces[0].failure.as_deref(), Some("initial_state: the adapter does not read mode"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

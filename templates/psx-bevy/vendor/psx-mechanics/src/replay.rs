//! Step replay of mechanics written as Rust (docs/rust-mechanics-plan.md,
//! section 6): each trace's steps run through the mechanic's own `step`,
//! with no emulator, and compared with what the original did. This proves
//! the reference: that the rules as written reproduce the original.
//!
//! The workspace's `replay` crate generates a [`Mechanic`] per mechanic in
//! its `build.rs` ([`crate::rust::write_replay`]) and calls [`run`] from its
//! test. The helpers here are what that generated code calls.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::Path;

use crate::Trace;
use crate::trace::Call;
pub use crate::value::{Value, Values};

/// A mechanic as replay sees it: the conversions between its structs and the
/// trace's values, and its step. Generated, not written.
pub trait Mechanic {
    const ID: &'static str;
    const VERSION: u32;
    const FINGERPRINT: &'static str;
    /// `None` for an exact mechanic; a faithful one's tolerance per state
    /// field, in raw units (fields it leaves out must match exactly).
    const TOLERANCE: Option<&'static [(&'static str, i128)]>;
    type State;
    type Inputs;
    type Params;
    fn state(v: &Values) -> Result<Self::State, String>;
    fn inputs(v: &Values) -> Result<Self::Inputs, String>;
    fn params(v: &Values) -> Result<Self::Params, String>;
    fn record(s: &Self::State) -> Values;
    /// The mechanic's step, its calls answered from `calls`.
    fn step(state: &mut Self::State, inputs: &Self::Inputs, params: &Self::Params, calls: &mut Calls);
}

/// The calls a step makes, checked against the trace's in order and
/// answered with its responses. The generated implementations of the
/// interfaces' traits call [`Calls::call`].
pub struct Calls<'t> {
    recorded: &'t [Call],
    next: usize,
}

/// Why a step's call stopped its replay: unwound to [`trace`] without the
/// panic hook, so nothing is printed.
struct Stop(String);

fn stop(why: String) -> ! {
    resume_unwind(Box::new(Stop(why)))
}

/// A command's response.
static NONE: Values = Values::new();

impl<'t> Calls<'t> {
    /// The step calls `interface`'s `call` with `request`: the trace's
    /// response (none for a command), once the trace has the step make this
    /// call next with this request.
    pub fn call(&mut self, interface: &str, call: &str, request: Values) -> &'t Values {
        let k = self.next;
        let Some(c) = self.recorded.get(k) else {
            stop(format!("call {k}: the step calls {interface}.{call}, the trace has no more calls"))
        };
        if c.interface != interface || c.call != call {
            stop(format!("call {k}: the step calls {interface}.{call}, the trace has {}.{}", c.interface, c.call));
        }
        let path = format!("call {k} ({interface}.{call}) request");
        if let Some(d) = differs(&path, Some(&Value::Record(request)), Some(&Value::Record(c.request.clone())), 0) {
            stop(d);
        }
        self.next += 1;
        c.response.as_ref().unwrap_or(&NONE)
    }

    /// The last call's response does not fit the interface's types.
    pub fn bad(&self, e: String) -> ! {
        stop(format!("call {} response: {e}", self.next.saturating_sub(1)))
    }
}

/// One mechanic of the workspace, as the replay crate was built.
pub struct Entry {
    /// The mechanic's directory, relative to the workspace root.
    pub dir: &'static str,
    pub replay: Replayer,
}

pub enum Replayer {
    /// Replays one trace.
    Run(fn(&Trace) -> Result<(), String>),
    /// Cannot replay: why, which fails the run.
    Fails(&'static str),
    /// Not replayed, and not a failure: why.
    Skipped(&'static str),
}

/// Replays every trace of every mechanic in `entries` under the workspace
/// `root`, prints a line per trace and panics naming each failure.
pub fn run(root: &Path, entries: &[Entry]) {
    let mut failures = Vec::new();
    if entries.is_empty() {
        println!("no mechanics under mechanics/");
    }
    for e in entries {
        let run = match &e.replay {
            Replayer::Run(f) => f,
            Replayer::Fails(why) => {
                println!("{}: FAIL: {why}", e.dir);
                failures.push(format!("{}: {why}", e.dir));
                continue;
            }
            Replayer::Skipped(why) => {
                println!("{}: not replayed: {why}", e.dir);
                continue;
            }
        };
        let traces = crate::trace::files(&root.join(e.dir).join("traces"));
        if traces.is_empty() {
            println!("{}: no traces", e.dir);
        }
        for (name, path) in traces {
            let result = std::fs::read_to_string(&path)
                .map_err(|err| err.to_string())
                .and_then(|text| Trace::parse(&text))
                .and_then(|t| run(&t).map(|()| t.steps.len()));
            match result {
                Ok(steps) => println!("{}: {name}: {steps} steps ok", e.dir),
                Err(why) => {
                    println!("{}: {name}: FAIL: {why}", e.dir);
                    failures.push(format!("{}/traces/{name}: {why}", e.dir));
                }
            }
        }
    }
    assert!(failures.is_empty(), "replay failed:\n{}", failures.join("\n"));
}

/// Replays `t` through `M`'s step: an exact mechanic from the initial state
/// through every step, taking each external change, a faithful one from the
/// recorded state at each step.
pub fn trace<M: Mechanic>(t: &Trace) -> Result<(), String> {
    let h = &t.header;
    if h.mechanic != M::ID {
        return Err(format!("the trace is of {}", h.mechanic));
    }
    if h.mechanic_version != M::VERSION {
        return Err(format!("captured with version {}, the mechanic is version {}", h.mechanic_version, M::VERSION));
    }
    if let Some(f) = &h.fingerprint {
        if f != M::FINGERPRINT {
            return Err(format!(
                "captured with fingerprint {f}, the mechanic's fields now give {}: capture again (and raise VERSION)",
                M::FINGERPRINT
            ));
        }
    }
    let params = M::params(&h.params).map_err(|e| format!("params: {e}"))?;
    let mut state = M::state(&h.initial_state).map_err(|e| format!("initial_state: {e}"))?;
    for (i, f) in t.expand().iter().enumerate() {
        let at = |e: String| format!("step {i}: {e}");
        if f.external || M::TOLERANCE.is_some() {
            state = M::state(&f.before).map_err(|e| at(format!("state: {e}")))?;
        }
        let inputs = M::inputs(&f.inputs).map_err(|e| at(format!("inputs: {e}")))?;
        let mut calls = Calls { recorded: &f.calls, next: 0 };
        if let Err(p) = catch_unwind(AssertUnwindSafe(|| M::step(&mut state, &inputs, &params, &mut calls))) {
            match p.downcast::<Stop>() {
                Ok(s) => return Err(at(s.0)),
                Err(p) => resume_unwind(p),
            }
        }
        if let Some(c) = f.calls.get(calls.next) {
            return Err(at(format!(
                "the step made {} calls, the trace has {}: the next is {}.{}",
                calls.next,
                f.calls.len(),
                c.interface,
                c.call
            )));
        }
        let ours = M::record(&state);
        let allowed = |k: &str| M::TOLERANCE.and_then(|t| t.iter().find(|(n, _)| *n == k)).map_or(0, |(_, d)| *d);
        for k in f.after.keys().chain(ours.keys().filter(|k| !f.after.contains_key(*k))) {
            if let Some(d) = differs(k, ours.get(k), f.after.get(k), allowed(k)) {
                return Err(at(d));
            }
        }
    }
    Ok(())
}

/// How `ours` differs from the trace's `theirs` at `path`, beyond `allowed`
/// raw units for an integer.
fn differs(path: &str, ours: Option<&Value>, theirs: Option<&Value>, allowed: i128) -> Option<String> {
    let shown = |v: Option<&Value>| v.map_or("nothing".to_string(), |v| serde_json::to_string(v).unwrap());
    match (ours, theirs) {
        (Some(Value::Int(a)), Some(Value::Int(b))) if (a - b).abs() <= allowed => None,
        (Some(Value::List(a)), Some(Value::List(b))) if a.len() == b.len() => {
            a.iter().zip(b).enumerate().find_map(|(i, (a, b))| differs(&format!("{path}[{i}]"), Some(a), Some(b), 0))
        }
        (Some(Value::Record(a)), Some(Value::Record(b))) => a
            .keys()
            .chain(b.keys().filter(|k| !a.contains_key(*k)))
            .find_map(|k| differs(&format!("{path}.{k}"), a.get(k), b.get(k), 0)),
        (a, b) if a == b => None,
        (a, b) => Some(format!("{path} is {}, the trace has {}", shown(a), shown(b))),
    }
}

// What the generated conversions call.

pub fn int<T: TryFrom<i128>>(v: &Value) -> Result<T, String> {
    match v {
        Value::Int(n) => T::try_from(*n).map_err(|_| format!("{n} is out of range for {}", std::any::type_name::<T>())),
        other => Err(format!("{} is not an integer", serde_json::to_string(other).unwrap())),
    }
}

pub fn boolean(v: &Value) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        other => Err(format!("{} is not a bool", serde_json::to_string(other).unwrap())),
    }
}

pub fn variant(v: &Value) -> Result<&str, String> {
    match v {
        Value::Variant(n) => Ok(n),
        other => Err(format!("{} is not a variant", serde_json::to_string(other).unwrap())),
    }
}

pub fn unknown<T>(variant: &str) -> Result<T, String> {
    Err(format!("{variant:?} is not one of the enum's variants"))
}

pub fn list(v: &Value) -> Result<&[Value], String> {
    match v {
        Value::List(l) => Ok(l),
        other => Err(format!("{} is not a list", serde_json::to_string(other).unwrap())),
    }
}

pub fn array<T, const N: usize>(v: Vec<T>) -> Result<[T; N], String> {
    let len = v.len();
    v.try_into().map_err(|_| format!("{len} elements, the table has {N}"))
}

pub fn record(v: &Value) -> Result<&Values, String> {
    match v {
        Value::Record(r) => Ok(r),
        other => Err(format!("{} is not a record", serde_json::to_string(other).unwrap())),
    }
}

/// `r`'s fields, which must be exactly `names`.
pub fn fields<'r>(r: &'r Values, names: &[&str]) -> Result<&'r Values, String> {
    if let Some(k) = r.keys().find(|k| !names.contains(&k.as_str())) {
        return Err(format!("{k}: no such field"));
    }
    if let Some(k) = names.iter().find(|k| !r.contains_key(**k)) {
        return Err(format!("{k}: missing"));
    }
    Ok(r)
}

pub fn field<'r>(r: &'r Values, name: &str) -> &'r Value {
    &r[name]
}

/// Prefixes an error with the field it is in.
pub fn at(name: &'static str) -> impl Fn(String) -> String {
    move |e| format!("{name}: {e}")
}

/// Prefixes an error with the element it is in.
pub fn index(i: usize) -> impl Fn(String) -> String {
    move |e| format!("[{i}]: {e}")
}

/// A record's value from its fields.
pub fn make_record<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Record(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// A record's fields.
pub fn fields_of(v: Value) -> Values {
    match v {
        Value::Record(r) => r,
        _ => Values::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adds `by` to `count` while `on`.
    struct Count;

    impl Mechanic for Count {
        const ID: &'static str = "demo.count";
        const VERSION: u32 = 1;
        const FINGERPRINT: &'static str = "f";
        const TOLERANCE: Option<&'static [(&'static str, i128)]> = None;
        type State = i32;
        type Inputs = bool;
        type Params = i32;
        fn state(v: &Values) -> Result<i32, String> {
            int(field(fields(v, &["count"])?, "count")).map_err(at("count"))
        }
        fn inputs(v: &Values) -> Result<bool, String> {
            boolean(field(fields(v, &["on"])?, "on")).map_err(at("on"))
        }
        fn params(v: &Values) -> Result<i32, String> {
            int(field(fields(v, &["by"])?, "by")).map_err(at("by"))
        }
        fn record(s: &i32) -> Values {
            fields_of(make_record([("count", Value::Int(*s as i128))]))
        }
        fn step(count: &mut i32, on: &bool, by: &i32, calls: &mut Calls) {
            if *on {
                *count += by;
            }
            // Asks the world for a bonus past 12, and says when it took one.
            if *count > 12 {
                let r = calls.call("world.bonus@1", "bonus", Values::from([("count".into(), Value::Int(*count as i128))]));
                let b: i32 = int(field(fields(r, &["value"]).unwrap_or_else(|e| calls.bad(e)), "value"))
                    .unwrap_or_else(|e| calls.bad(e));
                *count += b;
                calls.call("world.bonus@1", "took", Values::from([("bonus".into(), Value::Int(b as i128))]));
            }
        }
    }

    fn trace(header: &str, steps: &[&str]) -> Trace {
        let h = format!(
            r#"{{"format":"mechanic-trace","format_version":1,"mechanic":"demo.count","mechanic_version":1,"source":{{"kind":"synthetic"}},"producer":{{"name":"test","version":"1"}},"params":{{"by":2}},"initial_state":{{"count":0}}{header}}}"#
        );
        Trace::parse(&([h.as_str()].iter().chain(steps).copied().collect::<Vec<_>>().join("\n"))).unwrap()
    }

    #[test]
    fn a_trace_replays_through_external_changes() {
        let steps = [
            r#"{"step":0,"inputs":{"on":true},"state":{"count":2}}"#,
            r#"{"step":1,"inputs":{"on":false}}"#,
            r#"{"step":2,"inputs":{"on":true},"state_before":{"count":10},"state":{"count":12}}"#,
        ];
        assert_eq!(super::trace::<Count>(&trace(r#","fingerprint":"f""#, &steps)), Ok(()));
    }

    #[test]
    fn the_first_difference_is_named() {
        let steps = [r#"{"step":0,"inputs":{"on":true},"state":{"count":3}}"#];
        assert_eq!(super::trace::<Count>(&trace("", &steps)), Err("step 0: count is 2, the trace has 3".into()));
        let steps = [r#"{"step":0,"inputs":{"on":1}}"#];
        assert_eq!(super::trace::<Count>(&trace("", &steps)), Err("step 0: inputs: on: 1 is not a bool".into()));
    }

    #[test]
    fn calls_are_answered_from_the_trace_and_checked() {
        let bonus = |count: i32, b: i32| {
            format!(
                r#"[{{"interface":"world.bonus@1","call":"bonus","request":{{"count":{count}}},"response":{{"value":{b}}}}},{{"interface":"world.bonus@1","call":"took","request":{{"bonus":{b}}}}}]"#
            )
        };
        let steps = [
            format!(r#"{{"step":0,"inputs":{{"on":true}},"state_before":{{"count":11}},"calls":{},"state":{{"count":16}}}}"#, bonus(13, 3)),
        ];
        let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
        assert_eq!(super::trace::<Count>(&trace("", &steps)), Ok(()));
        // The step asks with another request.
        let other = [format!(
            r#"{{"step":0,"inputs":{{"on":true}},"state_before":{{"count":11}},"calls":{},"state":{{"count":16}}}}"#,
            bonus(14, 3)
        )];
        let other: Vec<&str> = other.iter().map(String::as_str).collect();
        assert_eq!(
            super::trace::<Count>(&trace("", &other)),
            Err("step 0: call 0 (world.bonus@1.bonus) request.count is 13, the trace has 14".into())
        );
        // A call the trace has and the step does not make.
        let more = [format!(r#"{{"step":0,"inputs":{{"on":false}},"calls":{}}}"#, bonus(13, 3))];
        let more: Vec<&str> = more.iter().map(String::as_str).collect();
        assert_eq!(
            super::trace::<Count>(&trace("", &more)),
            Err("step 0: the step made 0 calls, the trace has 2: the next is world.bonus@1.bonus".into())
        );
        // A response the step cannot read.
        let bad = [r#"{"step":0,"inputs":{"on":true},"state_before":{"count":11},"calls":[{"interface":"world.bonus@1","call":"bonus","request":{"count":13},"response":{"v":1}}]}"#];
        assert_eq!(super::trace::<Count>(&trace("", &bad)), Err("step 0: call 0 response: v: no such field".into()));
    }

    #[test]
    fn a_trace_of_other_fields_is_refused() {
        let e = super::trace::<Count>(&trace(r#","fingerprint":"g""#, &[])).unwrap_err();
        assert!(e.starts_with("captured with fingerprint g, the mechanic's fields now give f"), "{e}");
    }

    #[test]
    fn differences_inside_tables_and_records_are_named_by_path() {
        let rec = |n| Value::Record([("x".to_string(), Value::List(vec![Value::Int(1), Value::Int(n)]))].into());
        assert_eq!(differs("s", Some(&rec(2)), Some(&rec(3)), 0), Some("s.x[1] is 2, the trace has 3".into()));
        assert_eq!(differs("n", Some(&Value::Int(5)), Some(&Value::Int(7)), 2), None);
    }
}

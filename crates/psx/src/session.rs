//! A machine run as a session: where it started, what drives its pad, the
//! observers watching it, and one loop that advances it (docs/run-plan.md).
//!
//! The loop counts run fields from the session's start. At each field
//! boundary (and at the start) the observers look first, in the order they
//! were attached, then the driver sets the pad, through the adjustments
//! when there are any. Between boundaries the machine runs a whole field at
//! a time unless an observer or the adjustments need every instruction, or
//! the binding's tick is watched; then it steps, and each observer sees
//! each step and each tick's end, then the adjustments do.

use std::time::{Duration, Instant};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use serde_json::{Map, Value};

use crate::adjust::Running;

/// What an observer wants after it looked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Go,
    /// End the advance here, at this boundary; the reason is the run's termination.
    Stop(&'static str),
}

/// Why an advance ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The frame limit was reached.
    Limit,
    /// An observer stopped it at a boundary.
    Stopped(&'static str),
    /// The wall-clock deadline passed.
    Timeout,
}

impl Ended {
    pub fn termination(self) -> &'static str {
        match self {
            Ended::Limit => "frames_reached",
            Ended::Stopped(r) => r,
            Ended::Timeout => "timeout",
        }
    }
}

/// Where an observer writes what it saw: files in the run directory and its
/// section of the report.
pub struct Out<'a> {
    pub dir: &'a std::path::Path,
    pub files: &'a mut Map<String, Value>,
    pub report: &'a mut Map<String, Value>,
    /// The absolute frame the session started at.
    pub start: u64,
    pub termination: &'static str,
}

impl Out<'_> {
    /// Write `bytes` to `name` in the run directory and list it with its sha256.
    pub fn put(&mut self, name: &str, bytes: &[u8]) {
        let p = self.dir.join(name);
        if std::fs::write(&p, bytes).is_ok() {
            self.files
                .insert(name.into(), serde_json::json!({"path": p, "sha256": psx_media::disc::sha256_bytes(bytes)}));
        }
    }

    pub fn section(&mut self, key: &str, v: Value) {
        self.report.insert(key.into(), v);
    }
}

/// Something that watches (or, like pokes, intervenes in) a session.
pub trait Observer {
    /// Whether it needs to see every instruction.
    fn per_step(&self) -> bool {
        false
    }
    /// A field boundary, or the session's start: `rel` is the run field.
    fn field(&mut self, _rel: u64, _m: &mut Machine) -> crate::Result<Flow> {
        Ok(Flow::Go)
    }
    /// The instruction `info` just ran; a stop ends the advance after it
    /// (and after the tick it ended, if it ended one).
    fn step(&mut self, _info: &StepInfo, _m: &mut Machine) -> Flow {
        Flow::Go
    }
    /// The binding's tick ended in run field `rel`.
    fn tick(&mut self, _rel: u64, _m: &mut Machine) -> Flow {
        Flow::Go
    }
    /// The run's last boundary, the state its files capture; `complete` is
    /// false when it ended early (a timeout).
    fn last_field(&mut self, _rel: u64, _m: &mut Machine, _complete: bool) -> crate::Result<()> {
        Ok(())
    }
    /// Its files and its section of the report.
    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()>;
}

/// The pad, as a driver sets it: through the adjustments (latch holds it
/// for where the tick ends), or straight onto the controller.
pub struct Pad<'a> {
    pub m: &'a mut Machine,
    adjust: Option<&'a mut Running>,
}

impl Pad<'_> {
    pub fn apply(&mut self, i: &crate::replay::Input) {
        match self.adjust.as_deref_mut() {
            Some(a) => a.input(self.m, i),
            None => crate::replay::apply(self.m, i),
        }
    }
}

/// What sets the pad.
pub trait Driver {
    /// Run field `rel` begins (after the observers looked), with `ticks`
    /// ticks ended since the session's start.
    fn field(&mut self, rel: u64, ticks: u64, pad: &mut Pad);
    /// Tick `ticks` (counted from the session's start) ended.
    fn tick(&mut self, _ticks: u64) -> Flow {
        Flow::Go
    }
    /// What it applied, for the report.
    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
    /// Where it stands, for a debugging session's checkpoint (null when it
    /// keeps nothing).
    fn save(&self) -> Value {
        Value::Null
    }
}

/// No input: the pad stays as the machine has it.
pub struct NoInput;

impl Driver for NoInput {
    fn field(&mut self, _rel: u64, _ticks: u64, _pad: &mut Pad) {}
}

/// Complete pad states by run field: each from its field on.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ByField {
    pub events: Vec<crate::replay::Input>,
    pub next: usize,
}

impl Driver for ByField {
    fn field(&mut self, rel: u64, _ticks: u64, pad: &mut Pad) {
        while let Some(e) = self.events.get(self.next).filter(|e| e.frame <= rel) {
            pad.apply(e);
            self.next += 1;
        }
    }

    fn save(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// A recording's ticks: until a tick ends, the pad is the one that tick
/// ended with, as the port's replay has it; the session stops when the last
/// one ends.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ticks {
    pub recording: String,
    pub pads: Vec<crate::replay::Input>,
}

impl Driver for Ticks {
    fn field(&mut self, _rel: u64, ticks: u64, pad: &mut Pad) {
        if let Some(i) = self.pads.get(ticks as usize) {
            pad.apply(i);
        }
    }

    fn tick(&mut self, ticks: u64) -> Flow {
        if ticks >= self.pads.len() as u64 { Flow::Stop("recording_ended") } else { Flow::Go }
    }

    fn save(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn finish(self: Box<Self>, _m: &Machine, out: &mut Out) -> crate::Result<()> {
        out.section("input", serde_json::json!({"recording": self.recording, "ticks": self.pads.len(),
            "application": {"observed": true, "by": "tick",
                "note": "each tick's pad from the recording, on the controller until that tick ends (under latch, where the tick before it ends)"}}));
        Ok(())
    }
}

/// A person at the pad: the state they hold now, applied at the field
/// boundary when it changed. Whoever runs the session sets it before each
/// advance through the handle [`Person::new`] returns.
pub struct Person {
    pad: std::rc::Rc<std::cell::Cell<Option<crate::replay::Input>>>,
}

impl Person {
    pub fn new() -> (Person, std::rc::Rc<std::cell::Cell<Option<crate::replay::Input>>>) {
        let pad = std::rc::Rc::new(std::cell::Cell::new(None));
        (Person { pad: pad.clone() }, pad)
    }
}

impl Driver for Person {
    fn field(&mut self, _rel: u64, _ticks: u64, pad: &mut Pad) {
        if let Some(i) = self.pad.take() {
            pad.apply(&i);
        }
    }
}

/// States along a session's route, one every `every` run fields and the
/// last `keep` of them, each with what its owner needs to go on from it.
pub struct Checkpoints<T> {
    every: u64,
    keep: usize,
    held: std::collections::VecDeque<(u64, Vec<u8>, T)>,
}

impl<T: Clone> Checkpoints<T> {
    pub fn new(every: u64, keep: usize) -> Self {
        Checkpoints { every, keep, held: Default::default() }
    }

    /// Take one here when it is due (the first at once).
    pub fn offer(&mut self, s: &Session, media: Option<psx_core::state::MediaId>, with: impl FnOnce() -> T) {
        let rel = s.rel();
        if self.held.back().is_none_or(|h| rel >= h.0 + self.every) {
            if self.held.len() == self.keep {
                self.held.pop_front();
            }
            self.held.push_back((rel, s.m.save_state(media), with()));
        }
    }

    /// The latest at least `min` fields before run field `now`, dropping
    /// those after it: its field, state and what came with it.
    pub fn back(&mut self, now: u64, min: u64) -> Option<(u64, Vec<u8>, T)> {
        let i = self.held.iter().rposition(|h| h.0 + min <= now)?;
        self.held.truncate(i + 1);
        Some(self.held[i].clone())
    }

    pub fn clear(&mut self) {
        self.held.clear();
    }
}

/// How often the loop checks the deadline, in instructions.
const CHECK: u64 = 1 << 20;

/// How a recording's lead-in ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeadIn {
    /// The first tick ended: the run's fields count from the next.
    Ticked,
    /// No tick ended before the limit.
    NoTick,
    /// The deadline passed.
    Timeout,
}

/// Observers and drivers may borrow what outlives the session (`'a`): a
/// recorder its binding and the recording it fills.
pub struct Session<'a> {
    pub m: Machine,
    /// The absolute frame the session started at.
    pub start: u64,
    pub observers: Vec<Box<dyn Observer + 'a>>,
    pub driver: Box<dyn Driver + 'a>,
    /// The binding's tick, when something watches ticks.
    pub tick: Option<crate::binding::Tick>,
    /// The adjustments while they run.
    pub adjust: Option<Running>,
    /// Instructions run.
    pub steps: u64,
    /// Ticks ended since the start.
    pub ticks: u64,
    /// The last field the loop saw begin (absolute).
    last_field: u64,
    /// The driver has yet to set the pad for the field that began.
    driver_due: bool,
}

/// Where a session stands besides its machine, its driver and its
/// adjustments: what a debugging session's checkpoint keeps with them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Place {
    pub start: u64,
    pub steps: u64,
    pub ticks: u64,
    pub last_field: u64,
    pub driver_due: bool,
    pub tick: Option<crate::binding::Tick>,
}

impl<'a> Session<'a> {
    pub fn new(m: Machine, start: u64, driver: Box<dyn Driver + 'a>) -> Session<'a> {
        Session {
            m,
            start,
            observers: Vec::new(),
            driver,
            tick: None,
            adjust: None,
            steps: 0,
            ticks: 0,
            last_field: u64::MAX,
            driver_due: false,
        }
    }

    pub fn place(&self) -> Place {
        Place {
            start: self.start,
            steps: self.steps,
            ticks: self.ticks,
            last_field: self.last_field,
            driver_due: self.driver_due,
            tick: self.tick.clone(),
        }
    }

    /// Stand at `p` again, the machine, driver and adjustments put back by the caller.
    pub fn set_place(&mut self, p: Place) {
        self.start = p.start;
        self.steps = p.steps;
        self.ticks = p.ticks;
        self.last_field = p.last_field;
        self.driver_due = p.driver_due;
        self.tick = p.tick;
    }

    /// Set the pad now, as a driver would.
    pub fn pad(&mut self, i: &crate::replay::Input) {
        Pad { m: &mut self.m, adjust: self.adjust.as_mut() }.apply(i);
    }

    pub fn observe(&mut self, o: impl Observer + 'a) {
        self.observers.push(Box::new(o));
    }

    /// The run field the machine is in: 0 until the start, after a lead-in.
    pub fn rel(&self) -> u64 {
        self.m.bus.frames.saturating_sub(self.start)
    }

    /// A recording's lead-in, from where the recording starts: the pad
    /// released until the first tick ends (tick 0, the moment the port's
    /// launcher builds), then the run's fields count from the next field.
    /// Ticks that end in the rest of this field are the run's, in field 0.
    /// Observers see each step, and the first tick's end as tick 0 of
    /// run field 0, before the adjustments do; the limit is absolute field
    /// `limit`.
    pub fn lead_in(&mut self, limit: u64, started: Instant, deadline: Duration) -> LeadIn {
        let Some(tick) = self.tick.clone() else { return LeadIn::NoTick };
        let mut last = self.m.bus.frames;
        loop {
            if self.m.bus.frames != last {
                last = self.m.bus.frames;
                crate::live::field(&self.m);
                if last >= limit {
                    return LeadIn::NoTick;
                }
                if started.elapsed() > deadline {
                    return LeadIn::Timeout;
                }
            }
            let info = self.m.step();
            self.steps += 1;
            if let Some(a) = &mut self.adjust {
                a.step(&info, &mut self.m);
            }
            for o in &mut self.observers {
                let _ = o.step(&info, &mut self.m);
            }
            if tick.ended_by(&info) {
                for o in &mut self.observers {
                    let _ = o.tick(0, &mut self.m);
                }
                if let Some(a) = &mut self.adjust {
                    a.tick_ended(0, &mut self.m);
                }
                self.start = self.m.bus.frames + 1;
                self.last_field = self.m.bus.frames;
                return LeadIn::Ticked;
            }
        }
    }

    /// Go back to `state`, one taken on this session: the machine keeps its
    /// disc and SPU capture, `prepare` sets it up as the session's start
    /// did (the agent's C), then the adjustments start again from it (a
    /// loaded machine is stock) and the tick count with them.
    pub fn restore(&mut self, state: &[u8], bios: Vec<u8>, prepare: impl FnOnce(&mut Machine)) -> Result<(), String> {
        let (_, mut m) = Machine::load_state(state, bios).map_err(|e| e.to_string())?;
        m.bus.cdrom.disc = self.m.bus.cdrom.disc.take();
        m.bus.spu.capture = self.m.bus.spu.capture;
        self.m = m;
        prepare(&mut self.m);
        if let Some(a) = &mut self.adjust {
            *a = a.restart(&mut self.m);
        }
        self.ticks = 0;
        // No field has ended under the restarted adjustments yet.
        self.last_field = u64::MAX;
        self.driver_due = false;
        Ok(())
    }

    /// Run until absolute frame `target`, an observer's stop, or `deadline`
    /// after `started`.
    pub fn advance(&mut self, target: u64, started: Instant, deadline: Duration) -> crate::Result<Ended> {
        let per_step = self.tick.is_some()
            || self.adjust.as_ref().is_some_and(Running::per_step)
            || self.observers.iter().any(|o| o.per_step());
        while self.m.bus.frames < target {
            let frame = self.m.bus.frames;
            if frame != self.last_field && frame >= self.start {
                // A field boundary, or the start: observe first, then intervene.
                // A field ended, unless this is where the session started or was restored.
                let ended = self.last_field != u64::MAX;
                self.last_field = frame;
                let rel = self.rel();
                crate::live::field(&self.m);
                if rel > 0 && ended {
                    if let Some(a) = &mut self.adjust {
                        a.field_ended(rel, &mut self.m);
                    }
                }
                for o in &mut self.observers {
                    if let Flow::Stop(why) = o.field(rel, &mut self.m)? {
                        // The driver's input for this field is still due.
                        self.driver_due = true;
                        return Ok(Ended::Stopped(why));
                    }
                }
                self.driver_due = true;
            }
            if self.driver_due {
                self.driver_due = false;
                let rel = self.rel();
                self.driver.field(rel, self.ticks, &mut Pad { m: &mut self.m, adjust: self.adjust.as_mut() });
            }
            if per_step {
                let info = self.m.step();
                self.steps += 1;
                if let Some(a) = &mut self.adjust {
                    a.step(&info, &mut self.m);
                }
                let mut stop = None;
                for o in &mut self.observers {
                    if let Flow::Stop(why) = o.step(&info, &mut self.m) {
                        stop = stop.or(Some(why));
                    }
                }
                if self.tick.as_ref().is_some_and(|t| t.ended_by(&info)) {
                    let rel = self.rel();
                    for o in &mut self.observers {
                        if let Flow::Stop(why) = o.tick(rel, &mut self.m) {
                            stop = stop.or(Some(why));
                        }
                    }
                    self.ticks += 1;
                    if let Some(a) = &mut self.adjust {
                        a.tick_ended(self.ticks, &mut self.m);
                    }
                    if let Flow::Stop(why) = self.driver.tick(self.ticks) {
                        stop = stop.or(Some(why));
                    }
                }
                if let Some(why) = stop {
                    return Ok(Ended::Stopped(why));
                }
            } else {
                self.steps += self.m.run_frame_steps(CHECK - self.steps % CHECK);
            }
            if self.steps.is_multiple_of(CHECK) && started.elapsed() > deadline {
                return Ok(Ended::Timeout);
            }
        }
        Ok(Ended::Limit)
    }

    /// The last boundary: the driver's input due there, then the observers'
    /// last look.
    pub fn last_field(&mut self, complete: bool) -> crate::Result<()> {
        let rel = self.rel();
        if complete {
            if let Some(a) = &mut self.adjust {
                a.field_ended(rel, &mut self.m);
            }
            self.driver.field(rel, self.ticks, &mut Pad { m: &mut self.m, adjust: self.adjust.as_mut() });
        }
        self.driver_due = false;
        for o in &mut self.observers {
            o.last_field(rel, &mut self.m, complete)?;
        }
        Ok(())
    }

    /// The driver's and each observer's files and report sections.
    pub fn finish(&mut self, out: &mut Out) -> crate::Result<()> {
        std::mem::replace(&mut self.driver, Box::new(NoInput)).finish(&self.m, out)?;
        for o in std::mem::take(&mut self.observers) {
            o.finish(&self.m, out)?;
        }
        Ok(())
    }
}

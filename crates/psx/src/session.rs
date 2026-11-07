//! A machine run as a session: where it started, what drives its pad, the
//! observers watching it, and one loop that advances it (docs/run-plan.md).
//!
//! The loop counts run fields from the session's start. At each field
//! boundary (and at the start) the observers look first, in the order they
//! were attached, then the driver sets the pad. Between boundaries the
//! machine runs a whole field at a time unless an observer needs every
//! instruction, or the binding's tick is watched; then it steps, and each
//! observer sees each step and each tick's end.

use std::time::{Duration, Instant};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use serde_json::{Map, Value};

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
    /// The instruction `info` just ran.
    fn step(&mut self, _info: &StepInfo, _m: &mut Machine) {}
    /// The binding's tick ended in run field `rel`.
    fn tick(&mut self, _rel: u64, _m: &mut Machine) {}
    /// The run's last boundary, the state its files capture; `complete` is
    /// false when it ended early (a timeout).
    fn last_field(&mut self, _rel: u64, _m: &mut Machine, _complete: bool) -> crate::Result<()> {
        Ok(())
    }
    /// Its files and its section of the report.
    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()>;
}

/// What sets the pad.
pub trait Driver {
    /// Run field `rel` begins (after the observers looked).
    fn field(&mut self, rel: u64, m: &mut Machine);
    /// What it applied, for the report.
    fn finish(self: Box<Self>, _m: &Machine, _out: &mut Out) -> crate::Result<()> {
        Ok(())
    }
}

/// No input: the pad stays as the machine has it.
pub struct NoInput;

impl Driver for NoInput {
    fn field(&mut self, _rel: u64, _m: &mut Machine) {}
}

/// How often the loop checks the deadline, in instructions.
const CHECK: u64 = 1 << 20;

pub struct Session {
    pub m: Machine,
    /// The absolute frame the session started at.
    pub start: u64,
    pub observers: Vec<Box<dyn Observer>>,
    pub driver: Box<dyn Driver>,
    /// The binding's tick, when something watches ticks.
    pub tick: Option<crate::binding::Tick>,
    /// Instructions run.
    pub steps: u64,
    last_rel: u64,
}

impl Session {
    pub fn new(m: Machine, start: u64, driver: Box<dyn Driver>) -> Session {
        Session { m, start, observers: Vec::new(), driver, tick: None, steps: 0, last_rel: u64::MAX }
    }

    pub fn observe(&mut self, o: impl Observer + 'static) {
        self.observers.push(Box::new(o));
    }

    /// The run field the machine is in.
    pub fn rel(&self) -> u64 {
        self.m.bus.frames - self.start
    }

    /// Run until absolute frame `target`, an observer's stop, or `deadline`
    /// after `started`.
    pub fn advance(&mut self, target: u64, started: Instant, deadline: Duration) -> crate::Result<Ended> {
        let per_step = self.tick.is_some() || self.observers.iter().any(|o| o.per_step());
        while self.m.bus.frames < target {
            let rel = self.rel();
            if rel != self.last_rel {
                // A field boundary, or the start: observe first, then intervene.
                self.last_rel = rel;
                crate::live::field(&self.m);
                for o in &mut self.observers {
                    if let Flow::Stop(why) = o.field(rel, &mut self.m)? {
                        return Ok(Ended::Stopped(why));
                    }
                }
                self.driver.field(rel, &mut self.m);
            }
            if per_step {
                let info = self.m.step();
                for o in &mut self.observers {
                    o.step(&info, &mut self.m);
                }
                if self.tick.as_ref().is_some_and(|t| t.ended_by(&info)) {
                    let rel = self.rel();
                    for o in &mut self.observers {
                        o.tick(rel, &mut self.m);
                    }
                }
                self.steps += 1;
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
            self.driver.field(rel, &mut self.m);
        }
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

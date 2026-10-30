//! Mechanics (docs/mechanics.md): the PlayStation's vocabulary (R3000
//! locations, overlay-qualified PCs, the bracket a Rust binding
//! describes), `psx mechanic check` and `psx mechanic capture`. The rest of
//! the mechanic formats are platform-free, in psx-mechanics.

use psx_mechanics::interface::CallKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::commands::{Ctx, Op, Outcome};

mod audit;
pub mod capture;
pub mod plugin;
pub mod workspace;

pub use capture::MechanicCaptureArgs;

/// The PlayStation producer.
pub struct Psx;

/// Where a value is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Location {
    /// Main RAM (any segment) or the scratchpad (0x1F800000).
    Address { address: u32 },
    /// Relative to a register's value latched at the bracket's begin.
    Base { base: Reg, offset: i32 },
    /// A CPU register: at begin (inputs, params), at a call's entry
    /// (request) or return (response).
    Register { register: Reg },
    /// A GTE register: data 0 to 31 as `mfc2` numbers them, control 32 to 63
    /// (`cfc2` n is 32 + n).
    Gte { gte: u8 },
}

/// A CPU register, by its conventional name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[rustfmt::skip]
pub enum Reg {
    Zero, At, V0, V1, A0, A1, A2, A3,
    T0, T1, T2, T3, T4, T5, T6, T7,
    S0, S1, S2, S3, S4, S5, S6, S7,
    T8, T9, K0, K1, Gp, Sp, Fp, Ra,
    Hi, Lo,
}

/// A PC, with the overlay it is in when it is not in resident code. The
/// overlay is named as `psx inventory` names it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pc {
    pub pc: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,
}

/// How capture sees an interface call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum CallSite {
    /// A routine entered at this PC: the request is read at entry, the
    /// response when it returns to its caller.
    Entry { entry: Pc },
    /// A load at this PC that asks the world inline (a table lookup): the
    /// request and response are read once it has loaded.
    Read { read: Pc },
}

/// Where a Rust binding says the step is (`psx_capture::Bracket`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub begin: Pc,
    pub end: End,
    /// Reads the step makes that the binding deliberately leaves out, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explained: Vec<Explained>,
    /// The interface calls the step makes, in the binding's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Called>,
}

/// An interface call the binding declares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Called {
    /// `<id>@<version>`.
    pub interface: String,
    pub call: String,
    pub kind: CallKind,
    pub on: CallSite,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum End {
    /// When this PC runs.
    At(Pc),
    /// `next_begin`: at the next begin. `frame`: at the next vblank.
    Mode(EndMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndMode {
    NextBegin,
    Frame,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Explained {
    /// Where: `bytes` from this location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u32>,
    /// Or by whom: every load this instruction makes, wherever it reads
    /// (level data whose address changes from level to level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Pc>,
    pub why: String,
}

const RAM: u32 = 0x20_0000;
const SCRATCHPAD: std::ops::Range<u32> = 0x1F80_0000..0x1F80_0400;

/// Main RAM in any segment, or the scratchpad.
fn in_memory(a: u32) -> bool {
    [0, 0x8000_0000, 0xA000_0000].iter().any(|s| (*s..*s + RAM).contains(&a)) || SCRATCHPAD.contains(&a)
}

fn check_pc(pc: u32, overlay: &Option<String>) -> Result<(), String> {
    if !pc.is_multiple_of(4) || !in_memory(pc) || SCRATCHPAD.contains(&pc) {
        return Err(format!("pc {pc:#010x}: a word-aligned address in main RAM"));
    }
    if overlay.as_deref().is_some_and(|o| o.trim().is_empty()) {
        return Err("overlay: an inventory overlay's name, or left out for resident code".into());
    }
    Ok(())
}

impl Reg {
    /// Its conventional name: `a0`.
    pub fn name(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
    }

    /// Register `n` (0-33) as the R3000 numbers them, hi and lo last.
    pub fn from_number(n: u8) -> Reg {
        reg_name(psx_capture::Reg(n).name()).expect("0 to 33")
    }
}

fn reg_name(name: &str) -> Option<Reg> {
    serde_json::from_value(serde_json::Value::from(name)).ok()
}

/// What ends one tick of the game's logic: a store to `at`, of `bytes`
/// bytes, made by the instruction at `pc` when one is given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tick {
    pub at: u32,
    pub bytes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc: Option<u32>,
}

impl Psx {
    /// `source.platform`.
    pub const NAME: &'static str = "psx";

    /// What is wrong with an explained read's location.
    fn check_explained(at: &Location) -> Result<(), String> {
        match at {
            Location::Base { base: Reg::Zero, .. } | Location::Register { register: Reg::Zero } => {
                Err("zero is always 0: explain an address instead".into())
            }
            Location::Gte { gte } if *gte > 63 => Err(format!("gte {gte}: 0 to 63")),
            _ => Ok(()),
        }
    }

    pub fn check_call_site(on: &CallSite) -> Result<(), String> {
        match on {
            CallSite::Entry { entry: p } | CallSite::Read { read: p } => check_pc(p.pc, &p.overlay),
        }
    }

    pub fn check_tick(t: &Tick, problems: &mut Vec<String>) {
        if !in_memory(t.at) {
            problems.push(format!("tick.at {:#010x}: main RAM or the scratchpad", t.at));
        }
        if ![1, 2, 4].contains(&t.bytes) || !t.at.is_multiple_of(t.bytes) {
            problems.push("tick.bytes: 1, 2 or 4, with at aligned to it".into());
        }
        if let Some(pc) = t.pc {
            if let Err(e) = check_pc(pc, &None) {
                problems.push(format!("tick.{e}"));
            }
        }
    }

    pub fn check_capture(c: &Capture, problems: &mut Vec<String>) {
        if let Err(e) = check_pc(c.begin.pc, &c.begin.overlay) {
            problems.push(format!("begin: {e}"));
        }
        if let End::At(p) = &c.end {
            if let Err(e) = check_pc(p.pc, &p.overlay) {
                problems.push(format!("end: {e}"));
            }
        }
        for (i, x) in c.explained.iter().enumerate() {
            // Any address: a hardware register or the BIOS ROM is read but
            // never bound, so explaining it is the only way to account for it.
            match (&x.at, x.bytes, &x.by) {
                (Some(Location::Address { .. }), Some(1..), None) => {}
                (Some(at), Some(1..), None) => {
                    if let Err(e) = Psx::check_explained(at) {
                        problems.push(format!("explained[{i}]: {e}"));
                    }
                }
                (None, None, Some(p)) => {
                    if let Err(e) = check_pc(p.pc, &p.overlay) {
                        problems.push(format!("explained[{i}].by: {e}"));
                    }
                }
                _ => problems.push(format!("explained[{i}]: at and bytes, or by (a reading PC)")),
            }
            if x.why.trim().is_empty() {
                problems.push(format!("explained[{i}]: why"));
            }
        }
        for (i, c) in c.calls.iter().enumerate() {
            if let Err(e) = Psx::check_call_site(&c.on) {
                problems.push(format!("calls[{i}] ({}.{}): {e}", c.interface, c.call));
            }
        }
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MechanicCheckArgs {
    /// The mechanic (`mechanics/<ID>/`); default every one.
    pub id: Option<String>,
}

impl Op for MechanicCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report((|| {
            let ids = match self.id {
                Some(id) => vec![id],
                None => workspace::ids(&ctx.root),
            };
            let (_, interface_problems) = workspace::interfaces(&ctx.root)?;
            let mut ok = interface_problems.is_empty();
            let mut mechanics = Vec::new();
            for id in ids {
                let l = workspace::load(&ctx.root, &id)?;
                let traces = workspace::traces(&l)?;
                let problems: Vec<String> =
                    l.problems.iter().filter(|p| !p.starts_with("interfaces/")).cloned().collect();
                ok &= problems.is_empty() && traces.iter().all(|(_, p, _)| p.is_empty());
                let traces: Vec<_> = traces
                    .into_iter()
                    .map(|(name, problems, h)| {
                        let mut t = json!({"name": name, "problems": problems});
                        if let Some(a) = h.as_ref().and_then(|h| h.audit.as_ref()) {
                            t["audit"] = json!({"clear": a.unaccounted.is_empty(), "unaccounted": a.unaccounted.len()});
                        }
                        if let Some(c) = h.as_ref().and_then(|h| h.coverage.as_ref()) {
                            t["coverage"] = json!({"reached": c.reached, "total": c.total});
                        }
                        t
                    })
                    .collect();
                mechanics.push(json!({"id": id, "problems": problems, "traces": traces}));
            }
            Ok((ok, json!({"ok": ok, "interfaces": {"problems": interface_problems}, "mechanics": mechanics})))
        })())
    }
}

impl Op for MechanicCaptureArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(capture::capture(ctx, self))
    }
}

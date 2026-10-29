//! Mechanics (docs/mechanics.md): the PlayStation's binding vocabulary
//! (R3000 locations, overlay-qualified PCs and the capture bracket of
//! `binding.toml`), `psx mechanic check` and `psx mechanic capture`. The rest
//! of the mechanic formats are platform-free, in psx-mechanics.

use std::collections::BTreeMap;

use psx_mechanics::binding::{Endian, Platform, Role};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::commands::{Ctx, Op, Outcome};

pub mod capture;
pub mod workspace;

pub use capture::MechanicCaptureArgs;

/// The PlayStation producer.
pub struct Psx;

pub type Binding = psx_mechanics::BindingFor<Psx>;

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
    /// An inline read's (`on = { read }`) address (request) or loaded value
    /// (response).
    Load { load: Load },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Load {
    Address,
    Value,
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
    /// request is what is loaded from, the response what is loaded.
    Read { read: Pc },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub begin: Begin,
    pub end: End,
    /// Reads the step makes that the binding deliberately leaves out, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explained: Vec<Explained>,
}

/// A step begins when this PC runs, with every register filter matching.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Begin {
    pub pc: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,
    /// Register filters, `a0 = 0x801F0000`: the player's object rather than
    /// another that runs the same routine.
    #[serde(flatten)]
    pub filters: BTreeMap<String, u32>,
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
    pub at: Location,
    pub bytes: u32,
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
}

fn reg_name(name: &str) -> Option<Reg> {
    serde_json::from_value(serde_json::Value::from(name)).ok()
}

impl Platform for Psx {
    type Location = Location;
    type CallSite = CallSite;
    type Capture = Capture;
    const NAME: &'static str = "psx";
    const ENDIAN: Endian = Endian::Little;

    fn check_location(at: &Location, role: Role<'_, CallSite>) -> Result<(), String> {
        let inline = matches!(role, Role::Request(CallSite::Read { .. }) | Role::Response(CallSite::Read { .. }));
        match at {
            Location::Address { address } if !in_memory(*address) => {
                Err(format!("address {address:#010x}: main RAM or the scratchpad"))
            }
            Location::Base { base: Reg::Zero, .. } | Location::Register { register: Reg::Zero } => {
                Err("zero is always 0: bind an address instead".into())
            }
            Location::Register { .. } | Location::Gte { .. } if matches!(role, Role::State) => {
                Err("state lives in memory: registers are clobbered between steps (mark it unobserved if it is not in memory)".into())
            }
            Location::Gte { gte } if *gte > 63 => Err(format!("gte {gte}: 0 to 63")),
            Location::Load { load } => match (load, role) {
                (Load::Address, Role::Request(_)) | (Load::Value, Role::Response(_)) if inline => Ok(()),
                _ => Err("load is for an inline read's request (address) and response (value)".into()),
            },
            _ => Ok(()),
        }
    }

    fn check_call_site(on: &CallSite) -> Result<(), String> {
        match on {
            CallSite::Entry { entry: p } | CallSite::Read { read: p } => check_pc(p.pc, &p.overlay),
        }
    }

    fn check_capture(c: &Capture, problems: &mut Vec<String>) {
        if let Err(e) = check_pc(c.begin.pc, &c.begin.overlay) {
            problems.push(format!("capture.begin: {e}"));
        }
        for name in c.begin.filters.keys() {
            if reg_name(name).is_none_or(|r| r == Reg::Zero) {
                problems.push(format!("capture.begin.{name}: not a register (filters are a0 = 0x801F0000)"));
            }
        }
        if let End::At(p) = &c.end {
            if let Err(e) = check_pc(p.pc, &p.overlay) {
                problems.push(format!("capture.end: {e}"));
            }
        }
        for (i, x) in c.explained.iter().enumerate() {
            if !matches!(x.at, Location::Address { .. } | Location::Base { .. }) {
                problems.push(format!("capture.explained[{i}]: an address or base + offset"));
            } else if let Err(e) = Psx::check_location(&x.at, Role::Input) {
                problems.push(format!("capture.explained[{i}]: {e}"));
            }
            if x.bytes == 0 || x.why.trim().is_empty() {
                problems.push(format!("capture.explained[{i}]: bytes and why"));
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
                ok &= problems.is_empty() && traces.iter().all(|(_, p)| p.is_empty());
                let traces: Vec<_> =
                    traces.into_iter().map(|(name, problems)| json!({"name": name, "problems": problems})).collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use psx_mechanics::{Interface, Mechanic};

    const MECHANIC: &str = r#"
id = "croc.walk"
version = 1
title = "Walking"
summary = "Speed closes a quarter of the gap to four times the target each tick."
rate = { per = "tick", hz = 25 }
requires = ["world.floor-height@1"]
state.speed = { type = "fixed", signed = true, int_bits = 20, frac_bits = 12 }
inputs.stick = { type = "int", bits = 16, signed = false }
inputs.matrix = { type = "int", bits = 32, signed = false }
params.target = { type = "int", bits = 16, signed = false }
"#;

    const FLOOR: &str = r#"
id = "world.floor-height"
version = 1
title = "Floor height"
calls.height = { kind = "query", request.x = { type = "int", bits = 32, signed = true }, response.y = { type = "int", bits = 16, signed = true } }
"#;

    const BINDING: &str = r#"
mechanic = "croc.walk"
mechanic_version = 1
source = { platform = "psx", title = "Croc (Europe)", image_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" }

[state.speed]
parts = [{ at = { base = "a0", offset = 0x40 }, bytes = 4 }]

[inputs.stick]
parts = [{ at = { address = 0x800A5C10 }, bytes = 2 }]

[inputs.matrix]
parts = [{ at = { gte = 32 }, bytes = 4 }]

[params]
target = 35

[params_provenance]
target = "immediate at 0x800437F8"

[interfaces."world.floor-height@1".height]
on = { read = { pc = 0x80044000, overlay = "80100000 LEVEL.BIN" } }
request.x = { parts = [{ at = { load = "address" }, bytes = 4 }] }
response.y = { parts = [{ at = { load = "value" }, bytes = 2 }] }

[capture]
begin = { pc = 0x80043234, a0 = 0x801F0000 }
end = "next_begin"
explained = [{ at = { base = "a0", offset = 0x80 }, bytes = 0x20, why = "the animation, another mechanic" }]
"#;

    fn check(binding: &str) -> Vec<String> {
        let b: Binding = toml::from_str(binding).unwrap();
        let i = [Interface::parse(FLOOR).unwrap()];
        b.check::<Psx>(&Mechanic::parse(MECHANIC).unwrap(), &i)
    }

    #[test]
    fn an_r3000_binding_checks() {
        assert_eq!(check(BINDING), Vec::<String>::new());
        let b: Binding = toml::from_str(BINDING).unwrap();
        assert_eq!(b.capture.begin.filters, [("a0".to_string(), 0x801F_0000)].into());
        assert_eq!(b.capture.end, End::Mode(EndMode::NextBegin));
        let end_at = BINDING.replace("end = \"next_begin\"", "end = { pc = 0x800432F0 }");
        assert!(matches!(
            toml::from_str::<Binding>(&end_at).unwrap().capture.end,
            End::At(Pc { pc: 0x8004_32F0, overlay: None })
        ));
    }

    #[test]
    fn r3000_mistakes_are_named() {
        let bad = BINDING
            .replace("{ base = \"a0\", offset = 0x40 }", "{ register = \"v0\" }")
            .replace("{ address = 0x800A5C10 }", "{ address = 0x80200000 }")
            .replace("{ load = \"value\" }", "{ load = \"address\" }")
            .replace("a0 = 0x801F0000", "a9 = 1")
            .replace("on = { read =", "on = { entry =");
        assert_eq!(
            check(&bad),
            vec![
                "state.speed: parts[0]: state lives in memory: registers are clobbered between steps (mark it unobserved if it is not in memory)",
                "inputs.stick: parts[0]: address 0x80200000: main RAM or the scratchpad",
                "interfaces.\"world.floor-height@1\".height.request.x: parts[0]: load is for an inline read's request (address) and response (value)",
                "interfaces.\"world.floor-height@1\".height.response.y: parts[0]: load is for an inline read's request (address) and response (value)",
                "capture.begin.a9: not a register (filters are a0 = 0x801F0000)",
            ]
        );
        assert!(toml::from_str::<Binding>(&BINDING.replace("offset = 0x40", "offset = 0x40, extra = 1")).is_err());
    }

    #[test]
    fn the_documented_examples_check() {
        let doc = include_str!("../../../../docs/mechanics.md");
        let blocks: Vec<&str> = doc.split("```toml\n").skip(1).map(|b| b.split("```").next().unwrap()).collect();
        let (m, i) = (Mechanic::parse(blocks[0]).unwrap(), [Interface::parse(blocks[1]).unwrap()]);
        assert_eq!((m.check(&i), i[0].check()), (vec![], vec![]));
        let b: Binding = toml::from_str(&blocks[2].replace('…', &"0".repeat(64))).unwrap();
        assert_eq!(b.check::<Psx>(&m, &i), Vec::<String>::new());
    }

    #[test]
    fn the_binding_schema_is_current() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
        let stale = psx_mechanics::sync_schemas(
            &dir,
            &[("binding.schema.json", psx_mechanics::schema::<Binding>("binding.toml"))],
        );
        assert!(stale.is_empty(), "stale schemas {stale:?}: run with PSX_UPDATE_SCHEMAS=1");
    }
}

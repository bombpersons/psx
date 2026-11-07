//! The API the glossary's binding is written against (docs/scenarios.md).
//! The binding is a small `cdylib` crate: it says what ends a tick of the
//! game's logic and where each of the glossary's quantities is in the
//! machine. `psx scenario record` builds it, loads it, and asks it for the
//! quantities at the end of every tick; the emulator is psx's.
//!
//! In a port's workspace it is `glossary/binding/`, a member of the
//! workspace (root `Cargo.toml`), built with `cargo build -p glossary-binding`:
//!
//! ```toml
//! [package]
//! name = "glossary-binding"
//! version.workspace = true
//! edition.workspace = true
//!
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! glossary = { path = ".." }
//! psx-capture = { path = "../../vendor/psx-capture" }
//!
//! [build-dependencies]
//! psx-scenario = { path = "../../vendor/psx-scenario" }
//! ```
//!
//! Its `build.rs` writes the glue from the glossary's source:
//! `psx_scenario::rust::write_glossary_glue(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))`.
//! `src/lib.rs`:
//!
//! ```ignore
//! use glossary::Quantities;
//! use psx_capture::prelude::*;
//!
//! mod glue {
//!     include!(concat!(env!("OUT_DIR"), "/glue.rs"));
//! }
//!
//! struct Game;
//!
//! impl Glossary for Game {
//!     type Glue = glue::Glossary;
//!     const TITLE: &str = "A Game (Europe) SLES-01234";
//!     const IMAGE_SHA256: &str = "9f1c…";
//!
//!     fn tick(&self, t: &mut Tick) {
//!         t.store(0x800A_1C30, 4).by(0x8001_52A4);
//!     }
//!
//!     fn quantities(&self, m: &Machine) -> Quantities {
//!         let player = m.u32(0x800A_2F18);
//!         Quantities { player_x: m.i32(player + 0x24) }
//!     }
//! }
//!
//! psx_capture::export_glossary!(Game);
//! ```
//!
//! When the game's rules live in a script its own interpreter runs, the
//! binding can also say where the programs are ([`Glossary::programs`]),
//! so the bytes the game read of them count as coverage, and run code at
//! points of the interpreter ([`Glossary::hooks`], [`Glossary::hook`]) that
//! names the script position running and the way each decision went
//! (docs/script-coverage-design.md):
//!
//! ```ignore
//! fn programs(&self, p: &mut Programs, m: &Machine) {
//!     let base = m.u32(0x800D_8A20);
//!     p.program("croc", base, m.u32(base - 4));
//! }
//!
//! fn hooks(&self, h: &mut Hooks) {
//!     h.at(0x8002_CFF8).branch(0x8002_8190);
//! }
//!
//! fn hook(&self, at: Hook, m: &Machine, s: &mut Script) {
//!     let obj = m.reg(S0);
//!     let position = m.u32(obj) - m.u32(obj + 4);
//!     match at {
//!         Hook::At(_) => s.site("croc", position),
//!         Hook::Branch { taken, .. } => s.way("croc", position, taken as u32, 2),
//!     }
//! }
//! ```
//!
//! Once the script's format is decoded, the binding can also give each
//! program's map ([`Glossary::map`]): its statements, which of them decide
//! and between how many ways, and names for routines. Coverage then counts
//! statements run instead of bytes read, and decisions no hook reported:
//!
//! ```ignore
//! fn map(&self, name: &str, code: &[u8], map: &mut Map) {
//!     for s in decode(code) {
//!         map.statement(s.start, s.end);
//!         if let Some(ways) = s.ways {
//!             map.decision(s.start, ways);
//!         }
//!     }
//!     map.routine("jump_test", 0x1803, 0x18F1);
//! }
//! ```
//!
//! While a port parts from the original, the binding can also give probes
//! ([`Glossary::probes`]): values by any name, read when a tick ends,
//! never recorded or judged, so they come and go without touching the
//! glossary (docs/probes-plan.md):
//!
//! ```ignore
//! fn probes(&self, m: &Machine, p: &mut Probes) {
//!     let car = m.u32(0x800A_2F18);
//!     for w in 0..4 {
//!         p.probe(format!("wheel_load_{w}"), m.i16(car + 0x6A4 + w * 0x40));
//!     }
//! }
//! ```
//!
//! The boundary with psx is two C functions ([`abi`]), so a binding built
//! by another Rust compiler than psx's loads all the same. Values cross it
//! in the recording's JSON encoding.

pub mod abi;

/// The version of the boundary with psx. psx refuses a binding built
/// against another; `psx template update` brings the vendored crate to
/// psx's.
pub const ABI_VERSION: u32 = 6;

pub mod prelude {
    pub use crate::reg::*;
    pub use crate::{Glossary, Hook, Hooks, Machine, Map, Probes, Programs, Script, Tick};
}

/// A CPU register, numbered as the R3000 numbers them; `HI` and `LO` are 32
/// and 33.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reg(pub u8);

/// The registers by their conventional names.
pub mod reg {
    use super::Reg;
    pub const ZERO: Reg = Reg(0);
    pub const AT: Reg = Reg(1);
    pub const V0: Reg = Reg(2);
    pub const V1: Reg = Reg(3);
    pub const A0: Reg = Reg(4);
    pub const A1: Reg = Reg(5);
    pub const A2: Reg = Reg(6);
    pub const A3: Reg = Reg(7);
    pub const T0: Reg = Reg(8);
    pub const T1: Reg = Reg(9);
    pub const T2: Reg = Reg(10);
    pub const T3: Reg = Reg(11);
    pub const T4: Reg = Reg(12);
    pub const T5: Reg = Reg(13);
    pub const T6: Reg = Reg(14);
    pub const T7: Reg = Reg(15);
    pub const S0: Reg = Reg(16);
    pub const S1: Reg = Reg(17);
    pub const S2: Reg = Reg(18);
    pub const S3: Reg = Reg(19);
    pub const S4: Reg = Reg(20);
    pub const S5: Reg = Reg(21);
    pub const S6: Reg = Reg(22);
    pub const S7: Reg = Reg(23);
    pub const T8: Reg = Reg(24);
    pub const T9: Reg = Reg(25);
    pub const K0: Reg = Reg(26);
    pub const K1: Reg = Reg(27);
    pub const GP: Reg = Reg(28);
    pub const SP: Reg = Reg(29);
    pub const FP: Reg = Reg(30);
    pub const RA: Reg = Reg(31);
    pub const HI: Reg = Reg(32);
    pub const LO: Reg = Reg(33);
}

const REG_NAMES: [&str; 34] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra", "hi", "lo",
];

impl Reg {
    /// Its conventional name: `a0`.
    pub fn name(self) -> &'static str {
        REG_NAMES.get(self.0 as usize).copied().unwrap_or("?")
    }
}

/// The machine as psx sees it when a tick ends: memory, the CPU registers
/// and the GTE.
///
/// A read outside main RAM and the scratchpad stops the binding, and psx
/// reports it as the recording's refusal.
pub struct Machine<'h> {
    host: &'h abi::Host,
}

impl Machine<'_> {
    /// `out.len()` bytes from `addr`.
    pub fn bytes(&self, addr: u32, out: &mut [u8]) {
        // SAFETY: `host` is psx's, valid for the call; `out` is writable for its length.
        let ok = unsafe { (self.host.read)(self.host.ctx, addr, out.as_mut_ptr(), out.len() as u32) };
        if !ok {
            std::panic::panic_any(abi::ReadRefused);
        }
    }

    fn le<const N: usize>(&self, addr: u32) -> [u8; N] {
        let mut b = [0; N];
        self.bytes(addr, &mut b);
        b
    }

    pub fn u8(&self, addr: u32) -> u8 {
        self.le::<1>(addr)[0]
    }

    pub fn i8(&self, addr: u32) -> i8 {
        self.u8(addr) as i8
    }

    pub fn u16(&self, addr: u32) -> u16 {
        u16::from_le_bytes(self.le(addr))
    }

    pub fn i16(&self, addr: u32) -> i16 {
        i16::from_le_bytes(self.le(addr))
    }

    pub fn u32(&self, addr: u32) -> u32 {
        u32::from_le_bytes(self.le(addr))
    }

    pub fn i32(&self, addr: u32) -> i32 {
        i32::from_le_bytes(self.le(addr))
    }

    /// A CPU register.
    pub fn reg(&self, r: Reg) -> u32 {
        // SAFETY: as in `bytes`.
        unsafe { (self.host.reg)(self.host.ctx, r.0 as u32) }
    }

    /// A GTE register: data 0 to 31, control 32 to 63.
    pub fn gte(&self, n: u8) -> u32 {
        // SAFETY: as in `bytes`.
        unsafe { (self.host.gte)(self.host.ctx, n as u32) }
    }
}

/// A value in the recording's encoding: an integer (a fixed-point number's
/// underlying integer, a float's bit pattern), true or false, an enum
/// variant's name, a table's or list's elements, or a record's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i128),
    Variant(&'static str),
    List(Vec<Value>),
    Record(Vec<(&'static str, Value)>),
}

impl Value {
    /// JSON, as the recording writes it: integers outside ±2^53 as decimal
    /// strings.
    pub fn json(&self, out: &mut String) {
        const SAFE: i128 = 1 << 53;
        match self {
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(n) if (-SAFE..=SAFE).contains(n) => out.push_str(&n.to_string()),
            Value::Int(n) => out.push_str(&format!("\"{n}\"")),
            Value::Variant(v) => out.push_str(&string(v)),
            Value::List(l) => {
                out.push('[');
                for (i, v) in l.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.json(out);
                }
                out.push(']');
            }
            Value::Record(r) => {
                out.push('{');
                for (i, (k, v)) in r.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&string(k));
                    out.push(':');
                    v.json(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

macro_rules! int_values {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(n: $t) -> Value {
                Value::Int(n as i128)
            }
        }
    )*};
}
int_values!(i8, u8, i16, u16, i32, u32, i64, u64, isize, usize);

/// A JSON string.
fn string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The glossary's quantities (docs/scenarios.md): their fingerprint and
/// `Quantities` struct in the recording's encoding. Its implementation is
/// generated from the glossary's source by its binding's `build.rs`
/// (`psx_scenario::rust::write_glossary_glue`).
pub trait Quantities {
    /// The glossary's fingerprint, of the source the glue was generated from.
    const FINGERPRINT: &'static str;
    type Quantities;
    fn quantities(q: &Self::Quantities) -> Value;
}

/// What ends one tick of the game's logic: a store of `bytes` bytes to an
/// address, made by one instruction when it is given.
#[derive(Default)]
pub struct Tick {
    store: Option<(u32, u32)>,
    pc: Option<u32>,
}

impl Tick {
    /// A tick ends when the game stores `bytes` (1, 2 or 4) at `address`.
    pub fn store(&mut self, address: u32, bytes: u32) -> &mut Self {
        self.store = Some((address, bytes));
        self
    }

    /// Only the store the instruction at `pc` makes.
    pub fn by(&mut self, pc: u32) -> &mut Self {
        self.pc = Some(pc);
        self
    }

    fn json(&self) -> Result<String, &'static str> {
        let (at, bytes) = self.store.ok_or("the tick has no store: call t.store(address, bytes)")?;
        Ok(match self.pc {
            Some(pc) => format!("{{\"at\":{at},\"bytes\":{bytes},\"pc\":{pc}}}"),
            None => format!("{{\"at\":{at},\"bytes\":{bytes}}}"),
        })
    }
}

/// The glossary's binding on the PlayStation: what ends a tick of the
/// game's logic, and where each quantity is. The type is a unit struct,
/// exported with [`export_glossary!`].
pub trait Glossary {
    type Glue: Quantities;
    /// The release, in words.
    const TITLE: &'static str;
    /// The image's identity: `psx context`'s `resolved.image_sha256` for the media.
    const IMAGE_SHA256: &'static str;

    fn tick(&self, t: &mut Tick);

    /// The quantities, read when a tick ends.
    fn quantities(&self, m: &Machine) -> <Self::Glue as Quantities>::Quantities;

    /// The programs a script interpreter is running, each by a name that
    /// stays the same across runs, read where a recording starts and when
    /// each tick ends. The bytes the game reads of them count as coverage
    /// until the next tick says otherwise. None by default.
    fn programs(&self, _p: &mut Programs, _m: &Machine) {}

    /// The interpreter's instructions where [`Glossary::hook`] runs. None by
    /// default.
    fn hooks(&self, _h: &mut Hooks) {}

    /// At a hook: the script position running, and the way a decision went,
    /// worked out from the registers and memory as they are.
    fn hook(&self, _at: Hook, _m: &Machine, _s: &mut Script) {}

    /// The program `name`'s structure, decoded from its bytes `code`: asked
    /// once per recording for each program and bytes [`Glossary::programs`]
    /// names. Positions are offsets into `code`. None by default.
    fn map(&self, _name: &str, _code: &[u8], _map: &mut Map) {}

    /// Values to compare while chasing a divergence, read when a tick ends
    /// as the quantities are, by names of the agent's choosing: not the
    /// glossary's, not recorded, not judged (docs/probes-plan.md). None by
    /// default.
    fn probes(&self, _m: &Machine, _p: &mut Probes) {}
}

/// The probes read when a tick ends ([`Glossary::probes`]).
#[derive(Default)]
pub struct Probes {
    list: Vec<(String, Value)>,
}

impl Probes {
    /// The probe `name` is `value`, an integer or a bool. A name given
    /// again in the same tick makes a list, in order.
    pub fn probe(&mut self, name: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.list.push((name.into(), value.into()));
        self
    }

    fn json(&self) -> String {
        let mut names: Vec<&str> = Vec::new();
        for (n, _) in &self.list {
            if !names.contains(&n.as_str()) {
                names.push(n);
            }
        }
        let mut out = String::from("{");
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&string(n));
            out.push(':');
            let values: Vec<&Value> = self.list.iter().filter(|(m, _)| m == n).map(|(_, v)| v).collect();
            match values.as_slice() {
                [one] => one.json(&mut out),
                many => Value::List(many.iter().map(|v| (*v).clone()).collect()).json(&mut out),
            }
        }
        out.push('}');
        out
    }
}

/// A program's structure ([`Glossary::map`]).
#[derive(Default)]
pub struct Map {
    statements: Vec<(u32, u32)>,
    decisions: Vec<(u32, u32)>,
    routines: Vec<(String, u32, u32)>,
}

impl Map {
    /// A statement (an instruction of the script) is the bytes `start` to
    /// `end`, `end` excluded. It ran when a recording read any of them, or
    /// a hook named `start`; bytes in no statement are not coverage.
    pub fn statement(&mut self, start: u32, end: u32) -> &mut Self {
        self.statements.push((start, end));
        self
    }

    /// The statement at `at` decides between `ways` ways, which hooks name
    /// with [`Script::way`].
    pub fn decision(&mut self, at: u32, ways: u32) -> &mut Self {
        self.decisions.push((at, ways));
        self
    }

    /// The bytes `start` to `end` (excluded) are the routine `name`.
    pub fn routine(&mut self, name: &str, start: u32, end: u32) -> &mut Self {
        self.routines.push((name.to_string(), start, end));
        self
    }

    fn json(&self) -> String {
        let pairs = |v: &[(u32, u32)]| v.iter().map(|(a, b)| format!("[{a},{b}]")).collect::<Vec<_>>().join(",");
        let routines: Vec<String> = self.routines.iter().map(|(n, s, e)| format!("[{},{s},{e}]", string(n))).collect();
        format!(
            "{{\"statements\":[{}],\"decisions\":[{}],\"routines\":[{}]}}",
            pairs(&self.statements),
            pairs(&self.decisions),
            routines.join(",")
        )
    }
}

/// The programs running ([`Glossary::programs`]).
#[derive(Default)]
pub struct Programs {
    list: Vec<(String, u32, u32)>,
}

impl Programs {
    /// The program `name` is the `bytes` bytes at `address`. Positions in it
    /// are offsets from `address`.
    pub fn program(&mut self, name: &str, address: u32, bytes: u32) -> &mut Self {
        self.list.push((name.to_string(), address, bytes));
        self
    }

    fn json(&self) -> String {
        let items: Vec<String> = self.list.iter().map(|(n, a, b)| format!("[{},{a},{b}]", string(n))).collect();
        format!("[{}]", items.join(","))
    }
}

/// Where [`Glossary::hook`] runs ([`Glossary::hooks`]).
#[derive(Default)]
pub struct Hooks {
    at: Vec<u32>,
    branch: Vec<u32>,
}

impl Hooks {
    /// Once the instruction at `pc` has run.
    pub fn at(&mut self, pc: u32) -> &mut Self {
        self.at.push(pc);
        self
    }

    /// Once the conditional branch at `pc` has gone one way or the other,
    /// after its delay slot.
    pub fn branch(&mut self, pc: u32) -> &mut Self {
        self.branch.push(pc);
        self
    }

    fn json(&self) -> String {
        let list = |v: &[u32]| v.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        format!("{{\"at\":[{}],\"branch\":[{}]}}", list(&self.at), list(&self.branch))
    }
}

/// The hook that ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    /// The instruction at this pc ran ([`Hooks::at`]).
    At(u32),
    /// The branch at `pc` went: taken, or fell through ([`Hooks::branch`]).
    Branch { pc: u32, taken: bool },
}

/// What a hook says ran ([`Glossary::hook`]).
#[derive(Default)]
pub struct Script {
    out: String,
}

impl Script {
    /// The script position `position` of the program `name` ran.
    pub fn site(&mut self, name: &str, position: u32) {
        self.push(format!("[{},{position}]", string(name)));
    }

    /// The decision at `position` of the program `name`, one of `of` ways,
    /// went way `way` (from 0).
    pub fn way(&mut self, name: &str, position: u32, way: u32, of: u32) {
        assert!(way < of, "way {way} of {of}: a way is counted from 0, below of");
        self.push(format!("[{},{position},{way},{of}]", string(name)));
    }

    fn push(&mut self, item: String) {
        self.out.push(if self.out.is_empty() { '[' } else { ',' });
        self.out.push_str(&item);
    }

    fn json(self) -> String {
        if self.out.is_empty() { "[]".into() } else { self.out + "]" }
    }
}

/// Exports the glossary's binding, a unit struct implementing
/// [`Glossary`], as the C functions psx calls.
#[macro_export]
macro_rules! export_glossary {
    ($glossary:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn psx_capture_abi_version() -> u32 {
            $crate::ABI_VERSION
        }

        /// # Safety
        /// `host` is psx's, valid for the call.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn psx_capture_call(what: u32, host: *const $crate::abi::Host) -> i32 {
            // SAFETY: the caller's.
            unsafe { $crate::abi::call(&$glossary, what, host) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_ticks_are_json() {
        let mut s = String::new();
        Value::Record(vec![
            ("a", Value::Int(-3)),
            ("b", Value::Int(1 << 60)),
            ("c", Value::List(vec![Value::Bool(true), Value::Variant("on_ground")])),
        ])
        .json(&mut s);
        assert_eq!(s, r#"{"a":-3,"b":"1152921504606846976","c":[true,"on_ground"]}"#);

        let mut t = Tick::default();
        assert!(t.json().is_err());
        t.store(0x800A_1C30, 4).by(0x8001_52A4);
        assert_eq!(t.json().unwrap(), format!(r#"{{"at":{},"bytes":4,"pc":{}}}"#, 0x800A_1C30u32, 0x8001_52A4u32));
    }

    #[test]
    fn programs_hooks_and_script_marks_are_json() {
        let mut p = Programs::default();
        p.program("croc", 16, 4);
        assert_eq!(p.json(), r#"[["croc",16,4]]"#);
        let mut h = Hooks::default();
        assert_eq!(h.json(), r#"{"at":[],"branch":[]}"#);
        h.at(8).branch(12);
        assert_eq!(h.json(), r#"{"at":[8],"branch":[12]}"#);
        let mut s = Script::default();
        s.site("croc", 3);
        s.way("croc", 3, 1, 2);
        assert_eq!(s.json(), r#"[["croc",3],["croc",3,1,2]]"#);
        assert_eq!(Script::default().json(), "[]");
        let mut m = Map::default();
        m.statement(0, 3).statement(3, 4).decision(0, 2).routine("go", 0, 4);
        assert_eq!(m.json(), r#"{"statements":[[0,3],[3,4]],"decisions":[[0,2]],"routines":[["go",0,4]]}"#);
    }

    #[test]
    fn probes_are_json_a_repeated_name_a_list() {
        let mut p = Probes::default();
        assert_eq!(p.json(), "{}");
        p.probe("load", -3i16).probe("grip_0", 7u32).probe("load", 5i16).probe("slip", true);
        assert_eq!(p.json(), r#"{"load":[-3,5],"grip_0":7,"slip":true}"#);
    }
}

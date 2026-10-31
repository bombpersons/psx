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
//! The boundary with psx is two C functions ([`abi`]), so a binding built
//! by another Rust compiler than psx's loads all the same. Values cross it
//! in the recording's JSON encoding.

pub mod abi;

/// The version of the boundary with psx. psx refuses a binding built
/// against another; `psx template update` brings the vendored crate to
/// psx's.
pub const ABI_VERSION: u32 = 4;

pub mod prelude {
    pub use crate::reg::*;
    pub use crate::{Glossary, Machine, Tick};
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
}

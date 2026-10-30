//! The API a mechanic's binding is written against (docs/mechanics.md, "The
//! binding"). A binding is a small `cdylib` crate: it says where the step
//! begins and ends on the PlayStation, which calls of the routine are the
//! step, and how to read the mechanic's state, inputs and params from the
//! machine. `psx mechanic capture` builds it, loads it and asks it these
//! questions while it runs the game; the emulator, the read audit, coverage
//! and the trace are psx's.
//!
//! ```ignore
//! use psx_capture::prelude::*;
//! use croc_turn::{Inputs, State};
//!
//! mod glue {
//!     include!(concat!(env!("OUT_DIR"), "/glue.rs"));
//! }
//!
//! struct Turn;
//!
//! impl Binding for Turn {
//!     type Mechanic = glue::Mechanic;
//!     const TITLE: &str = "Croc - Legend of the Gobbos (Europe) SLES-00593";
//!     const IMAGE_SHA256: &str = "b30a…";
//!
//!     fn bracket(&self, b: &mut Bracket) {
//!         b.begin(0x8004_425C).end(0x8002_D024);
//!     }
//!
//!     fn state(&self, m: &Machine) -> State {
//!         State { heading: m.u16(m.latched(A0) + 0x36A), turn_speed: m.i32(0x8007_51D0) }
//!     }
//!     // inputs, params
//! }
//!
//! psx_capture::export!(Turn);
//! ```
//!
//! The boundary with psx is two C functions ([`abi`]), so a binding built
//! by another Rust compiler than psx's loads all the same. Values cross it
//! in the trace's JSON encoding.

pub mod abi;

/// The version of the boundary with psx. psx refuses a binding built
/// against another; `psx template update` brings the vendored crate to
/// psx's.
pub const ABI_VERSION: u32 = 2;

pub mod prelude {
    pub use crate::reg::*;
    pub use crate::{At, Binding, Bracket, Load, Machine, Pc, Site};
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

/// A PC, in resident code or in an overlay named as `psx inventory` names
/// it. A plain `u32` is a resident PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pc {
    pub pc: u32,
    pub overlay: Option<&'static str>,
}

impl Pc {
    /// A PC in an overlay: the step's code is there only while the overlay
    /// is loaded.
    pub fn in_overlay(overlay: &'static str, pc: u32) -> Pc {
        Pc { pc, overlay: Some(overlay) }
    }
}

impl From<u32> for Pc {
    fn from(pc: u32) -> Pc {
        Pc { pc, overlay: None }
    }
}

/// Where an explained read is: an address in main RAM or the scratchpad, an
/// offset from a register as latched at the step's begin, a CPU register,
/// or a GTE register (data 0 to 31 as `mfc2` numbers them, control 32 to 63).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Address(u32),
    Base(Reg, i32),
    Register(Reg),
    Gte(u8),
}

/// Where an interface call happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    /// A routine entered at this PC: the request is read as it is entered,
    /// the response when it returns to its caller. At both, `latched` is the
    /// registers as it was entered.
    Entry(Pc),
    /// A load at this PC that asks the world inline (a table lookup): the
    /// request and response are read once it has loaded, with
    /// [`Machine::load`] saying where it read.
    Load(Pc),
}

impl Site {
    pub fn entry(pc: impl Into<Pc>) -> Site {
        Site::Entry(pc.into())
    }

    pub fn load(pc: impl Into<Pc>) -> Site {
        Site::Load(pc.into())
    }
}

/// What the load at a [`Site::Load`] read: where, how many bytes, and the
/// value it gave (zero-extended), which is the only way to see a hardware
/// register's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Load {
    pub address: u32,
    pub bytes: u32,
    pub value: u32,
}

/// One call of an interface the mechanic requires, as the binding's glue
/// declares it from the interface's trait: a marker type per method.
pub trait Call {
    /// `<id>@<version>`: `world.floor-slope@1`.
    const INTERFACE: &'static str;
    /// The method: `slope_x`.
    const NAME: &'static str;
    /// The method's arguments: one type, or a tuple of them; `()` for none.
    type Request: 'static;
    /// What it returns; `()` for a command.
    type Response: 'static;
    fn request(r: &Self::Request) -> Value;
    fn response(r: &Self::Response) -> Value;
}

/// A call that returns something: [`Bracket::query`].
pub trait Query: Call {}

/// A call that returns nothing: [`Bracket::command`].
pub trait Command: Call<Response = ()> {}

type Reader = Box<dyn Fn(&Machine) -> Value>;

struct Hook {
    interface: &'static str,
    name: &'static str,
    site: Site,
    request: Reader,
    response: Option<Reader>,
}

/// Where the step is: its begin and end, the interface calls it makes, and
/// the reads the step makes that the binding deliberately leaves out, with
/// why.
#[derive(Default)]
pub struct Bracket {
    begin: Option<Pc>,
    end: Option<End>,
    explained: Vec<(Explained, &'static str)>,
    calls: Vec<Hook>,
}

enum End {
    At(Pc),
    NextBegin,
    Frame,
}

enum Explained {
    At(At, u32),
    By(Pc),
}

impl Bracket {
    /// The step begins when this PC is about to run (and `is_step` says
    /// this call is the step).
    pub fn begin(&mut self, pc: impl Into<Pc>) -> &mut Self {
        self.begin = Some(pc.into());
        self
    }

    /// The step ends when this PC is about to run.
    pub fn end(&mut self, pc: impl Into<Pc>) -> &mut Self {
        self.end = Some(End::At(pc.into()));
        self
    }

    /// The step ends where the next one begins.
    pub fn end_at_next_begin(&mut self) -> &mut Self {
        self.end = Some(End::NextBegin);
        self
    }

    /// The step ends at the next vblank.
    pub fn end_at_frame(&mut self) -> &mut Self {
        self.end = Some(End::Frame);
        self
    }

    /// The step reads `bytes` at `at`, and it is not behaviour: why.
    pub fn explain(&mut self, at: At, bytes: u32, why: &'static str) -> &mut Self {
        self.explained.push((Explained::At(at, bytes), why));
        self
    }

    /// Every load the instruction at `pc` makes, wherever it reads, is not
    /// behaviour (level data whose address changes from level to level): why.
    pub fn explain_reader(&mut self, pc: impl Into<Pc>, why: &'static str) -> &mut Self {
        self.explained.push((Explained::By(pc.into()), why));
        self
    }

    /// The step asks the world `Q` at `site`: `request` reads what it asks
    /// and `response` what the world answers.
    pub fn query<Q: Query>(
        &mut self,
        site: Site,
        request: fn(&Machine) -> Q::Request,
        response: fn(&Machine) -> Q::Response,
    ) -> &mut Self {
        self.calls.push(Hook {
            interface: Q::INTERFACE,
            name: Q::NAME,
            site,
            request: Box::new(move |m| Q::request(&request(m))),
            response: Some(Box::new(move |m| Q::response(&response(m)))),
        });
        self
    }

    /// The step tells the world `C` at `site`: `request` reads what it tells.
    pub fn command<C: Command>(&mut self, site: Site, request: fn(&Machine) -> C::Request) -> &mut Self {
        self.calls.push(Hook {
            interface: C::INTERFACE,
            name: C::NAME,
            site,
            request: Box::new(move |m| C::request(&request(m))),
            response: None,
        });
        self
    }

    /// Call `which`'s request or response, read from `m`, as JSON.
    pub(crate) fn read_call(&self, which: u32, response: bool, m: &Machine, out: &mut String) -> bool {
        let Some(h) = self.calls.get(which as usize) else { return false };
        match (response, &h.response) {
            (false, _) => (h.request)(m).json(out),
            (true, Some(r)) => r(m).json(out),
            (true, None) => return false,
        }
        true
    }

    /// The bracket as JSON: `begin`, `end`, `explained` and `calls`.
    fn json(&self) -> Result<String, &'static str> {
        let pc = |p: &Pc| match p.overlay {
            Some(o) => format!("{{\"pc\":{},\"overlay\":{}}}", p.pc, string(o)),
            None => format!("{{\"pc\":{}}}", p.pc),
        };
        let begin = self.begin.as_ref().ok_or("the bracket has no begin: call b.begin(pc)")?;
        let end = match self
            .end
            .as_ref()
            .ok_or("the bracket has no end: call b.end(pc), or end at the next begin or frame")?
        {
            End::At(p) => pc(p),
            End::NextBegin => "\"next_begin\"".into(),
            End::Frame => "\"frame\"".into(),
        };
        let explained: Vec<String> = self
            .explained
            .iter()
            .map(|(e, why)| match e {
                Explained::At(at, bytes) => {
                    let at = match at {
                        At::Address(a) => format!("{{\"address\":{a}}}"),
                        At::Base(r, o) => format!("{{\"base\":\"{}\",\"offset\":{o}}}", r.name()),
                        At::Register(r) => format!("{{\"register\":\"{}\"}}", r.name()),
                        At::Gte(g) => format!("{{\"gte\":{g}}}"),
                    };
                    format!("{{\"at\":{at},\"bytes\":{bytes},\"why\":{}}}", string(why))
                }
                Explained::By(p) => format!("{{\"by\":{},\"why\":{}}}", pc(p), string(why)),
            })
            .collect();
        let calls: Vec<String> = self
            .calls
            .iter()
            .map(|h| {
                let on = match &h.site {
                    Site::Entry(p) => format!("{{\"entry\":{}}}", pc(p)),
                    Site::Load(p) => format!("{{\"read\":{}}}", pc(p)),
                };
                format!(
                    "{{\"interface\":{},\"call\":{},\"kind\":\"{}\",\"on\":{on}}}",
                    string(h.interface),
                    string(h.name),
                    if h.response.is_some() { "query" } else { "command" }
                )
            })
            .collect();
        Ok(format!(
            "{{\"begin\":{},\"end\":{end},\"explained\":[{}],\"calls\":[{}]}}",
            pc(begin),
            explained.join(","),
            calls.join(",")
        ))
    }
}

/// The machine as psx sees it at a hook: memory, the CPU registers now and
/// as latched at the step's begin, and the GTE. psx records every byte and
/// register read through it, so the read audit counts them as the
/// binding's.
///
/// A read outside main RAM and the scratchpad stops the binding, and psx
/// reports it as the capture's refusal.
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

    /// A CPU register now.
    pub fn reg(&self, r: Reg) -> u32 {
        // SAFETY: as in `bytes`.
        unsafe { (self.host.reg)(self.host.ctx, r.0 as u32, false) }
    }

    /// A CPU register as it was at the step's begin: where the step's
    /// arguments are.
    pub fn latched(&self, r: Reg) -> u32 {
        // SAFETY: as in `bytes`.
        unsafe { (self.host.reg)(self.host.ctx, r.0 as u32, true) }
    }

    /// A GTE register now: data 0 to 31, control 32 to 63.
    pub fn gte(&self, n: u8) -> u32 {
        // SAFETY: as in `bytes`.
        unsafe { (self.host.gte)(self.host.ctx, n as u32) }
    }

    /// At a [`Site::Load`] call: where the load read. Anywhere else it
    /// stops the binding with an error.
    pub fn load(&self) -> Load {
        let (mut address, mut bytes, mut value) = (0, 0, 0);
        // SAFETY: as in `bytes`; the outputs are writable.
        if !unsafe { (self.host.load)(self.host.ctx, &mut address, &mut bytes, &mut value) } {
            panic!("Machine::load: only a Site::load call's request and response have a load");
        }
        Load { address, bytes, value }
    }
}

/// A value in the trace's encoding: an integer (a fixed-point number's
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
    /// JSON, as the trace writes it: integers outside ±2^53 as decimal
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

/// The mechanic a binding reads: its id, version and fingerprint, and its
/// structs in the trace's encoding. Its implementation is generated from
/// the mechanic's source by the binding's `build.rs`
/// (`psx_mechanics::rust::write_glue`).
pub trait Mechanic {
    const ID: &'static str;
    const VERSION: u32;
    /// `Mechanic::fingerprint` of the source the glue was generated from.
    const FINGERPRINT: &'static str;
    type State;
    type Inputs;
    type Params;
    fn state(s: &Self::State) -> Value;
    fn inputs(i: &Self::Inputs) -> Value;
    fn params(p: &Self::Params) -> Value;
}

/// A mechanic's binding on the PlayStation. The type is a unit struct
/// (`struct Turn;`), exported with [`export!`].
pub trait Binding {
    type Mechanic: Mechanic;
    /// The release, in words: `Croc - Legend of the Gobbos (Europe) SLES-00593`.
    const TITLE: &'static str;
    /// The image's identity, as `psx context` reports the media.
    const IMAGE_SHA256: &'static str;

    /// Where the step begins and ends, and the reads explained.
    fn bracket(&self, b: &mut Bracket);

    /// At the begin PC: whether this call of the routine is the mechanic's
    /// step (the player's object, not an enemy's that runs the same code).
    /// On `false` nothing is recorded and the routine runs on.
    fn is_step(&self, m: &Machine) -> bool {
        let _ = m;
        true
    }

    /// The state: at the begin, with the registers as latched there, and at
    /// the end.
    fn state(&self, m: &Machine) -> <Self::Mechanic as Mechanic>::State;

    /// The inputs, at the begin.
    fn inputs(&self, m: &Machine) -> <Self::Mechanic as Mechanic>::Inputs;

    /// The params, once, at the first step's begin.
    fn params(&self, m: &Machine) -> <Self::Mechanic as Mechanic>::Params;
}

/// Exports a binding, a unit struct implementing [`Binding`], as the C
/// functions psx calls.
#[macro_export]
macro_rules! export {
    ($binding:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn psx_capture_abi_version() -> u32 {
            $crate::ABI_VERSION
        }

        /// # Safety
        /// `host` is psx's, valid for the call.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn psx_capture_call(what: u32, which: u32, host: *const $crate::abi::Host) -> i32 {
            // SAFETY: the caller's.
            unsafe { $crate::abi::call(&$binding, what, which, host) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_brackets_are_json() {
        struct Slope;
        impl Call for Slope {
            const INTERFACE: &'static str = "world.floor-slope@1";
            const NAME: &'static str = "slope_x";
            type Request = ();
            type Response = i16;
            fn request(_: &()) -> Value {
                Value::Record(Vec::new())
            }
            fn response(r: &i16) -> Value {
                Value::Record(vec![("value", Value::Int(*r as i128))])
            }
        }
        impl Query for Slope {}

        let mut s = String::new();
        Value::Record(vec![
            ("a", Value::Int(-3)),
            ("b", Value::Int(1 << 60)),
            ("c", Value::List(vec![Value::Bool(true), Value::Variant("on_ground")])),
        ])
        .json(&mut s);
        assert_eq!(s, r#"{"a":-3,"b":"1152921504606846976","c":[true,"on_ground"]}"#);

        let mut b = Bracket::default();
        assert!(b.json().is_err());
        b.begin(0x8004_425C).end(Pc::in_overlay("level", 16));
        b.explain(At::Base(reg::A0, -4), 4, "the \"pc\"").explain_reader(0x10u32, "level data");
        b.query::<Slope>(Site::load(0x8004_3A84), |_| (), |m| m.i16(m.load().address));
        assert_eq!(
            b.json().unwrap(),
            r#"{"begin":{"pc":2147762780},"end":{"pc":16,"overlay":"level"},"explained":[{"at":{"base":"a0","offset":-4},"bytes":4,"why":"the \"pc\""},{"by":{"pc":16},"why":"level data"}],"calls":[{"interface":"world.floor-slope@1","call":"slope_x","kind":"query","on":{"read":{"pc":2147760772}}}]}"#
        );
    }
}

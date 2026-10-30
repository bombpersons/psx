//! The boundary between psx and a binding: C functions with integers and
//! pointers to bytes only, so a binding built by another Rust compiler than
//! psx's loads all the same.
//!
//! The binding exports `psx_capture_abi_version() -> u32` and
//! `psx_capture_call(what, which, host) -> i32` ([`export!`](crate::export)). psx
//! passes a [`Host`]: its own functions to read the machine and to take the
//! answer, which is JSON written through [`Host::out`].

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{Binding, Bracket, Machine, Mechanic, string};

/// psx's side, valid for one call.
#[repr(C)]
pub struct Host {
    pub ctx: *mut c_void,
    /// `len` bytes at `addr` into `out`; false when psx refuses the read
    /// (it keeps the reason).
    pub read: unsafe extern "C" fn(ctx: *mut c_void, addr: u32, out: *mut u8, len: u32) -> bool,
    /// CPU register `which` (0-33), now or as latched at the step's begin.
    pub reg: unsafe extern "C" fn(ctx: *mut c_void, which: u32, latched: bool) -> u32,
    /// GTE register `which` (0-63).
    pub gte: unsafe extern "C" fn(ctx: *mut c_void, which: u32) -> u32,
    /// At an inline call: the load's address, width and value; false elsewhere.
    pub load: unsafe extern "C" fn(ctx: *mut c_void, address: *mut u32, bytes: *mut u32, value: *mut u32) -> bool,
    /// The answer, or a panic's message.
    pub out: unsafe extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize),
}

/// `what`: the mechanic and the bracket (JSON: `mechanic`,
/// `mechanic_version`, `fingerprint`, `source` and `capture`, the bracket:
/// `begin`, `end`, `explained` and `calls`).
pub const DESCRIBE: u32 = 0;
/// `what`: whether this call is the step (JSON `true` or `false`).
pub const IS_STEP: u32 = 1;
/// `what`: the state, inputs or params (a JSON object, field to value).
pub const STATE: u32 = 2;
pub const INPUTS: u32 = 3;
pub const PARAMS: u32 = 4;
/// `what`: call `which`'s request or response (`which` counts the calls
/// in the order `bracket` declares them).
pub const REQUEST: u32 = 5;
pub const RESPONSE: u32 = 6;

/// Returned: the answer was written.
pub const OK: i32 = 0;
/// Returned: the binding panicked; its message was written.
pub const PANICKED: i32 = 1;
/// Returned: psx refused a read, and has the reason.
pub const READ_REFUSED: i32 = 2;
/// Returned: `what` (or `which`) is none of the above.
pub const UNKNOWN: i32 = 3;

/// The panic `Machine` raises when psx refuses a read.
pub struct ReadRefused;

thread_local! {
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Answers psx's question `what` for `binding`.
///
/// # Safety
/// `host` is psx's, valid for the call.
pub unsafe fn call<B: Binding>(binding: &B, what: u32, which: u32, host: *const Host) -> i32 {
    // A panic is the capture's error, reported by psx: keep its message and
    // where it was, and print nothing.
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            if !info.payload().is::<ReadRefused>() {
                PANIC.with(|p| *p.borrow_mut() = Some(info.to_string()));
            }
        }))
    });
    // SAFETY: the caller's.
    let host = unsafe { &*host };
    let m = Machine { host };
    let answer = catch_unwind(AssertUnwindSafe(|| {
        let mut out = String::new();
        match what {
            DESCRIBE => out = describe(binding),
            IS_STEP => out.push_str(if binding.is_step(&m) { "true" } else { "false" }),
            STATE => B::Mechanic::state(&binding.state(&m)).json(&mut out),
            INPUTS => B::Mechanic::inputs(&binding.inputs(&m)).json(&mut out),
            PARAMS => B::Mechanic::params(&binding.params(&m)).json(&mut out),
            REQUEST | RESPONSE => {
                let mut b = Bracket::default();
                binding.bracket(&mut b);
                if !b.read_call(which, what == RESPONSE, &m, &mut out) {
                    return None;
                }
            }
            _ => return None,
        }
        Some(out)
    }));
    let (code, text) = match answer {
        Ok(Some(out)) => (OK, out),
        Ok(None) => (UNKNOWN, String::new()),
        Err(p) if p.is::<ReadRefused>() => (READ_REFUSED, String::new()),
        Err(_) => (PANICKED, PANIC.with(|p| p.borrow_mut().take()).unwrap_or_else(|| "the binding panicked".into())),
    };
    // SAFETY: the caller's; `text` is valid for its length.
    unsafe { (host.out)(host.ctx, text.as_ptr(), text.len()) };
    code
}

fn describe<B: Binding>(binding: &B) -> String {
    let mut b = Bracket::default();
    binding.bracket(&mut b);
    let capture = match b.json() {
        Ok(c) => c,
        Err(e) => panic!("{e}"),
    };
    format!(
        "{{\"mechanic\":{},\"mechanic_version\":{},\"fingerprint\":{},\"source\":{{\"platform\":\"psx\",\"title\":{},\"image_sha256\":{}}},\"capture\":{capture}}}",
        string(B::Mechanic::ID),
        B::Mechanic::VERSION,
        string(B::Mechanic::FINGERPRINT),
        string(B::TITLE),
        string(B::IMAGE_SHA256),
    )
}

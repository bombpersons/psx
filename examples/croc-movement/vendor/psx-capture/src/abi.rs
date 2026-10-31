//! The boundary between psx and the glossary's binding: C functions with
//! integers and pointers to bytes only, so a binding built by another Rust
//! compiler than psx's loads all the same.
//!
//! The binding exports `psx_capture_abi_version() -> u32` and
//! `psx_capture_call(what, host) -> i32`
//! ([`export_glossary!`](crate::export_glossary)). psx passes a [`Host`]: its
//! own functions to read the machine and to take the answer, which is JSON
//! written through [`Host::out`].

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{Glossary, Machine, Quantities, Tick, string};

/// psx's side, valid for one call.
#[repr(C)]
pub struct Host {
    pub ctx: *mut c_void,
    /// `len` bytes at `addr` into `out`; false when psx refuses the read
    /// (it keeps the reason).
    pub read: unsafe extern "C" fn(ctx: *mut c_void, addr: u32, out: *mut u8, len: u32) -> bool,
    /// CPU register `which` (0-33: r0-r31, hi, lo).
    pub reg: unsafe extern "C" fn(ctx: *mut c_void, which: u32) -> u32,
    /// GTE register `which` (0-63).
    pub gte: unsafe extern "C" fn(ctx: *mut c_void, which: u32) -> u32,
    /// The answer, or a panic's message.
    pub out: unsafe extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize),
}

/// `what`: the binding (JSON: `glossary`, the fingerprint; `source`; and
/// `tick`).
pub const DESCRIBE: u32 = 0;
/// `what`: the quantities (a JSON object, quantity to value).
pub const QUANTITIES: u32 = 1;

/// Returned: the answer was written.
pub const OK: i32 = 0;
/// Returned: the binding panicked; its message was written.
pub const PANICKED: i32 = 1;
/// Returned: psx refused a read, and has the reason.
pub const READ_REFUSED: i32 = 2;
/// Returned: `what` is none of the above.
pub const UNKNOWN: i32 = 3;

/// The panic `Machine` raises when psx refuses a read.
pub struct ReadRefused;

thread_local! {
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Answers psx's question `what` for the glossary's binding.
///
/// # Safety
/// `host` is psx's, valid for the call.
pub unsafe fn call<G: Glossary>(glossary: &G, what: u32, host: *const Host) -> i32 {
    // SAFETY: the caller's.
    unsafe {
        answer(host, |m, out| match what {
            DESCRIBE => {
                let mut t = Tick::default();
                glossary.tick(&mut t);
                let tick = match t.json() {
                    Ok(t) => t,
                    Err(e) => panic!("{e}"),
                };
                *out = format!(
                    "{{\"glossary\":{},\"source\":{{\"platform\":\"psx\",\"title\":{},\"image_sha256\":{}}},\"tick\":{tick}}}",
                    string(G::Glue::FINGERPRINT),
                    string(G::TITLE),
                    string(G::IMAGE_SHA256),
                );
                true
            }
            QUANTITIES => {
                G::Glue::quantities(&glossary.quantities(m)).json(out);
                true
            }
            _ => false,
        })
    }
}

/// Runs `f` with the machine behind `host`, catching a panic, and writes
/// its answer (or the panic's message) through `host`. `f` returns false
/// for a question it does not know.
///
/// # Safety
/// `host` is psx's, valid for the call.
unsafe fn answer(host: *const Host, f: impl FnOnce(&Machine, &mut String) -> bool) -> i32 {
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
        f(&m, &mut out).then_some(out)
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

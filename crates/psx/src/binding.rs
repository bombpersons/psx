//! The glossary's binding (docs/scenarios.md): a `cdylib` crate in
//! `glossary/binding/`, built against the vendored `psx-capture`. `psx
//! scenario record` builds it with cargo, loads it, asks it what ends a tick
//! of the game's logic, and at the end of each tick asks it for the
//! glossary's quantities. Its reads of the machine come back through psx.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use psx_capture::abi;
use psx_core::Machine;
use psx_scenario::{Type, Values};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Error;

/// `source.platform`.
pub const PLATFORM: &str = "psx";

const RAM: u32 = 0x20_0000;
const SCRATCHPAD: std::ops::Range<u32> = 0x1F80_0000..0x1F80_0400;

/// Main RAM in any segment, or the scratchpad.
fn in_memory(a: u32) -> bool {
    [0, 0x8000_0000, 0xA000_0000].iter().any(|s| (*s..*s + RAM).contains(&a)) || SCRATCHPAD.contains(&a)
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

impl Tick {
    /// Whether the instruction `info` ran ended a tick.
    pub fn ended_by(&self, info: &psx_core::cpu::StepInfo) -> bool {
        let Some(acc) = info.mem.filter(|a| a.store) else { return false };
        let (lo, a) = (crate::run::fold(self.at), crate::run::fold(acc.addr));
        a < lo + self.bytes && a + acc.width as u32 > lo && self.pc.is_none_or(|pc| pc == info.pc)
    }

    pub fn check(&self, problems: &mut Vec<String>) {
        if !in_memory(self.at) {
            problems.push(format!("tick.at {:#010x}: main RAM or the scratchpad", self.at));
        }
        if ![1, 2, 4].contains(&self.bytes) || !self.at.is_multiple_of(self.bytes) {
            problems.push("tick.bytes: 1, 2 or 4, with at aligned to it".into());
        }
        if let Some(pc) = self.pc {
            if !pc.is_multiple_of(4) || !in_memory(pc) || SCRATCHPAD.contains(&pc) {
                problems.push(format!("tick.pc {pc:#010x}: a word-aligned address in main RAM"));
            }
        }
    }
}

/// Whether `dir` holds a Rust crate: `Cargo.toml` and `src/lib.rs`.
pub fn is_crate(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join(psx_scenario::rust::LIB).is_file()
}

/// The `.rs` files under `dir/src`, by path relative to `dir`, sorted.
pub fn sources(dir: &Path) -> crate::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut todo = vec![dir.join("src")];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| Error::io(&d, e))?.flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&p).map_err(|e| Error::io(&p, e))?));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Builds the binding crate `package` at `dir` (a member of the workspace at
/// `root`) with cargo, and returns the library it built.
pub fn build(root: &Path, dir: &Path, package: &str) -> crate::Result<PathBuf> {
    let manifest = dir.join("Cargo.toml");
    let out = Command::new("cargo")
        .args(["build", "--lib", "--message-format=json-render-diagnostics", "-p", package])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::invalid(format!("cargo: {e}: building a Rust binding needs the Rust toolchain")))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(60)..].join("\n");
        return Err(
            Error::invalid(format!("{}: the binding does not build", rel(root, dir))).with("cargo", json!(tail))
        );
    }
    let want = manifest.canonicalize().map_err(|e| Error::io(&manifest, e))?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(m) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let ours =
            m["manifest_path"].as_str().and_then(|p| Path::new(p).canonicalize().ok()).is_some_and(|p| p == want);
        let cdylib = m["target"]["kind"].as_array().is_some_and(|k| k.iter().any(|k| k == "cdylib"));
        if m["reason"] == "compiler-artifact" && ours && cdylib {
            let lib = m["filenames"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).find(|f| {
                let ext = Path::new(f).extension().and_then(|e| e.to_str()).unwrap_or_default();
                ext == std::env::consts::DLL_EXTENSION
            });
            if let Some(lib) = lib {
                return Ok(PathBuf::from(lib));
            }
        }
    }
    Err(Error::invalid(format!(
        "{}: cargo built no library: the crate is a cdylib ([lib] crate-type = [\"cdylib\"])",
        rel(root, dir)
    )))
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned()
}

/// `psx_capture_call`.
pub type Call = unsafe extern "C" fn(u32, *const abi::Host) -> i32;

/// A loaded binding, with its answer to `DESCRIBE` as `D`.
pub struct Plugin<D> {
    call: Call,
    /// Keeps `call` loaded (none for a binding linked into psx, in tests).
    _lib: Option<libloading::Library>,
    described: D,
}

/// `n` (at least 1) bytes at `a`, in main RAM or the scratchpad.
fn memory(m: &Machine, a: u32, n: u32) -> Result<Vec<u8>, String> {
    let last = a.wrapping_add(n - 1);
    if !in_memory(a) || !in_memory(last) || (a >= 0x1F80_0000) != (last >= 0x1F80_0000) {
        return Err(format!("{n} bytes at {a:#010x}: not in main RAM or the scratchpad"));
    }
    let bus = &m.bus;
    Ok(if SCRATCHPAD.contains(&a) {
        bus.scratch[(a - 0x1F80_0000) as usize..][..n as usize].to_vec()
    } else {
        let o = (a & 0x1F_FFFF) as usize;
        bus.ram.get(o..o + n as usize).ok_or(format!("{n} bytes at {a:#010x}: past the end of RAM"))?.to_vec()
    })
}

/// psx's side of one call: the machine (none while the binding describes
/// itself), and what the binding answered.
struct Ctx<'a> {
    m: Option<&'a Machine>,
    refused: Option<String>,
    out: Vec<u8>,
}

/// # Safety
/// `ctx` is a `Ctx`; `out` is writable for `len` bytes.
unsafe extern "C" fn host_read(ctx: *mut c_void, addr: u32, out: *mut u8, len: u32) -> bool {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    if len == 0 {
        return true;
    }
    let Some(m) = c.m else {
        c.refused.get_or_insert("the binding read the machine while describing itself".into());
        return false;
    };
    match memory(m, addr, len) {
        Ok(bytes) => {
            // SAFETY: the caller's.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
            true
        }
        Err(e) => {
            c.refused.get_or_insert(e);
            false
        }
    }
}

/// # Safety
/// `ctx` is a `Ctx`.
unsafe extern "C" fn host_reg(ctx: *mut c_void, which: u32) -> u32 {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    let Some(m) = c.m else { return 0 };
    match which {
        0..32 => m.cpu.regs[which as usize],
        32 => m.cpu.hi,
        33 => m.cpu.lo,
        _ => 0,
    }
}

/// # Safety
/// `ctx` is a `Ctx`.
unsafe extern "C" fn host_gte(ctx: *mut c_void, which: u32) -> u32 {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    let Some(m) = c.m else { return 0 };
    let g = &m.cpu.gte;
    match which {
        0..32 => g.read_data(which as usize),
        32..64 => g.read_ctrl(which as usize - 32),
        _ => 0,
    }
}

/// # Safety
/// `ctx` is a `Ctx`; `data` is readable for `len` bytes.
unsafe extern "C" fn host_out(ctx: *mut c_void, data: *const u8, len: usize) {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    // SAFETY: the caller's.
    c.out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
}

impl<D: serde::de::DeserializeOwned> Plugin<D> {
    /// Loads the library cargo built at `lib` and asks it to describe
    /// itself. The library is loaded from a copy named by its contents, so
    /// a rebuilt binding is never taken for one loaded before, and by this
    /// process, so recordings made at once never share one.
    pub fn load(lib: &Path) -> crate::Result<Plugin<D>> {
        let bytes = std::fs::read(lib).map_err(|e| Error::io(lib, e))?;
        let sha = crate::stamp::sha256(&bytes);
        let name = format!("psx-load-{}-{}.{}", &sha[..16], std::process::id(), std::env::consts::DLL_EXTENSION);
        let copy = lib.with_file_name(name);
        std::fs::write(&copy, &bytes).map_err(|e| Error::io(&copy, e))?;
        // SAFETY: the library is the workspace's binding, which psx just
        // built; its initialisers are the Rust standard library's.
        let loaded = unsafe { libloading::Library::new(&copy) };
        let _ = std::fs::remove_file(&copy);
        let lib_err = |e: libloading::Error| Error::invalid(format!("{}: {e}", lib.display()));
        let library = loaded.map_err(lib_err)?;
        // SAFETY: the symbols' types are psx-capture's `export_glossary!`'s.
        let (version, call) = unsafe {
            let version = library.get::<unsafe extern "C" fn() -> u32>(b"psx_capture_abi_version\0").map_err(|_| {
                Error::invalid(format!(
                    "{}: exports no binding: end it with psx_capture::export_glossary!(..)",
                    lib.display()
                ))
            })?;
            let call = library.get::<Call>(b"psx_capture_call\0").map_err(lib_err)?;
            (version(), *call)
        };
        if version != psx_capture::ABI_VERSION {
            return Err(Error::invalid(format!(
                "the binding was built against psx-capture's boundary version {version}, this psx has {}: psx port update brings vendor/psx-capture to this psx's",
                psx_capture::ABI_VERSION
            )));
        }
        Plugin::new(call, Some(library))
    }

    /// The binding whose `psx_capture_call` is `call`, asked to describe
    /// itself.
    pub fn new(call: Call, lib: Option<libloading::Library>) -> crate::Result<Plugin<D>> {
        let text = invoke(call, abi::DESCRIBE, None).map_err(Error::invalid)?;
        let described = serde_json::from_str(&text)
            .map_err(|e| Error::invalid(format!("the binding's description: {e}")).with("description", json!(text)))?;
        Ok(Plugin { call, _lib: lib, described })
    }

    /// Asks the binding `what` with the machine as it is.
    pub fn call(&self, what: u32, m: &Machine) -> Result<String, String> {
        invoke(self.call, what, Some(m))
    }

    /// What the binding says of itself.
    pub fn described(&self) -> &D {
        &self.described
    }
}

/// Calls the binding's `psx_capture_call` with psx's side of the boundary.
fn invoke(call: Call, what: u32, m: Option<&Machine>) -> Result<String, String> {
    let mut ctx = Ctx { m, refused: None, out: Vec::new() };
    let host = abi::Host {
        ctx: &mut ctx as *mut Ctx as *mut c_void,
        read: host_read,
        reg: host_reg,
        gte: host_gte,
        out: host_out,
    };
    // SAFETY: `host` and `ctx` outlive the call.
    let code = unsafe { call(what, &host) };
    let out = String::from_utf8_lossy(&ctx.out).into_owned();
    match code {
        abi::OK => Ok(out),
        abi::PANICKED => Err(format!("the binding panicked: {out}")),
        abi::READ_REFUSED => Err(ctx.refused.take().unwrap_or_else(|| "a read was refused".into())),
        c => Err(format!("the binding gave code {c} for question {what}")),
    }
}

/// The binding's answer for a set of quantities, as values of their
/// `types`; each problem is `(quantity, problem)`.
pub fn values(text: &str, types: &std::collections::BTreeMap<String, Type>) -> Result<Values, Vec<(String, String)>> {
    let v: Values =
        serde_json::from_str(text).map_err(|e| vec![(String::new(), format!("the binding's answer: {e}"))])?;
    let mut problems = Vec::new();
    for (name, ty) in types {
        match v.get(name) {
            None => problems.push((name.clone(), "the binding gave no value".to_string())),
            Some(x) => {
                if let Err(e) = ty.check(x) {
                    problems.push((name.clone(), e));
                }
            }
        }
    }
    for name in v.keys().filter(|n| !types.contains_key(*n)) {
        problems.push((name.clone(), "the glossary has no such quantity".to_string()));
    }
    if problems.is_empty() { Ok(v) } else { Err(problems) }
}

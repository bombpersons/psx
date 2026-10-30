//! Bindings written as Rust (docs/mechanics.md, "The binding"): a `cdylib`
//! crate in `mechanics/<id>/binding/`, built against the vendored
//! `psx-capture`. Capture builds it with cargo, loads it and asks it, at the
//! binding's hooks, whether this call is the step and what the state,
//! inputs and params are. Its reads of the machine come back through psx,
//! which records them for the read audit.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use psx_capture::abi;
use psx_core::Machine;
use psx_mechanics::binding::Source;
use psx_mechanics::{Mechanic, Platform, Type, Values};
use serde_json::json;

use super::capture::{Regs, memory};
use super::{Binding, Capture, Psx};
use crate::Error;
use crate::run::fold;

/// The package a mechanic's Rust binding is: `croc-turn-binding`.
pub fn package(id: &str) -> String {
    format!("{}-binding", id.replace('.', "-"))
}

/// Builds the binding crate at `dir` (a member of the workspace at `root`)
/// with cargo, and returns the library it built.
pub fn build(root: &Path, dir: &Path, id: &str) -> crate::Result<PathBuf> {
    let manifest = dir.join("Cargo.toml");
    let out = Command::new("cargo")
        .args(["build", "--lib", "--message-format=json-render-diagnostics", "-p", &package(id)])
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

type Call = unsafe extern "C" fn(u32, *const abi::Host) -> i32;

/// A loaded binding.
pub struct Plugin {
    call: Call,
    /// Keeps `call` loaded.
    _lib: libloading::Library,
    described: Described,
}

/// The binding's answer to `DESCRIBE`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Described {
    mechanic: String,
    mechanic_version: u32,
    fingerprint: String,
    source: Source,
    capture: Capture,
}

/// What the binding read during one call: memory as physical `[start,
/// end)` ranges, and CPU registers now, CPU registers as latched, and GTE
/// registers as bit sets.
#[derive(Clone, Debug, Default)]
pub struct Reads {
    pub memory: Vec<(u32, u32)>,
    pub registers: u64,
    pub latched: u64,
    pub gte: u64,
}

impl Reads {
    pub fn extend(&mut self, o: &Reads) {
        self.memory.extend_from_slice(&o.memory);
        self.registers |= o.registers;
        self.latched |= o.latched;
        self.gte |= o.gte;
    }
}

/// psx's side of one call: the machine at the hook, and what the binding
/// read and answered.
struct Ctx<'a> {
    /// None while the binding describes itself.
    m: Option<&'a Machine>,
    now: &'a Regs,
    latched: &'a Regs,
    reads: Reads,
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
            let p = fold(addr);
            c.reads.memory.push((p, p + len));
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
unsafe extern "C" fn host_reg(ctx: *mut c_void, which: u32, latched: bool) -> u32 {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    let Some(&v) = (if latched { c.latched } else { c.now }).get(which as usize) else { return 0 };
    if latched {
        c.reads.latched |= 1 << which;
    } else {
        c.reads.registers |= 1 << which;
    }
    v
}

/// # Safety
/// `ctx` is a `Ctx`.
unsafe extern "C" fn host_gte(ctx: *mut c_void, which: u32) -> u32 {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    let Some(m) = c.m else { return 0 };
    let g = &m.cpu.gte;
    let v = match which {
        0..32 => g.read_data(which as usize),
        32..64 => g.read_ctrl(which as usize - 32),
        _ => return 0,
    };
    c.reads.gte |= 1 << which;
    v
}

/// # Safety
/// `ctx` is a `Ctx`; `data` is readable for `len` bytes.
unsafe extern "C" fn host_out(ctx: *mut c_void, data: *const u8, len: usize) {
    // SAFETY: the caller's.
    let c = unsafe { &mut *(ctx as *mut Ctx) };
    // SAFETY: the caller's.
    c.out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
}

impl Plugin {
    /// Loads the library cargo built at `lib` and asks it to describe
    /// itself. The library is loaded from a copy named by its contents, so
    /// a rebuilt binding is never taken for one loaded before.
    pub fn load(lib: &Path) -> crate::Result<Plugin> {
        let bytes = std::fs::read(lib).map_err(|e| Error::io(lib, e))?;
        let sha = crate::stamp::sha256(&bytes);
        let copy = lib.with_file_name(format!("psx-load-{}.{}", &sha[..16], std::env::consts::DLL_EXTENSION));
        std::fs::write(&copy, &bytes).map_err(|e| Error::io(&copy, e))?;
        // SAFETY: the library is the workspace's binding, which psx just
        // built; its initialisers are the Rust standard library's.
        let loaded = unsafe { libloading::Library::new(&copy) };
        let _ = std::fs::remove_file(&copy);
        let lib_err = |e: libloading::Error| Error::invalid(format!("{}: {e}", lib.display()));
        let library = loaded.map_err(lib_err)?;
        // SAFETY: the symbols' types are psx-capture's `export!`'s.
        let (version, call) = unsafe {
            let version = library.get::<unsafe extern "C" fn() -> u32>(b"psx_capture_abi_version\0").map_err(|_| {
                Error::invalid(format!("{}: exports no binding: end it with psx_capture::export!(..)", lib.display()))
            })?;
            let call = library.get::<Call>(b"psx_capture_call\0").map_err(lib_err)?;
            (version(), *call)
        };
        if version != psx_capture::ABI_VERSION {
            return Err(Error::invalid(format!(
                "the binding was built against psx-capture's boundary version {version}, this psx has {}: psx template update brings vendor/psx-capture to this psx's",
                psx_capture::ABI_VERSION
            )));
        }
        let text = invoke(call, abi::DESCRIBE, None, &[0; 34], &[0; 34]).0.map_err(Error::invalid)?;
        let described = serde_json::from_str(&text)
            .map_err(|e| Error::invalid(format!("the binding's description: {e}")).with("description", json!(text)))?;
        Ok(Plugin { call, _lib: library, described })
    }

    /// Asks the binding `what` with the machine at a hook.
    pub fn call(&self, what: u32, m: &Machine, now: &Regs, latched: &Regs) -> (Result<String, String>, Reads) {
        invoke(self.call, what, Some(m), now, latched)
    }

    /// The binding as capture plans with it: the mechanic, source and
    /// bracket the binding describes, with no fields of its own (it reads
    /// them itself). What is wrong with it is refused.
    pub fn binding(&self, mechanic: &Mechanic) -> crate::Result<Binding> {
        let d = &self.described;
        let mut p = Vec::new();
        if d.mechanic != mechanic.id || d.mechanic_version != mechanic.version {
            p.push(format!(
                "binds {}@{}, the mechanic is {}@{}",
                d.mechanic, d.mechanic_version, mechanic.id, mechanic.version
            ));
        } else if d.fingerprint != mechanic.fingerprint() {
            p.push("its glue reads the mechanic's fields differently from this psx: psx template update brings vendor/psx-mechanics to this psx's".into());
        }
        d.source.check(Psx::NAME, &mut p);
        Psx::check_capture(&d.capture, &mut p);
        if !p.is_empty() {
            return Err(Error::invalid("the Rust binding has problems").with("problems", json!(p)));
        }
        Ok(Binding {
            mechanic: d.mechanic.clone(),
            mechanic_version: d.mechanic_version,
            source: d.source.clone(),
            state: Default::default(),
            inputs: Default::default(),
            params: Default::default(),
            params_provenance: Default::default(),
            interfaces: Default::default(),
            capture: d.capture.clone(),
        })
    }
}

/// Calls the binding's `psx_capture_call` with psx's side of the boundary.
fn invoke(call: Call, what: u32, m: Option<&Machine>, now: &Regs, latched: &Regs) -> (Result<String, String>, Reads) {
    let mut ctx = Ctx { m, now, latched, reads: Reads::default(), refused: None, out: Vec::new() };
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
    let answer = match code {
        abi::OK => Ok(out),
        abi::PANICKED => Err(format!("the binding panicked: {out}")),
        abi::READ_REFUSED => Err(ctx.refused.take().unwrap_or_else(|| "a read was refused".into())),
        c => Err(format!("the binding gave code {c} for question {what}")),
    };
    (answer, ctx.reads)
}

/// The binding's answer for a set of fields, as values of their `types`;
/// each problem is `(field, problem)`.
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
        problems.push((name.clone(), "the mechanic has no such field".to_string()));
    }
    if problems.is_empty() { Ok(v) } else { Err(problems) }
}

/// Whether the binding's answer to `IS_STEP` says yes.
pub fn yes(text: &str) -> Result<bool, String> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        t => Err(format!("is_step answered {t:?}")),
    }
}

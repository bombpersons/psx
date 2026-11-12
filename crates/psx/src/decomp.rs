//! The agent's C for the original's functions (docs/decomp-plan.md, step
//! 9): a header from the project's names, and a build of `decomp/` that
//! psx-core can run where the game cannot see it ([`psx_core::bus::HIDDEN`],
//! [`psx_core::callcheck`]).
//!
//! The header has every struct and union of the names files laid out at
//! their offsets (padding between fields, packed only where natural
//! alignment cannot reach an offset) with a static assert on every field's
//! offset and every size, their enums and typedefs, every global as
//! `extern`, and every function's prototype. The linker script places the
//! globals and functions at their addresses with `PROVIDE`, so the C calls
//! any original function by name, and a function the C defines itself
//! (the agent's version) takes its name over. A name two modules give
//! different addresses is left out of both, with a note.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use psx_symbols::names::{BASE_TYPES, Names, Type, TypeBody, TypeDef};
use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// What the agent's C is built to run at.
pub const BASE: u32 = psx_core::bus::HIDDEN;

/// C's own types among the names files' base types.
const C_KEYWORDS: &[&str] = &["void", "char", "short", "int", "long", "float", "double"];

/// The C each non-keyword base type stands for.
fn base_c(name: &str) -> &'static str {
    match name {
        "i8" | "s8" => "signed char",
        "u8" | "byte" | "uchar" | "u_char" | "bool" | "undefined" | "undefined1" => "unsigned char",
        "i16" | "s16" => "short",
        "u16" | "ushort" | "u_short" | "undefined2" => "unsigned short",
        "i32" | "s32" => "int",
        "u32" | "uint" | "u_int" | "undefined4" => "unsigned int",
        "ulong" | "u_long" => "unsigned long",
        "i64" | "s64" | "longlong" => "long long",
        "u64" | "undefined8" => "unsigned long long",
        "code" => "void",
        _ => "unsigned char",
    }
}

/// `ty` declaring `decl` (a name, or empty for a type alone), in C.
pub(crate) fn declare(ty: &Type, decl: &str) -> String {
    match ty {
        Type::Named { name } => format!("{name} {decl}").trim_end().to_string(),
        Type::Pointer { to, offset } if *offset != 0 => {
            // A pointer into the middle of a struct: its bytes, as C has no type for it.
            let what = declare(to, "");
            format!("unsigned char *{decl} /* {what} *+0x{offset:X} */")
        }
        Type::Pointer { to, .. } => match **to {
            Type::Array { .. } | Type::Function(_) => declare(to, &format!("(*{decl})")),
            _ => declare(to, &format!("*{decl}")),
        },
        Type::Array { of, len } => declare(of, &format!("{decl}[{len}]")),
        Type::Function(sig) => {
            let mut params: Vec<String> = sig
                .params
                .iter()
                .enumerate()
                .map(|(k, p)| declare(&p.ty, &p.name.as_ref().map_or(format!("a{k}"), |n| n.text.clone())))
                .collect();
            if sig.varargs {
                params.push("...".into());
            }
            if params.is_empty() {
                params.push("void".into());
            }
            declare(&sig.ret, &format!("{decl}({})", params.join(", ")))
        }
    }
}

/// The natural alignment of `ty` for MIPS GCC (o32).
fn align(names: &Names, ty: &Type) -> u32 {
    match ty {
        Type::Pointer { .. } | Type::Function(_) => 4,
        Type::Array { of, .. } => align(names, of),
        Type::Named { name } => {
            if let Some(s) = psx_symbols::names::base_size(name) {
                return s.clamp(1, 8);
            }
            match names.type_named(name).map(|t| &t.body) {
                Some(TypeBody::Struct { fields } | TypeBody::Union { fields }) => {
                    fields.iter().map(|f| align(names, &f.ty)).max().unwrap_or(1)
                }
                Some(TypeBody::Enum { .. }) => 4,
                Some(TypeBody::Typedef { ty }) => align(names, ty),
                None => 1,
            }
        }
    }
}

/// The struct and union names `ty` holds by value (not through a pointer).
fn by_value<'a>(ty: &'a Type, out: &mut Vec<&'a str>) {
    match ty {
        Type::Named { name } => out.push(name),
        Type::Array { of, .. } => by_value(of, out),
        _ => {}
    }
}

/// The header and the linker script for `names`, and notes on what was left out.
pub fn header(names: &Names) -> (String, String, Vec<String>) {
    let mut h = String::from(
        "/* From the project's names files by psx; do not edit (docs/decomp-plan.md). */\n#ifndef PSX_NAMES_H\n#define PSX_NAMES_H\n\n",
    );
    let mut notes = Vec::new();
    for (name, _) in BASE_TYPES {
        if !C_KEYWORDS.contains(name) {
            h += &format!("typedef {} {name};\n", base_c(name));
        }
    }
    h += "\n";
    let aggregates: Vec<&TypeDef> =
        names.types.iter().filter(|t| matches!(t.body, TypeBody::Struct { .. } | TypeBody::Union { .. })).collect();
    for t in &aggregates {
        let kw = if matches!(t.body, TypeBody::Union { .. }) { "union" } else { "struct" };
        h += &format!("typedef {kw} {0} {0};\n", t.name.text);
    }
    for t in &names.types {
        match &t.body {
            TypeBody::Enum { values } => {
                h += &format!("typedef enum {} {{\n", t.name.text);
                for v in values {
                    h += &format!("    {} = {},\n", v.name.text, v.value);
                }
                h += &format!("}} {};\n", t.name.text);
            }
            TypeBody::Typedef { ty } => h += &format!("typedef {};\n", declare(ty, &t.name.text)),
            _ => {}
        }
    }
    h += "\n";
    // Definitions, each after the aggregates it holds by value.
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut order: Vec<&TypeDef> = Vec::new();
    fn visit<'a>(t: &'a TypeDef, names: &'a Names, done: &mut BTreeSet<&'a str>, order: &mut Vec<&'a TypeDef>) {
        if !done.insert(&t.name.text) {
            return;
        }
        if let TypeBody::Struct { fields } | TypeBody::Union { fields } = &t.body {
            for f in fields {
                let mut held = Vec::new();
                by_value(&f.ty, &mut held);
                for n in held {
                    if let Some(u) = names.type_named(n).filter(|u| !matches!(u.body, TypeBody::Enum { .. })) {
                        visit(u, names, done, order);
                    }
                }
            }
            order.push(t);
        }
    }
    for t in &aggregates {
        visit(t, names, &mut done, &mut order);
    }
    for t in order {
        let (union, fields) = match &t.body {
            TypeBody::Struct { fields } => (false, fields),
            TypeBody::Union { fields } => (true, fields),
            _ => continue,
        };
        let size = t.size.or_else(|| names.size_of(&Type::Named { name: t.name.text.clone() }));
        let mut sorted: Vec<_> = fields.iter().collect();
        sorted.sort_by_key(|f| f.offset);
        let mut body = String::new();
        let mut at = 0;
        let mut packed = false;
        let mut kept = Vec::new();
        for f in sorted {
            let Some(fs) = names.size_of(&f.ty) else {
                notes.push(format!("{}.{}: its size is not known, left out", t.name.text, f.name.text));
                continue;
            };
            if !union {
                if f.offset < at {
                    notes.push(format!("{}.{}: overlaps the field before, left out", t.name.text, f.name.text));
                    continue;
                }
                if f.offset > at {
                    body += &format!("    unsigned char _pad_{at:X}[0x{:X}];\n", f.offset - at);
                }
                let a = align(names, &f.ty);
                packed |= f.offset % a != 0;
                at = f.offset + fs;
            }
            body += &format!("    {};\n", declare(&f.ty, &f.name.text));
            kept.push(f);
        }
        if let (false, Some(size)) = (union, size) {
            if size > at {
                body += &format!("    unsigned char _pad_{at:X}[0x{:X}];\n", size - at);
            }
        }
        let natural = aggregates_align(names, t);
        packed |= size.is_some_and(|s| s % natural != 0);
        let kw = if union { "union" } else { "struct" };
        let attr = if packed { " __attribute__((packed))" } else { "" };
        h += &format!("{kw}{attr} {} {{\n{body}}};\n", t.name.text);
        for f in kept {
            h += &format!(
                "_Static_assert(__builtin_offsetof({0}, {1}) == 0x{2:X}, \"{0}.{1} at 0x{2:X}\");\n",
                t.name.text, f.name.text, f.offset
            );
        }
        if let Some(s) = size {
            h += &format!("_Static_assert(sizeof({0}) == 0x{1:X}, \"{0} is 0x{1:X} bytes\");\n", t.name.text, s);
        }
        h += "\n";
    }
    // Globals and functions: one address per name.
    let mut addresses: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    for (n, a) in names
        .globals
        .iter()
        .map(|g| (&g.name.text, g.address))
        .chain(names.functions.iter().map(|f| (&f.name.text, f.address)))
    {
        addresses.entry(n).or_default().insert(a);
    }
    let unique = |n: &str| addresses.get(n).is_some_and(|a| a.len() == 1);
    let mut ld = String::from("/* From the project's names files by psx. */\n");
    let mut seen = BTreeSet::new();
    for g in &names.globals {
        if !seen.insert(&g.name.text) {
            continue;
        }
        if !unique(&g.name.text) {
            notes.push(format!("{}: several addresses (modules), left out: write MODULE's in C", g.name.text));
            continue;
        }
        let decl = match &g.ty {
            Some(ty) => declare(ty, &g.name.text),
            None => format!("unsigned char {}[]", g.name.text),
        };
        h += &format!("extern {decl};\n");
        ld += &format!("PROVIDE({} = 0x{:08X});\n", g.name.text, g.address);
    }
    h += "\n";
    for f in &names.functions {
        if !seen.insert(&f.name.text) {
            continue;
        }
        if !unique(&f.name.text) {
            notes.push(format!("{}: several addresses (modules), left out: write MODULE's in C", f.name.text));
            continue;
        }
        match &f.signature {
            Some(sig) => h += &format!("{};\n", declare(&Type::Function(sig.clone()), &f.name.text)),
            None => h += &format!("int {}(); /* no signature in names/ */\n", f.name.text),
        }
        ld += &format!("PROVIDE({} = 0x{:08X});\n", f.name.text, f.address);
    }
    h += "\n#endif\n";
    (h, ld, notes)
}

/// The natural alignment of an aggregate, as GCC lays it out.
fn aggregates_align(names: &Names, t: &TypeDef) -> u32 {
    align(names, &Type::Named { name: t.name.text.clone() }).max(1)
}

/// How many return registers `function`'s signature in `names` uses: 0
/// for void, 2 for a 64-bit value, else 1 (also without a signature).
pub fn returns(names: &Names, function: &str) -> usize {
    let Some(sig) = names.functions.iter().find(|f| f.name.text == function).and_then(|f| f.signature.as_ref()) else {
        return 1;
    };
    match names.size_of(&sig.ret) {
        Some(0) => 0,
        Some(8) => 2,
        _ => 1,
    }
}

/// The MIPS GCC psx builds with: `mipsel-none-elf-gcc` or
/// `mipsel-linux-gnu-gcc` on PATH.
pub fn compiler() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in ["mipsel-none-elf-gcc", "mipsel-linux-gnu-gcc"] {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// The build of some C: the bytes to load at [`BASE`], and the address of
/// each function it defines.
#[derive(Debug)]
pub struct Build {
    pub image: Vec<u8>,
    pub functions: BTreeMap<String, u32>,
}

/// A compiler or linker message: file, line, what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub file: String,
    pub line: u32,
    pub text: String,
}

/// Builds `sources` (C files) with `names`' header and linker script at
/// [`BASE`], in `work` (made if missing), at `-O2 -G0`. Errors come back by
/// file and line.
pub fn build(names: &Names, sources: &[PathBuf], work: &Path) -> Result<Build, Vec<Message>> {
    build_at(names, sources, work, BASE)
}

/// [`build`], linked at `base` (in the hidden region).
fn build_at(names: &Names, sources: &[PathBuf], work: &Path, base: u32) -> Result<Build, Vec<Message>> {
    let fail = |text: String| vec![Message { file: String::new(), line: 0, text }];
    let gcc = compiler().ok_or_else(|| {
        fail("no MIPS GCC: install mipsel-none-elf-gcc or mipsel-linux-gnu-gcc (psx guide setup)".into())
    })?;
    std::fs::create_dir_all(work).map_err(|e| fail(format!("{}: {e}", work.display())))?;
    let (h, ld, _) = header(names);
    let write = |name: &str, text: &str| {
        std::fs::write(work.join(name), text).map_err(|e| fail(format!("{}: {e}", work.join(name).display())))
    };
    write("names.h", &h)?;
    write(
        "build.ld",
        &format!(
            "INCLUDE names.ld\nSECTIONS {{\n  . = 0x{base:08X};\n  .text : {{ *(.text*) }}\n  .rodata : {{ *(.rodata*) }}\n  .data : {{ *(.data*) *(.sdata*) }}\n  .bss : {{ *(.bss*) *(.sbss*) *(COMMON) }}\n  /DISCARD/ : {{ *(.MIPS.*) *(.reginfo) *(.comment) *(.pdr) *(.gnu.*) }}\n}}\n",
        ),
    )?;
    write("names.ld", &ld)?;
    let elf = work.join("decomp.elf");
    let out = std::process::Command::new(&gcc)
        .args([
            "-O2",
            "-G0",
            "-march=r3000",
            "-mabi=32",
            "-mno-abicalls",
            "-fno-pic",
            "-msoft-float",
            "-ffreestanding",
            "-fno-builtin",
            "-fno-stack-protector",
            "-nostdlib",
            "-static",
            "-Wl,--build-id=none",
            "-Wl,-e,0",
        ])
        .arg("-I")
        .arg(work)
        // As separate arguments: `-Wl,` splits at commas, and the script's
        // INCLUDE finds names.ld through -L, so no path is quoted anywhere.
        .arg("-L")
        .arg(work)
        .arg("-T")
        .arg(work.join("build.ld"))
        .arg("-o")
        .arg(&elf)
        .args(sources)
        .output()
        .map_err(|e| fail(format!("{}: {e}", gcc.display())))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        let mut msgs: Vec<Message> = text.lines().filter_map(message).collect();
        if msgs.is_empty() {
            msgs = fail(text.trim().to_string());
        }
        return Err(msgs);
    }
    let tool = |t: &str| binutil(&gcc, t);
    let bin = work.join("decomp.bin");
    let ok = std::process::Command::new(tool("objcopy"))
        .args(["-O", "binary"])
        .arg(&elf)
        .arg(&bin)
        .status()
        .map_err(|e| fail(format!("objcopy: {e}")))?;
    if !ok.success() {
        return Err(fail("objcopy failed".into()));
    }
    let image = std::fs::read(&bin).map_err(|e| fail(format!("{}: {e}", bin.display())))?;
    let nm = std::process::Command::new(tool("nm")).arg(&elf).output().map_err(|e| fail(format!("nm: {e}")))?;
    let functions = String::from_utf8_lossy(&nm.stdout)
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (a, kind, name) = (w.next()?, w.next()?, w.next()?);
            let a = u32::from_str_radix(a, 16).ok()?;
            (matches!(kind, "T" | "t") && a >= base).then(|| (name.to_string(), a))
        })
        .collect();
    Ok(Build { image, functions })
}

/// One call of the function the game made, checked.
#[derive(Debug, Clone)]
pub struct Case {
    /// Which of its calls (1: the first from the state or the recording).
    pub hit: u64,
    /// The scenario and recording it came from.
    pub from: Option<(String, String)>,
    /// A varied call (k of the call's): its arguments and what they point
    /// to set at random within their types.
    pub varied: Option<u64>,
    /// A varied call the original did not return from: one the game could
    /// not make, so nothing was compared.
    pub dropped: bool,
    /// `a0`..`a3` at the call.
    pub args: [u32; 4],
    pub differences: Vec<String>,
    /// The original's conditional branches on this call, with their ways.
    pub branches: BTreeSet<(u32, bool)>,
    /// Where the state at the call was saved, when the case failed.
    pub saved: Option<PathBuf>,
}

/// How [`check`] runs.
pub struct CheckOpts<'a> {
    /// Calls to check.
    pub cases: usize,
    /// Fields to wait for a call.
    pub fields: u64,
    /// Steps one call may take.
    pub steps: u64,
    /// Where failed cases' states go (`<tag>-<hit>.state`, or `<tag>.state`
    /// for one case).
    pub save: Option<&'a Path>,
    pub tag: &'a str,
    /// The function's code (the inventory's chunks): its conditional
    /// branches are read from RAM at the first call.
    pub body: Option<&'a [(u32, u32)]>,
    /// Varied calls after each call (see [`Case::varied`]).
    pub varied: usize,
}

/// The original's entry of `function` in `names` (`[MODULE:]name`).
fn entry(names: &Names, function: &str) -> Result<u32, String> {
    let (module, name) = function.rsplit_once(':').map_or((None, function), |(m, n)| (Some(m), n));
    let found: BTreeSet<u32> = names
        .functions
        .iter()
        .filter(|f| {
            f.name.text == name && module.is_none_or(|m| f.module.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(m)))
        })
        .map(|f| f.address)
        .collect();
    match found.len() {
        1 => Ok(*found.first().unwrap()),
        0 => Err(format!("{function:?}: no function of that name in names/")),
        _ => Err(format!("{function:?}: several modules name it: say which, MODULE:{name}")),
    }
}

/// `line` with each `byte 0xADDR` named: by the global in `names` that
/// holds it, else through a pointer argument (`params`, see
/// [`crate::names_bind::through_params`]).
fn named(names: &Names, params: &[(String, Type, u32)], line: &str) -> String {
    let Some(rest) = line.strip_prefix("byte 0x") else { return line.to_string() };
    let Some((hex, tail)) = rest.split_once(':') else { return line.to_string() };
    let Ok(addr) = u32::from_str_radix(hex, 16) else { return line.to_string() };
    let name =
        crate::names_bind::global_at(names, addr).or_else(|| crate::names_bind::through_params(names, params, addr));
    match name {
        Some(g) => format!("byte 0x{hex} ({g}):{tail}"),
        None => line.to_string(),
    }
}

/// How many argument registers the executable's function at `entry` takes
/// by its signature in `names` (a 64-bit value takes an even pair, a
/// struct its words); None without a signature, or with varargs.
fn argument_slots(names: &Names, entry: u32, here: impl Fn() -> Option<String>) -> Option<usize> {
    let slots = |f: &psx_symbols::names::Function| {
        let sig = f.signature.as_ref().filter(|s| !s.varargs)?;
        let mut slots = 0usize;
        for p in &sig.params {
            let size = names.size_of(&p.ty).unwrap_or(4).max(1) as usize;
            if size == 8 {
                slots += slots % 2;
            }
            slots += size.div_ceil(4);
        }
        Some(slots)
    };
    // Each module may name its own function at the address: the one whose
    // code is there when they disagree.
    let at: Vec<_> = names.functions.iter().filter(|f| f.address & 0x1FFF_FFFF == entry & 0x1FFF_FFFF).collect();
    let counts: BTreeSet<Option<usize>> = at.iter().map(|f| slots(f)).collect();
    match counts.len() {
        0 => None,
        1 => *counts.first().unwrap(),
        _ => {
            let here = here()?;
            at.iter().find(|f| crate::names_bind::label(&f.name) == here).and_then(|f| slots(f))
        }
    }
}

/// The checked name of the code at `entry` in `ram`.
fn resident(ram: &[u8], entry: u32) -> Option<String> {
    let scope = crate::names_bind::Scope::project()?;
    scope.describe(entry, 0x8000_0000, ram, 0).filter(|(n, checked)| *checked && !n.contains('+')).map(|(n, _)| n)
}

/// `difference` with a note when it is a call whose callee has no
/// signature in names/, so all four argument registers were compared.
fn unsigned_callee(
    names: &Names,
    ram: &[u8],
    difference: &str,
    a: &psx_core::callcheck::Effects,
    b: &psx_core::callcheck::Effects,
) -> String {
    let Some(k) =
        difference.strip_prefix("call ").and_then(|r| r.split(':').next()).and_then(|n| n.parse::<usize>().ok())
    else {
        return difference.to_string();
    };
    let (Some(x), Some(y)) = (a.calls.get(k - 1), b.calls.get(k - 1)) else { return difference.to_string() };
    if x.0 != y.0 || argument_slots(names, x.0, || resident(ram, x.0)).is_some() {
        return difference.to_string();
    }
    format!("{difference} (0x{:08X} has no signature in names/: all four argument registers are compared)", x.0)
}

/// The pointer parameters among the first four of `function`'s signature,
/// with their names (`a0`.. when unnamed) and values in `regs`.
fn pointer_params(names: &Names, function: &str, regs: &[u32; 32]) -> Vec<(String, Type, u32)> {
    let Some(sig) = names.functions.iter().find(|f| f.name.text == function).and_then(|f| f.signature.as_ref()) else {
        return Vec::new();
    };
    sig.params
        .iter()
        .take(4)
        .enumerate()
        .filter(|(_, p)| matches!(p.ty, Type::Pointer { .. }))
        .map(|(k, p)| {
            let name = p.name.as_ref().map_or_else(|| format!("a{k}"), crate::names_bind::label);
            (name, p.ty.clone(), regs[4 + k])
        })
        .collect()
}

/// From `m`, runs the game until it calls `function` (the original's) and
/// checks each call, up to `o.cases` of them, against `build`'s function of
/// that name: the original and the build each run from the moment of the
/// call (at most `o.steps` steps), and their effects are compared. Calls the
/// build makes to its own functions count as calls to the originals of
/// the same names. Gives up waiting for a call after `o.fields` fields. A
/// failed case's state is saved in `o.save` when given. The function's
/// conditional branches (from `o.body`) are added to `branches`.
pub fn check(
    m: &mut psx_core::Machine,
    names: &Names,
    build: &Build,
    function: &str,
    o: &CheckOpts,
    branches: &mut BTreeSet<u32>,
) -> Result<Vec<Case>, String> {
    let original = entry(names, function)?;
    let bare = function.rsplit_once(':').map_or(function, |(_, n)| n);
    let replacement =
        *build.functions.get(bare).ok_or_else(|| format!("{bare:?}: decomp/ defines no function of that name"))?;
    // The build's own functions, as the originals they stand for.
    let aliases: BTreeMap<u32, u32> =
        build.functions.iter().filter_map(|(n, a)| entry(names, n).ok().map(|o| (*a, o))).collect();
    let returns = returns(names, bare);
    let limit = m.bus.frames + o.fields;
    let mut out = Vec::new();
    let mut real = 0u64;
    let at = |m: &psx_core::Machine| m.cpu.pc & 0x1FFF_FFFF == original & 0x1FFF_FFFF;
    // The call `m` is at, compared.
    let one = |m: &mut psx_core::Machine, hit: u64, varied: Option<u64>, branches: &BTreeSet<u32>| {
        let c = psx_core::callcheck::compare(m, &build.image, replacement, returns, o.steps)?;
        let r = &m.cpu.regs;
        if varied.is_some() && !c.original.returned {
            return Ok::<_, String>(Case {
                hit,
                from: None,
                varied,
                dropped: true,
                args: [r[4], r[5], r[6], r[7]],
                differences: Vec::new(),
                branches: BTreeSet::new(),
                saved: None,
            });
        }
        let mut theirs = c.replacement.clone();
        for call in &mut theirs.calls {
            if let Some(o) = aliases.get(&call.0) {
                call.0 = *o;
            }
        }
        // A callee's argument registers past its parameters hold whatever
        // the caller left there: compared only by a signature's count.
        let mut ours = c.original.clone();
        for e in [&mut ours, &mut theirs] {
            for call in &mut e.calls {
                if let Some(n) = argument_slots(names, call.0, || resident(&m.bus.ram, call.0)) {
                    call.1[n.min(4)..].fill(0);
                }
            }
        }
        let params = pointer_params(names, bare, &m.cpu.regs);
        let differences: Vec<String> = psx_core::callcheck::differences(&ours, &theirs, returns, o.steps)
            .iter()
            .map(|d| unsigned_callee(names, &m.bus.ram, &named(names, &params, d), &ours, &theirs))
            .collect();
        let tag = match (varied, o.cases) {
            (None, 1) => o.tag.to_string(),
            (None, _) => format!("{}-{hit}", o.tag),
            (Some(k), 1) => format!("{}-varied-{k}", o.tag),
            (Some(k), _) => format!("{}-{hit}-varied-{k}", o.tag),
        };
        let saved = match (differences.is_empty(), o.save) {
            (false, Some(dir)) => {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let p = dir.join(format!("{tag}.state"));
                std::fs::write(&p, m.save_state(None)).map_err(|e| format!("{}: {e}", p.display()))?;
                Some(p)
            }
            _ => None,
        };
        let mine = c.original.branches.iter().copied().filter(|(b, _)| branches.contains(b) || o.body.is_none());
        Ok(Case {
            hit,
            from: None,
            varied,
            dropped: false,
            args: [r[4], r[5], r[6], r[7]],
            differences,
            branches: mine.collect(),
            saved,
        })
    };
    while real < o.cases as u64 && m.bus.frames < limit {
        if !at(m) {
            m.step();
            continue;
        }
        if real == 0 {
            for &(s, e) in o.body.unwrap_or_default() {
                for pc in (s..e).step_by(4) {
                    let p = (pc & 0x1F_FFFF) as usize;
                    let Some(w) = m.bus.ram.get(p..p + 4) else { continue };
                    let w = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                    if psx_core::callcheck::conditional_branch(w, pc).is_some() {
                        branches.insert(pc);
                    }
                }
            }
        }
        real += 1;
        out.push(one(m, real, None, branches)?);
        // The same call with its arguments and what they point to varied,
        // each from the call's own state.
        if o.varied > 0 {
            let state = m.save_state(None);
            for k in 1..=o.varied as u64 {
                let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (real << 20) ^ k);
                vary(m, names, bare, &mut rng);
                out.push(one(m, real, Some(k), branches)?);
                let disc = m.bus.cdrom.disc.take();
                let (_, back) =
                    psx_core::Machine::load_state(&state, m.bus.bios.clone()).map_err(|e| format!("{e:?}"))?;
                *m = back;
                m.bus.cdrom.disc = disc;
            }
        }
        // On past this call's entry, to the next.
        m.step();
    }
    Ok(out)
}

/// A small, seeded random source (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// A value for a `size`-byte integer: half the time a small one
    /// (-16..16), else any.
    /// A value of `ty` when it is an integer: one of an enum's values, or
    /// any within a base type's size; with the size.
    fn of(&mut self, names: &Names, ty: &Type) -> Option<(u32, u32)> {
        if let Type::Named { name } = ty {
            match names.type_named(name).map(|t| &t.body) {
                Some(TypeBody::Typedef { ty }) => return self.of(names, ty),
                Some(TypeBody::Enum { values }) if !values.is_empty() => {
                    let v = values[self.next() as usize % values.len()].value;
                    return Some((names.size_of(ty).unwrap_or(4).clamp(1, 4), v as u32));
                }
                _ => {}
            }
        }
        let (size, signed) = integer(names, ty)?;
        Some((size, self.value(size, signed)))
    }

    fn value(&mut self, size: u32, signed: bool) -> u32 {
        let v = if self.next() & 1 == 0 { (self.next() % 33) as i32 - 16 } else { self.next() as i32 };
        let bits = 8 * size.min(4);
        if bits == 32 {
            return v as u32;
        }
        let low = (v as u32) & ((1u32 << bits) - 1);
        if signed && low >> (bits - 1) == 1 { low | !((1u32 << bits) - 1) } else { low }
    }
}

/// A base type's size and whether it is signed; None for any other type.
fn integer(names: &Names, ty: &Type) -> Option<(u32, bool)> {
    let Type::Named { name } = ty else { return None };
    if let Some(TypeDef { body: TypeBody::Typedef { ty }, .. }) = names.type_named(name) {
        return integer(names, ty);
    }
    if let Some(TypeDef { body: TypeBody::Enum { .. }, .. }) = names.type_named(name) {
        return Some((4, true));
    }
    let &(_, size) = BASE_TYPES.iter().find(|(n, _)| n == name)?;
    let unsigned = name.starts_with('u') || ["byte", "bool"].contains(&name.as_str());
    (size > 0 && name != "float" && name != "double").then_some((size, !unsigned))
}

/// Writes random values of `ty`'s integers at `addr` in main RAM: each
/// field of a struct and element of an array in turn; pointers, floats
/// and types of unknown layout are left as they are.
fn vary_memory(m: &mut psx_core::Machine, names: &Names, ty: &Type, addr: u32, rng: &mut Rng, depth: u32) {
    if depth > 8 {
        return;
    }
    if let Some((size, v)) = rng.of(names, ty) {
        for k in 0..size {
            if let Some(p) = crate::names_bind::canonical(addr + k) {
                m.bus.ram[(p & 0x1F_FFFF) as usize] = (v >> (8 * k)) as u8;
            }
        }
        return;
    }
    match ty {
        Type::Named { name } => match names.type_named(name).map(|t| &t.body) {
            Some(TypeBody::Struct { fields }) => {
                for f in fields {
                    vary_memory(m, names, &f.ty, addr + f.offset, rng, depth + 1);
                }
            }
            Some(TypeBody::Typedef { ty }) => vary_memory(m, names, ty, addr, rng, depth + 1),
            _ => {}
        },
        Type::Array { of, len } => {
            let size = names.size_of(of).unwrap_or(0);
            for k in 0..(*len).min(64) {
                if size > 0 {
                    vary_memory(m, names, of, addr + k * size, rng, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// The call `m` is at the entry of, with its integer arguments and the
/// memory its pointer arguments point to varied within their types, by
/// `function`'s signature in `names` (nothing without one).
fn vary(m: &mut psx_core::Machine, names: &Names, function: &str, rng: &mut Rng) {
    let Some(sig) = names.functions.iter().find(|f| f.name.text == function).and_then(|f| f.signature.as_ref()) else {
        return;
    };
    for (k, p) in sig.params.iter().take(4).enumerate() {
        match &p.ty {
            Type::Pointer { to, offset } => {
                let base = m.cpu.regs[4 + k].wrapping_sub(*offset);
                if crate::names_bind::canonical(base).is_some() {
                    vary_memory(m, names, to, base, rng, 0);
                }
            }
            ty => {
                if let Some((_, v)) = rng.of(names, ty) {
                    m.cpu.regs[4 + k] = v;
                }
            }
        }
    }
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CodeCheckArgs {
    /// Functions: each one's name in names/ (`[MODULE:]name`), and the name of its C in decomp/. Default: every function decomp/ defines whose C or names changed since its last check.
    #[serde(default)]
    pub functions: Vec<String>,
    /// Run from this state instead of the recordings, named (`states list`) or a state file: the game's calls of the function from there are the cases.
    #[arg(long)]
    #[serde(default)]
    pub state: Option<PathBuf>,
    /// Take the cases from these scenarios' recordings. Default: every scenario's recordings that reach the function (by the coverage kept beside each).
    #[arg(long = "scenario")]
    #[serde(default)]
    pub scenarios: Vec<String>,
    /// Take the cases from these recordings: NAME, or ID/NAME for one scenario's.
    #[arg(long = "recording")]
    #[serde(default)]
    pub recordings: Vec<String>,
    /// The BIOS (default: as run resolves it).
    #[arg(long)]
    #[serde(default)]
    pub bios: Option<PathBuf>,
    /// Calls to check per function (default 3 from a state, 8 over recordings, spread over all the calls made).
    #[arg(long)]
    #[serde(default)]
    pub cases: Option<usize>,
    /// From a state: fields to wait for each call.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_fields")]
    pub fields: u64,
    /// Wall-clock limit per recording's replay, in seconds.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_timeout")]
    pub timeout: u64,
    /// Varied calls after each call: its integer arguments, and the memory its pointer arguments point to, set at random within their types (by its signature in names/), to reach branches no recording does.
    #[arg(long, default_value_t = 2)]
    #[serde(default = "d_varied")]
    pub varied: usize,
}

fn d_varied() -> usize {
    2
}

fn d_fields() -> u64 {
    600
}
fn d_timeout() -> u64 {
    600
}

/// Steps one call may take before it counts as not returning.
const CALL_STEPS: u64 = 5_000_000;

fn messages_json(m: &[Message]) -> Value {
    json!(m.iter().map(|m| json!({"file": m.file, "line": m.line, "message": m.text})).collect::<Vec<_>>())
}

/// The C files in decomp/, in order.
fn c_files(root: &Path) -> Vec<PathBuf> {
    c_files_in(root, "decomp")
}

fn check_cmd(ctx: &Ctx, a: CodeCheckArgs) -> crate::Result<(bool, Value)> {
    let root = &ctx.root;
    let sources = c_files(root);
    if sources.is_empty() {
        return Err(Error::invalid("decomp/ holds no C: write the function's C as decomp/<name>.c"));
    }
    let names = &*crate::names_bind::project_names();
    let one = (a.functions.len() == 1).then(|| a.functions[0].clone());
    let build = match build(names, &sources, &root.join(".psx/decomp")) {
        Ok(b) => b,
        Err(m) => {
            let mut v = json!({"status": "build_failed", "messages": messages_json(&m)});
            match &one {
                Some(f) => v["function"] = json!(f),
                None => v["functions"] = json!(a.functions),
            }
            return Ok((false, v));
        }
    };
    let wanted = if a.functions.is_empty() { changed(root, names) } else { a.functions.clone() };
    if wanted.is_empty() {
        return Ok((
            true,
            json!({"status": "unchanged", "functions": [],
                   "why": "every function decomp/ defines and names/ names was checked on this C and these names"}),
        ));
    }
    let resolved = crate::context::resolve(root, None, a.bios.clone(), None, false)?;
    if let Some(e) = resolved.setup_error() {
        return Err(e);
    }
    let bios_path = resolved.bios.clone().unwrap();
    let bios = std::fs::read(&bios_path).map_err(|e| Error::io(&bios_path, e))?;
    let media = resolved.media.clone();
    if let Some(f) = one {
        return check_one(ctx, &a, names, &build, &f, &bios, media.as_deref());
    }
    let mut ok = true;
    let mut out = Vec::new();
    for f in &wanted {
        let v = match check_one(ctx, &a, names, &build, f, &bios, media.as_deref()) {
            Ok((pass, v)) => {
                ok &= pass;
                v
            }
            Err(e) => {
                ok = false;
                json!({"function": f, "status": "error", "error": e.to_value()["error"]})
            }
        };
        out.push(v);
    }
    Ok((ok, json!({"status": if ok { "passed" } else { "failed" }, "functions": out})))
}

/// decomp/'s functions that names/ names whose last check was on other C
/// or other names, or that have none.
fn changed(root: &Path, names: &Names) -> Vec<String> {
    let mut out: Vec<String> = c_functions(root)
        .into_iter()
        .map(|(f, _, _)| f)
        .filter(|f| entry(names, f).is_ok())
        .filter(|f| last_check_in(root, names, f).is_none_or(|k| k["stale"] == true))
        .collect();
    out.dedup();
    out
}

/// A recording a check takes cases from: its scenario and name.
struct Rec {
    id: String,
    name: String,
}

/// The captured recordings `a` asks for, or by default every one whose
/// kept coverage reaches `entry` (one with no coverage kept is tried).
fn recordings(root: &Path, a: &CodeCheckArgs, entry: u32) -> Vec<Rec> {
    let ids = if a.scenarios.is_empty() { crate::scenario::ids(root) } else { a.scenarios.clone() };
    let mut out = Vec::new();
    for id in ids {
        let l = crate::scenario::Loaded::load(root, &id);
        for (name, file, r) in &l.recordings {
            let Ok(r) = r else { continue };
            if r.header.source.kind != psx_scenario::scenario::SourceKind::Captured {
                continue;
            }
            if !a.recordings.is_empty() && !a.recordings.iter().any(|x| x == name || *x == format!("{id}/{name}")) {
                continue;
            }
            if a.scenarios.is_empty() && a.recordings.is_empty() && !reaches(file, entry) {
                continue;
            }
            out.push(Rec { id: id.clone(), name: name.clone() });
        }
    }
    out
}

/// Whether the coverage kept beside `recording` executed `entry`; true
/// when none is kept.
fn reaches(recording: &Path, entry: u32) -> bool {
    let p = recording.with_extension("coverage.json");
    let Some(v) = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return true };
    let hex = |v: &Value| v.as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let want = entry & 0x1FFF_FFFF;
    v["images"].as_array().into_iter().flatten().flat_map(|i| i["executed"].as_array().into_iter().flatten()).any(|r| {
        matches!((hex(&r[0]), hex(&r[1])), (Some(s), Some(e)) if (s & 0x1FFF_FFFF..e & 0x1FFF_FFFF).contains(&want))
    })
}

/// `n` of `items`, spread evenly over them, in order.
fn spread<T>(items: Vec<T>, n: usize) -> Vec<T> {
    let len = items.len();
    if len <= n {
        return items;
    }
    let pick: BTreeSet<usize> = (0..n).map(|k| k * len / n).collect();
    items.into_iter().enumerate().filter(|(k, _)| pick.contains(k)).map(|(_, t)| t).collect()
}

#[allow(clippy::too_many_arguments)]
fn check_one(
    ctx: &Ctx,
    a: &CodeCheckArgs,
    names: &Names,
    build: &Build,
    function: &str,
    bios: &[u8],
    media: Option<&Path>,
) -> crate::Result<(bool, Value)> {
    let root = &ctx.root;
    let original = entry(names, function).map_err(Error::invalid)?;
    let bare = function.rsplit_once(':').map_or(function, |(_, n)| n);
    let module = function.rsplit_once(':').map(|(m, _)| m);
    let body = crate::names_bind::body(original, module);
    let save = root.join(".psx/decomp/cases");
    let mut branches = BTreeSet::new();
    let mut cases = Vec::new();
    let mut held = String::new();
    let mut v = json!({"function": function});
    let from: Value;
    if let Some(state) = &a.state {
        let (states, _) = crate::states::Dirs::default().get(ctx);
        let state = crate::run::named_state(&states, state.clone());
        let bytes = std::fs::read(&state).map_err(|e| Error::io(&state, e))?;
        let (_, mut m) =
            psx_core::Machine::load_state(&bytes, bios.to_vec()).map_err(|e| Error::new("invalid_state", e))?;
        m.bus.cdrom.disc = crate::run_native::open_media(media)?.disc;
        let n = a.cases.unwrap_or(3);
        let opts = CheckOpts {
            cases: n,
            fields: a.fields,
            steps: CALL_STEPS,
            save: Some(&save),
            tag: bare,
            body: body.as_deref(),
            varied: a.varied,
        };
        cases = check(&mut m, names, build, function, &opts, &mut branches).map_err(Error::invalid)?;
        if !cases.is_empty() {
            held = entry_bytes(&m, original, body.as_deref());
        }
        if cases.is_empty() {
            v["why"] = json!(format!("the game did not call it within {} fields of the state", a.fields));
        }
        from = json!({"state": state});
    } else {
        let n = a.cases.unwrap_or(8);
        let recs = recordings(root, a, original);
        let mut kept = Vec::new();
        let mut replays = Vec::new();
        let mut disc = None;
        for r in &recs {
            let mut prepare = |m: &mut psx_core::Machine| m.redirects.insert(original, psx_core::Redirect::Keep(n));
            let (diffs, mut m) = crate::scenario::replay(
                ctx,
                &r.id,
                &r.name,
                std::time::Duration::from_secs(a.timeout),
                &mut |_, _| {},
                Some(&mut prepare),
                None,
            )?;
            let calls = m.redirects.hits(original);
            for (call, state) in m.redirects.kept(original) {
                kept.push((r, call, state));
            }
            disc = m.bus.cdrom.disc.take().or(disc);
            let mut j = json!({"scenario": r.id, "recording": r.name, "calls": calls});
            if !diffs.is_empty() {
                j["replay_differs"] = json!(diffs);
            }
            replays.push(j);
        }
        for (r, call, state) in spread(kept, n) {
            let (_, mut m) =
                psx_core::Machine::load_state(&state, bios.to_vec()).map_err(|e| Error::new("invalid_state", e))?;
            m.bus.cdrom.disc = disc.take();
            if held.is_empty() {
                held = entry_bytes(&m, original, body.as_deref());
            }
            let tag = format!("{bare}-{}-{}-{call}", r.id, r.name);
            let opts = CheckOpts {
                cases: 1,
                fields: 1,
                steps: CALL_STEPS,
                save: Some(&save),
                tag: &tag,
                body: body.as_deref(),
                varied: a.varied,
            };
            let got = check(&mut m, names, build, function, &opts, &mut branches);
            disc = m.bus.cdrom.disc.take();
            for mut c in got.map_err(Error::invalid)? {
                c.hit = call;
                c.from = Some((r.id.clone(), r.name.clone()));
                cases.push(c);
            }
        }
        if cases.is_empty() {
            v["why"] = json!(if recs.is_empty() {
                "no captured recording reaches it (by the coverage kept beside each): record one that does, or pass state".to_string()
            } else {
                "the recordings made no call to it".to_string()
            });
        }
        from = json!({"recordings": replays});
    }
    if a.varied > 0 {
        v["varied_dropped"] = json!(cases.iter().filter(|c| c.dropped).count());
    }
    cases.retain(|c| !c.dropped);
    let failed = cases.iter().filter(|c| !c.differences.is_empty()).count();
    let ok = !cases.is_empty() && failed == 0;
    v["status"] = json!(if cases.is_empty() {
        "no_calls"
    } else if ok {
        "passed"
    } else {
        "failed"
    });
    v["passed"] = json!(cases.len() - failed);
    v["failed"] = json!(failed);
    v["cases"] = json!(cases.iter().map(case_json).collect::<Vec<_>>());
    if !branches.is_empty() || body.is_some() {
        let reached: BTreeSet<(u32, bool)> = cases.iter().flat_map(|c| c.branches.iter().copied()).collect();
        let mut missed = Vec::new();
        for &b in &branches {
            for (way, word) in [(true, "taken"), (false, "fell through")] {
                if !reached.contains(&(b, way)) {
                    missed.push(format!("0x{b:08X} {word}"));
                }
            }
        }
        v["branch_ways"] =
            json!({"reached": 2 * branches.len() - missed.len(), "of": 2 * branches.len(), "missed": missed});
    }
    for (k, x) in from.as_object().unwrap() {
        v[k] = x.clone();
    }
    v["entry_bytes"] = json!(held);
    save_check(root, names, function, &from, &v);
    v.as_object_mut().unwrap().remove("entry_bytes");
    Ok((ok, v))
}

fn case_json(c: &Case) -> Value {
    let mut j = json!({"args": c.args.map(|a| format!("0x{a:08X}")), "passed": c.differences.is_empty()});
    match &c.from {
        Some((id, name)) => {
            j["scenario"] = json!(id);
            j["recording"] = json!(name);
            j["call"] = json!(c.hit);
        }
        None => j["hit"] = json!(c.hit),
    }
    if let Some(k) = c.varied {
        j["varied"] = json!(k);
    }
    if !c.differences.is_empty() {
        j["differences"] = json!(c.differences);
    }
    if let Some(p) = &c.saved {
        j["state"] = json!(p);
    }
    j
}

/// Where each function's last check is kept (`<function>.json`), and the
/// log of every check (`checks.jsonl`), for psx web's Code view.
pub const CHECKS: &str = ".psx/decomp/checks";

/// A hash of decomp/'s C, to tell a check made on other C (FNV-1a over
/// the files' names and bytes, in order).
pub fn sources_hash(root: &Path) -> String {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("decomp"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c" || x == "h"))
        .collect();
    files.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for f in files {
        let name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
        for b in name.bytes().chain([0]).chain(std::fs::read(&f).unwrap_or_default()) {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// The file a function's last check is kept in.
fn check_file(root: &Path, function: &str) -> PathBuf {
    let safe: String = function.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).collect();
    root.join(CHECKS).join(format!("{safe}.json"))
}

/// A hash of the names files (names/ and .psx/names/), to tell a check
/// made on other names.
pub fn names_hash(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "names") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("names"), &mut files);
    walk(&root.join(".psx/names"), &mut files);
    files.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for f in files {
        let name = f.strip_prefix(root).unwrap_or(&f).to_string_lossy().into_owned();
        for b in name.bytes().chain([0]).chain(std::fs::read(&f).unwrap_or_default()) {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Keep a check's result: the function's last, and a line in the log.
/// FNV-1a over `parts`, each ended by a 0.
fn fnv<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in p.iter().chain(&[0]) {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// The words of C text that could name something.
fn idents(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
        .map(String::from)
        .collect()
}

fn type_words(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Named { name } => out.push(name.clone()),
        Type::Pointer { to, .. } => type_words(to, out),
        Type::Array { of, .. } => type_words(of, out),
        Type::Function(sig) => {
            type_words(&sig.ret, out);
            for p in &sig.params {
                type_words(&p.ty, out);
            }
        }
    }
}

/// What a function's check depends on, as the hashes `(sources, names)`
/// it keeps, so the check goes stale only when they change:
/// - sources: decomp/'s headers, the file that defines the function, and
///   the files of the decomp/ functions those name (the C's own takes a
///   call over, so a callee's C is part of the function's);
/// - names: the header's declarations of the names that C uses and the
///   function's own, with the types they reach (comments and lines left
///   out, so other names' edits leave it alone).
pub fn check_key(root: &Path, names: &Names, function: &str) -> (String, String) {
    let bare = function.rsplit_once(':').map_or(function, |(_, n)| n);
    let defs = c_functions(root);
    let mut files: BTreeSet<String> = defs.iter().filter(|(n, _, _)| n == bare).map(|(_, f, _)| f.clone()).collect();
    let mut headers: Vec<PathBuf> = std::fs::read_dir(root.join("decomp"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "h"))
        .collect();
    headers.sort();
    let mut words: BTreeSet<String> = BTreeSet::from([bare.to_string()]);
    for h in &headers {
        words.extend(idents(&std::fs::read_to_string(h).unwrap_or_default()));
    }
    let mut todo: Vec<String> = files.iter().cloned().collect();
    while let Some(f) = todo.pop() {
        for w in idents(&std::fs::read_to_string(root.join("decomp").join(&f)).unwrap_or_default()) {
            for (n, file, _) in &defs {
                if *n == w && files.insert(file.clone()) {
                    todo.push(file.clone());
                }
            }
            words.insert(w);
        }
    }
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for h in &headers {
        parts.push(h.file_name().unwrap_or_default().to_string_lossy().as_bytes().to_vec());
        parts.push(std::fs::read(h).unwrap_or_default());
    }
    for f in &files {
        parts.push(f.as_bytes().to_vec());
        parts.push(std::fs::read(root.join("decomp").join(f)).unwrap_or_default());
    }
    let sources = fnv(parts.iter().map(|p| p.as_slice()));

    // The names those words reach: a type by its name or one of its enum
    // values, a global or function by its name, then the types each holds.
    let mut reached: BTreeSet<String> = BTreeSet::new();
    let mut todo: Vec<String> = words.into_iter().collect();
    let (mut types, mut globals, mut functions) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    while let Some(w) = todo.pop() {
        if !reached.insert(w.clone()) {
            continue;
        }
        let mut more = Vec::new();
        for (i, t) in names.types.iter().enumerate() {
            let value = matches!(&t.body, TypeBody::Enum { values } if values.iter().any(|v| v.name.text == w));
            if (t.name.text == w || value) && types.insert(i) {
                more.push(t.name.text.clone());
                match &t.body {
                    TypeBody::Struct { fields } | TypeBody::Union { fields } => {
                        fields.iter().for_each(|f| type_words(&f.ty, &mut more))
                    }
                    TypeBody::Typedef { ty } => type_words(ty, &mut more),
                    TypeBody::Enum { .. } => {}
                }
            }
        }
        for (i, g) in names.globals.iter().enumerate() {
            if g.name.text == w && globals.insert(i) {
                g.ty.iter().for_each(|ty| type_words(ty, &mut more));
            }
        }
        for (i, f) in names.functions.iter().enumerate() {
            if f.name.text == w && functions.insert(i) {
                if let Some(sig) = &f.signature {
                    type_words(&Type::Function(sig.clone()), &mut more);
                }
            }
        }
        todo.extend(more);
    }
    let used = Names {
        types: types.into_iter().map(|i| names.types[i].clone()).collect(),
        globals: globals.into_iter().map(|i| names.globals[i].clone()).collect(),
        functions: functions
            .into_iter()
            .map(|i| {
                let mut f = names.functions[i].clone();
                f.locals.clear();
                f
            })
            .collect(),
        ..Default::default()
    };
    let (h, ld, _) = header(&used);
    (sources, fnv([h.as_bytes(), ld.as_bytes()]))
}

fn save_check(root: &Path, names: &Names, function: &str, from: &Value, v: &Value) {
    let (sources, names_key) = check_key(root, names, function);
    let commit = crate::git::head(root);
    let first = v["cases"].as_array().and_then(|c| c.iter().find(|c| c["passed"] == false)).cloned();
    let kept = json!({
        "function": function, "status": v["status"], "passed": v["passed"], "failed": v["failed"],
        "at": crate::now_ms() as u64, "commit": commit, "sources": sources, "names": names_key,
        "state": from["state"], "recordings": from["recordings"], "branch_ways": v["branch_ways"],
        "first_failure": first, "messages": v["messages"],
        "entry_bytes": v["entry_bytes"],
    });
    let p = check_file(root, function);
    if std::fs::create_dir_all(p.parent().unwrap()).is_err() {
        return;
    }
    let _ = crate::write_atomic(&p, serde_json::to_vec_pretty(&kept).unwrap().as_slice());
    let line = json!({"t": kept["at"], "function": function, "status": kept["status"],
        "passed": kept["passed"], "failed": kept["failed"], "commit": kept["commit"]});
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(root.join(CHECKS).join("checks.jsonl"))
    {
        let _ = std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes());
    }
}

/// The functions decomp/'s C defines, as `(name, file, line)`: a line at
/// the left margin that names a function and does not end in `;` (a
/// prototype), with its body's `{` there or on the next line.
pub fn c_functions(root: &Path) -> Vec<(String, String, usize)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("decomp"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let file = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let lines: Vec<&str> = text.lines().collect();
        let mut comment = false;
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_end();
            // Block comments that span lines.
            if comment {
                comment = !t.contains("*/");
                continue;
            }
            if t.trim_start().starts_with("/*") && !t.contains("*/") {
                comment = true;
                continue;
            }
            let first = t.chars().next();
            if !first.is_some_and(|c| c.is_ascii_alphabetic() || c == '_') || t.ends_with(';') {
                continue;
            }
            let word = t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).next().unwrap_or("");
            if ["typedef", "struct", "union", "enum", "return", "if", "for", "while", "switch", "extern"]
                .contains(&word)
            {
                continue;
            }
            let Some(paren) = t.find('(') else { continue };
            let name: String = t[..paren]
                .trim_end()
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let body = t.ends_with('{') || lines.get(i + 1).is_some_and(|n| n.trim_start().starts_with('{'));
            if !name.is_empty() && body && !t[..paren].contains('=') {
                out.push((name, file.clone(), i + 1));
            }
        }
    }
    out
}

/// A function's last check, with `stale` when its C or the names it uses
/// changed since ([`check_key`]).
pub fn last_check(root: &Path, function: &str) -> Option<Value> {
    last_check_in(root, &psx_symbols::names::load(root), function)
}

/// [`last_check`] with the names loaded.
pub fn last_check_in(root: &Path, names: &Names, function: &str) -> Option<Value> {
    let mut k: Value = serde_json::from_slice(&std::fs::read(check_file(root, function)).ok()?).ok()?;
    let (sources, names_key) = check_key(root, names, function);
    k["stale"] =
        json!(k["sources"].as_str() != Some(sources.as_str()) || k["names"].as_str() != Some(names_key.as_str()));
    Some(k)
}

/// The function's C as decomp/ defines it: its file, line and text, from
/// its first line to the brace that closes its body.
pub fn c_of(root: &Path, function: &str) -> Option<(String, usize, String)> {
    let (_, file, line) = c_functions(root).into_iter().find(|(n, _, _)| n == function)?;
    let text = std::fs::read_to_string(root.join("decomp").join(&file)).ok()?;
    let mut depth = 0i32;
    let mut out = Vec::new();
    let mut opened = false;
    for l in text.lines().skip(line - 1) {
        out.push(l);
        for c in l.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            break;
        }
    }
    Some((format!("decomp/{file}"), line, out.join("\n")))
}

/// Functions named, in C, and passing their last check on the current C
/// and names, for code inventory, port status and psx web's Stats.
pub fn counts(root: &Path) -> Value {
    let names = psx_symbols::names::load(root);
    let c = c_functions(root);
    let passing = c
        .iter()
        .filter(|(n, _, _)| {
            last_check_in(root, &names, n).is_some_and(|k| k["status"] == "passed" && k["stale"] == false)
        })
        .count();
    json!({"named": names.functions.len(), "in_c": c.len(), "passing": passing})
}

/// The bytes a replaced function's entry must hold: up to this many, from
/// the entry to the end of its first chunk.
const GUARD: u32 = 64;

/// The bytes at `entry` in `m`'s RAM, as a check keeps them (hex) so a
/// whole run replaces the function only while they are there.
fn entry_bytes(m: &psx_core::Machine, entry: u32, body: Option<&[(u32, u32)]>) -> String {
    let end = body.and_then(|b| b.iter().find(|(s, e)| (*s..*e).contains(&entry))).map_or(entry + GUARD, |c| c.1);
    let at = (entry & 0x1F_FFFF) as usize;
    let n = end.saturating_sub(entry).min(GUARD) as usize;
    m.bus.ram.get(at..at + n).map(|b| b.iter().map(|x| format!("{x:02x}")).collect()).unwrap_or_default()
}

/// One function a whole run sends to the agent's C.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Replaced {
    pub name: String,
    pub entry: u32,
    pub at: u32,
    /// What the entry must hold (from its last check), else replaced
    /// wherever the CPU reaches it.
    pub guard: Option<Vec<u8>>,
}

/// The agent's C for a whole run (`--decomp`, docs/decomp-plan.md, step
/// 13): the build, the functions it replaces, and those left out with why.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Replacement {
    pub image: Vec<u8>,
    pub functions: Vec<Replaced>,
    pub left_out: Vec<(String, String)>,
}

impl Replacement {
    /// `which` empty: the functions whose last check passed on this C and
    /// these names; `all`: every function decomp/ defines and names/
    /// names; otherwise those named.
    pub fn new(root: &Path, which: &[String]) -> crate::Result<Replacement> {
        let sources = c_files(root);
        if sources.is_empty() {
            return Err(Error::invalid("--decomp: decomp/ holds no C"));
        }
        // The root's names, not the process's: psx web plays with the C in its own process.
        let names = &psx_symbols::names::load(root);
        let build = build(names, &sources, &root.join(".psx/decomp/run")).map_err(|m| {
            Error::invalid("--decomp: the C does not build (psx code check gives the messages)")
                .with("messages", messages_json(&m))
        })?;
        let all = which.iter().any(|w| w == "all");
        let wanted: Vec<String> = if which.is_empty() || all {
            let mut c: Vec<String> = c_functions(root).into_iter().map(|(f, _, _)| f).collect();
            c.dedup();
            c
        } else {
            which.to_vec()
        };
        let (mut functions, mut left_out) = (Vec::new(), Vec::new());
        for f in wanted {
            let bare = f.rsplit_once(':').map_or(f.as_str(), |(_, n)| n).to_string();
            let entry = match entry(names, &f) {
                Ok(e) => e,
                Err(e) => {
                    left_out.push((f, e));
                    continue;
                }
            };
            let Some(&at) = build.functions.get(&bare) else {
                left_out.push((f, "decomp/ does not define it".into()));
                continue;
            };
            let check = last_check_in(root, names, &f);
            if which.is_empty() {
                let why = match &check {
                    None => Some("never checked"),
                    Some(k) if k["status"] != "passed" => Some("its last check did not pass"),
                    Some(k) if k["stale"] == true => Some("its C or the names it uses changed since its last check"),
                    _ => None,
                };
                if let Some(why) = why {
                    left_out.push((f, why.into()));
                    continue;
                }
            }
            let guard =
                check.as_ref().and_then(|k| k["entry_bytes"].as_str()).filter(|h| !h.is_empty()).and_then(|h| {
                    (0..h.len() / 2)
                        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok())
                        .collect::<Option<Vec<u8>>>()
                });
            let module = f.rsplit_once(':').is_some()
                || names.functions.iter().any(|n| n.address == entry && n.module.is_some());
            if guard.is_none() && module {
                left_out.push((
                    f,
                    "an overlay's function with no check to say what its entry holds: check it first".into(),
                ));
                continue;
            }
            functions.push(Replaced { name: f, entry, at, guard });
        }
        if build.image.len() > (FIX_BASE - BASE) as usize {
            return Err(Error::invalid("--decomp: the C's build runs into where fixes/ loads"));
        }
        Ok(Replacement { image: build.image, functions, left_out })
    }

    /// Loads the build where the game cannot see it and sends the calls
    /// there; counts start from here.
    pub fn install(&self, m: &mut psx_core::Machine) {
        load_hidden(m, BASE, &self.image);
        for f in &self.functions {
            let to = psx_core::Redirect::To(f.at);
            match &f.guard {
                Some(g) => m.redirects.insert_guarded(f.entry, to, g.clone()),
                None => m.redirects.insert(f.entry, to),
            }
        }
        m.redirects.count_from_here();
    }

    /// What the replacement did on `m` since [`Replacement::install`].
    pub fn report(&self, m: &psx_core::Machine) -> Value {
        let r = &m.redirects;
        let share = if r.steps == 0 { 0.0 } else { r.hidden_steps as f64 / r.steps as f64 };
        json!({
            "replaced": self.functions.iter().map(|f| {
                let mut j = json!({"function": f.name, "address": format!("0x{:08X}", f.entry), "calls": r.hits(f.entry)});
                if f.guard.is_some() {
                    j["calls_elsewhere"] = json!(r.passed_over(f.entry));
                }
                j
            }).collect::<Vec<_>>(),
            "left_out": self.left_out.iter().map(|(f, why)| json!({"function": f, "why": why})).collect::<Vec<_>>(),
            "steps": r.steps, "steps_in_c": r.hidden_steps, "share_in_c": (share * 1e6).round() / 1e6,
        })
    }
}

/// Functions whose C passed a check and whose last check fails, with the
/// last pass's commit and the first failing case's first difference
/// (port status shows them).
pub fn regressed(root: &Path) -> Vec<(String, Option<String>, String)> {
    let names = psx_symbols::names::load(root);
    let log = std::fs::read_to_string(root.join(CHECKS).join("checks.jsonl")).unwrap_or_default();
    let lines: Vec<Value> = log.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut out = Vec::new();
    for (f, _, _) in c_functions(root) {
        let Some(k) = last_check_in(root, &names, &f) else { continue };
        if k["status"] != "failed" {
            continue;
        }
        let Some(pass) = lines.iter().rev().find(|l| l["function"] == json!(f) && l["status"] == "passed") else {
            continue;
        };
        let first = k["first_failure"]["differences"][0].as_str().unwrap_or("a case differs").to_string();
        out.push((f, pass["commit"].as_str().map(String::from), first));
    }
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The log of replays with the C (`--decomp`): a line per recording.
const RUNS: &str = ".psx/decomp/runs.jsonl";

/// Keeps a replay with the C in [`RUNS`], for the Code view's share of the
/// game in the C over time.
pub fn log_run(root: &Path, scenario: &str, recording: &str, status: &str, share: &Value, replaced: &Value) {
    let line = json!({"t": crate::now_ms() as u64, "scenario": scenario, "recording": recording, "status": status,
        "share_in_c": share, "functions": replaced.as_array().map(|a| a.len()).unwrap_or(0)});
    let p = root.join(RUNS);
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes());
    }
}

/// psx web's Code view: the names, the functions in C with their last
/// checks, and those scenarios turn off (docs/decomp-plan.md, step 14).
pub fn overview(root: &Path) -> Value {
    let names = psx_symbols::names::load(root);
    let guessed = names.functions.iter().filter(|f| f.name.guess).count();
    let addr = |n: &str| {
        names
            .functions
            .iter()
            .find(|f| f.name.text == n)
            .map(|f| json!({"address": format!("0x{:08X}", f.address), "module": f.module, "guess": f.name.guess}))
    };
    // Functions scenarios turn off, with the scenarios.
    let mut off: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for id in crate::scenario::ids(root) {
        let Ok(text) = std::fs::read_to_string(root.join(format!("scenarios/{id}.toml"))) else { continue };
        if let Ok(s) = psx_scenario::scenario::Scenario::parse(&text) {
            for f in s.off.keys() {
                off.entry(f.clone()).or_default().push(id.clone());
            }
        }
    }
    let c: Vec<Value> = c_functions(root)
        .into_iter()
        .map(|(name, file, line)| {
            let mut v = json!({"name": name, "file": format!("decomp/{file}"), "line": line,
                "names": addr(&name), "off": off.get(&name)});
            if let Some(k) = last_check_in(root, &names, &name) {
                v["check"] = k;
            }
            v
        })
        .collect();
    let log = std::fs::read_to_string(root.join(CHECKS).join("checks.jsonl")).unwrap_or_default();
    let checks: Vec<Value> = log.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    json!({
        "names": {"functions": names.functions.len(), "guessed": guessed, "globals": names.globals.len(), "types": names.types.len()},
        "c": c,
        "off": off.iter().map(|(f, s)| json!({"function": f, "scenarios": s, "names": addr(f)})).collect::<Vec<_>>(),
        "checks": checks.iter().rev().take(200).rev().collect::<Vec<_>>(),
        "compiler": compiler().map(|p| p.display().to_string()),
        "replaced_runs": std::fs::read_to_string(root.join(RUNS)).unwrap_or_default().lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>().into_iter().rev().take(400).rev().collect::<Vec<_>>(),
    })
}

impl Op for CodeCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(check_cmd(ctx, self))
    }
}

/// A GCC message line, `file:line:col: error: text` (or a warning).
fn message(l: &str) -> Option<Message> {
    let mut parts = l.splitn(4, ':');
    let (file, line, _col, text) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let line = line.trim().parse().ok()?;
    let text = text.trim();
    (text.starts_with("error") || text.starts_with("warning")).then(|| Message {
        file: Path::new(file).file_name().map_or(file.to_string(), |f| f.to_string_lossy().into_owned()),
        line,
        text: text.to_string(),
    })
}

/// The binutils `tool` beside `gcc`: `mipsel-linux-gnu-gcc-12` →
/// `mipsel-linux-gnu-objcopy` (binutils carry no compiler version).
fn binutil(gcc: &Path, tool: &str) -> PathBuf {
    let name = gcc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, exe) = match name.strip_suffix(".exe") {
        Some(s) => (s.to_string(), ".exe"),
        None => (name.clone(), ""),
    };
    let Some(at) = stem.rfind("gcc") else { return gcc.with_file_name(format!("{tool}{exe}")) };
    let rest = &stem[at + 3..];
    let versioned =
        rest.strip_prefix('-').is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    let rest = if versioned { "" } else { rest };
    gcc.with_file_name(format!("{}{tool}{rest}{exe}", &stem[..at]))
}

/// Where fixes/ loads in the hidden region: after decomp/'s build, so a run
/// can have both.
pub const FIX_BASE: u32 = BASE + 0x0010_0000;

/// Put `image` in the hidden region at `base`, keeping what else is there.
pub fn load_hidden(m: &mut psx_core::Machine, base: u32, image: &[u8]) {
    let o = (base - BASE) as usize;
    let h = m.bus.hidden.get_or_insert_with(Vec::new);
    if h.len() < o + image.len() {
        h.resize(o + image.len(), 0);
    }
    h[o..o + image.len()].copy_from_slice(image);
}

/// The C files in `root/dir`, sorted.
fn c_files_in(root: &Path, dir: &str) -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(root.join(dir))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    sources.sort();
    sources
}

/// A hash of fixes/'s C and headers, which a recording keeps.
pub fn fixes_hash(root: &Path) -> Option<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join(FIXES))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c" || x == "h"))
        .collect();
    if !files.iter().any(|f| f.extension().is_some_and(|x| x == "c")) {
        return None;
    }
    files.sort();
    let mut bytes = Vec::new();
    for f in files {
        bytes.extend(f.file_name().unwrap_or_default().to_string_lossy().bytes());
        bytes.push(0);
        bytes.extend(std::fs::read(&f).unwrap_or_default());
    }
    Some(psx_media::disc::sha256_bytes(&bytes)[..16].to_string())
}

/// Where the agent's fixes are: C replacing the game's functions that
/// keep input from the game every tick (docs/master-plan.md, A9).
pub const FIXES: &str = "fixes";

/// fixes/ built: each function it defines that names/ names replaces the
/// game's; the rest are its helpers. None when fixes/ holds no C.
pub fn fixes(root: &Path) -> crate::Result<Option<crate::adjust::Fixes>> {
    let Some(sources_hash) = fixes_hash(root) else { return Ok(None) };
    let names = &psx_symbols::names::load(root);
    let build = build_at(names, &c_files_in(root, FIXES), &root.join(".psx/fixes"), FIX_BASE)
        .map_err(|m| Error::invalid("fixes/: the C does not build").with("messages", messages_json(&m)))?;
    let mut functions = Vec::new();
    for (name, &at) in &build.functions {
        let Ok(address) = entry(names, name) else { continue };
        if names.functions.iter().any(|f| f.address == address && f.module.is_some()) {
            return Err(Error::invalid(format!(
                "fixes/: {name} is an overlay's function: a fix replaces a function of the main executable"
            )));
        }
        functions.push(crate::adjust::Fixed { name: name.clone(), address, at });
    }
    if functions.is_empty() {
        return Err(Error::invalid(
            "fixes/: its C defines no function names/ names: a fix is a game function's replacement, under the function's name",
        ));
    }
    Ok(Some(crate::adjust::Fixes { functions, image: build.image, sources: sources_hash }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binutils_sit_beside_the_compiler() {
        let t = |g: &str| binutil(Path::new(g), "objcopy");
        assert_eq!(t("/usr/bin/mipsel-linux-gnu-gcc"), Path::new("/usr/bin/mipsel-linux-gnu-objcopy"));
        assert_eq!(t("/usr/bin/mipsel-linux-gnu-gcc-12"), Path::new("/usr/bin/mipsel-linux-gnu-objcopy"));
        assert_eq!(t("C:/x/mipsel-none-elf-gcc.exe"), Path::new("C:/x/mipsel-none-elf-objcopy.exe"));
    }

    /// The Code view finds the functions the C defines, not its prototypes,
    /// types, comments or calls, and tells a check made on other C.
    #[test]
    fn c_functions_are_the_definitions() {
        let root = std::env::temp_dir().join(format!("psx-c-functions-{}", std::process::id()));
        std::fs::create_dir_all(root.join("decomp")).unwrap();
        let c = "#include \"names.h\"\n/* world_collide(a)\n   {\n*/\nint helper(int x);\ntypedef struct { int a; } T;\n\
static int helper(int x)\n{\n    return x + 1;\n}\n\nvoid world_collide(Actor *a) {\n    helper(a->x);\n}\nint table[2] = {1, 2};\n";
        std::fs::write(root.join("decomp/world.c"), c).unwrap();
        let fs = c_functions(&root);
        assert_eq!(fs, vec![("helper".into(), "world.c".into(), 7), ("world_collide".into(), "world.c".into(), 12)]);
        assert_eq!(
            c_of(&root, "helper").unwrap(),
            ("decomp/world.c".into(), 7, "static int helper(int x)\n{\n    return x + 1;\n}".into())
        );
        // A check kept on this C passes until the C changes.
        assert_eq!(counts(&root)["passing"], 0);
        save_check(
            &root,
            &Names::default(),
            "helper",
            &json!({}),
            &json!({"status": "passed", "passed": 3, "failed": 0}),
        );
        assert_eq!(counts(&root), json!({"named": 0, "in_c": 2, "passing": 1}));
        assert_eq!(last_check(&root, "helper").unwrap()["stale"], false);
        let h = sources_hash(&root);
        std::fs::write(root.join("decomp/world.c"), format!("{c}\n")).unwrap();
        assert_ne!(h, sources_hash(&root), "a change to the C changes the hash");
        assert_eq!(last_check(&root, "helper").unwrap()["stale"], true);
        assert_eq!(counts(&root)["passing"], 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn names(text: &str) -> (PathBuf, Names) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("psx-decomp-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("names")).unwrap();
        std::fs::write(dir.join("names/a.names"), text).unwrap();
        let names = psx_symbols::names::load(&dir);
        assert!(names.problems.is_empty(), "{:?}", names.problems);
        (dir, names)
    }

    /// A check goes stale with its own C, its callees' C, decomp/'s headers
    /// and the names it reaches, and with nothing else.
    #[test]
    fn a_check_goes_stale_with_what_it_depends_on() {
        let (root, _) = names(NAMES);
        std::fs::create_dir_all(root.join("decomp")).unwrap();
        let w = |f: &str, t: &str| std::fs::write(root.join(f), t).unwrap();
        w("decomp/twice.c", "#include \"names.h\"\nint twice(int a) {\n  return add(a, a) + player.x;\n}\n");
        w("decomp/add.c", "#include \"names.h\"\nint add(int a, int b) {\n  return a + b;\n}\n");
        w("decomp/other.c", "#include \"names.h\"\nvoid helper(int n) {\n  counter = n;\n}\n");
        let key = |f: &str| check_key(&root, &psx_symbols::names::load(&root), f);
        let (twice, other) = (key("twice"), key("helper"));
        let same = |what: &str, before: &(String, String), f: &str, stale: bool| {
            assert_eq!(*before != key(f), stale, "{what}: {f}");
        };

        w("decomp/other.c", "#include \"names.h\"\nvoid helper(int n) {\n  counter = n + 1;\n}\n");
        same("another function's C", &twice, "twice", false);
        same("its own C", &other, "helper", true);
        let other = key("helper");

        w("decomp/add.c", "#include \"names.h\"\nint add(int a, int b) {\n  return b + a;\n}\n");
        same("a callee's C", &twice, "twice", true);
        same("another's callee", &other, "helper", false);
        let twice = key("twice");

        w("names/a.names", &format!("{NAMES}global 0x80030000 int unused\n"));
        same("a name it does not use", &twice, "twice", false);
        w("names/a.names", &NAMES.replace("0x4 int x", "0x8 int x"));
        same("a field of a struct it reads", &twice, "twice", true);
        same("a struct it does not read", &other, "helper", false);
        let twice = key("twice");

        w("decomp/util.h", "#define TWO 2\n");
        same("a header", &twice, "twice", true);
        std::fs::remove_dir_all(root).unwrap();
    }

    const NAMES: &str = "struct Object 0x20 {\n  0x0 u16 kind\n  0x4 int x\n  0xA i16 odd\n  0x10 Object *next\n  0x14 u8 flags[4]\n}\nglobal 0x80020000 int counter\nglobal 0x80020100 Object player\nfunction 0x80010200 void helper(int n)\nfunction 0x80010100 int twice(int a)\n";

    #[test]
    fn the_header_asserts_every_offset() {
        let (_dir, n) = names(NAMES);
        let (h, ld, notes) = header(&n);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(h.contains("typedef struct Object Object;"));
        assert!(h.contains("_Static_assert(__builtin_offsetof(Object, odd) == 0xA"), "{h}");
        assert!(h.contains("_Static_assert(sizeof(Object) == 0x20"));
        assert!(h.contains("    Object *next;") && h.contains("    u8 flags[4];"));
        assert!(h.contains("extern Object player;") && h.contains("void helper(int n);"));
        assert!(ld.contains("PROVIDE(twice = 0x80010100);") && ld.contains("PROVIDE(counter = 0x80020000);"));
    }

    /// Builds `c` with `NAMES`, when a MIPS GCC is installed.
    fn built(c: &str) -> Option<(PathBuf, Result<Build, Vec<Message>>)> {
        if compiler().is_none() {
            eprintln!("no MIPS GCC: the build is not tested");
            return None;
        }
        let (dir, n) = names(NAMES);
        std::fs::create_dir_all(dir.join("decomp")).unwrap();
        let src = dir.join("decomp/twice.c");
        std::fs::write(&src, c).unwrap();
        let b = build(&n, &[src], &dir.join(".psx/decomp"));
        Some((dir, b))
    }

    #[test]
    fn the_header_compiles_with_its_asserts() {
        let Some((_d, b)) = built("#include \"names.h\"\nint probe(void) { return player.odd + sizeof(Object); }\n")
        else {
            return;
        };
        let b = b.unwrap();
        assert!(b.functions.contains_key("probe"));
    }

    #[test]
    fn compiler_errors_come_back_by_file_and_line() {
        let Some((_d, b)) = built("#include \"names.h\"\nint twice(int a) {\n  return a * nothing;\n}\n") else {
            return;
        };
        let e = b.unwrap_err();
        assert!(e.iter().any(|m| m.file == "twice.c" && m.line == 3 && m.text.contains("nothing")), "{e:?}");
    }

    /// The agent's C for psx-core's call-check test function, built with
    /// GCC and run where the game cannot see it, does what the original's
    /// hand-written machine code does.
    #[test]
    fn the_agent_s_c_does_what_the_original_does() {
        // f(a): counter = a + 1; helper(5); return a * 2 (callcheck's test function).
        let c = "#include \"names.h\"\nint twice(int a) {\n  counter = a + 1;\n  helper(5);\n  return a * 2;\n}\n";
        let Some((_d, b)) = built(c) else { return };
        let b = b.unwrap();
        let entry = b.functions["twice"];
        let (_n_dir, n) = names(NAMES);
        let mut m = psx_core::callcheck::tests_support::at_call();
        let c = psx_core::callcheck::compare(&mut m, &b.image, entry, returns(&n, "twice"), 10_000).unwrap();
        assert!(c.replacement.returned, "{:?}", c.replacement);
        assert_eq!(c.differences, Vec::<String>::new());
        // And a wrong one is caught.
        let wrong = c_with("counter = a + 2;");
        let Some((_d, b)) = built(&wrong) else { return };
        let b = b.unwrap();
        let c =
            psx_core::callcheck::compare(&mut m, &b.image, b.functions["twice"], returns(&n, "twice"), 10_000).unwrap();
        assert_eq!(c.differences, ["byte 0x80020000: original 0x15, replacement 0x16"]);
    }

    #[test]
    fn check_takes_the_game_s_calls_as_cases() {
        use psx_core::callcheck::tests_support::{at_call, jal, sw};
        use psx_core::padflow::test_program::{asm::*, load};
        let game = || {
            // The game calls twice(20), then twice(21), ..., keeping v0 at 0x80030000.
            let mut m = at_call();
            load(
                &mut m,
                0x8001_0400,
                &[
                    addiu(17, 0, 20),
                    or(4, 17, 0),
                    jal(0x8001_0100),
                    NOP,
                    addiu(17, 17, 1),
                    lui(1, 0x8003),
                    sw(2, 0, 1),
                    j(0x8001_0404),
                    NOP,
                ],
            );
            m.cpu.set_pc(0x8001_0400);
            m
        };
        let right = "#include \"names.h\"\nint twice(int a) {\n  counter = a + 1;\n  helper(5);\n  return a * 2;\n}\n";
        let Some((dir, b)) = built(right) else { return };
        let (_n_dir, n) = names(NAMES);
        let saved = dir.join("cases");
        let opts = |cases| CheckOpts {
            cases,
            fields: 10,
            steps: 10_000,
            save: Some(&saved),
            tag: "twice",
            body: None,
            varied: 0,
        };
        let mut br = BTreeSet::new();
        let cases = check(&mut game(), &n, &b.unwrap(), "twice", &opts(3), &mut br).unwrap();
        assert_eq!(cases.iter().map(|c| c.args[0]).collect::<Vec<_>>(), [20, 21, 22]);
        assert!(cases.iter().all(|c| c.differences.is_empty()), "{cases:?}");
        let Some((dir, b)) = built(&c_with("counter = a + 2;")) else { return };
        let saved = dir.join("cases");
        let opts =
            CheckOpts { cases: 2, fields: 10, steps: 10_000, save: Some(&saved), tag: "twice", body: None, varied: 0 };
        let cases = check(&mut game(), &n, &b.unwrap(), "twice", &opts, &mut br).unwrap();
        assert_eq!(cases[1].differences, ["byte 0x80020000 (counter): original 0x16, replacement 0x17"]);
        assert!(cases[1].saved.as_ref().unwrap().ends_with("twice-2.state"));
        assert!(cases[1].saved.as_ref().unwrap().is_file());
        let none = CheckOpts { cases: 1, fields: 1, steps: 10, save: None, tag: "nobody", body: None, varied: 0 };
        assert!(check(&mut game(), &n, &b_none(), "nobody", &none, &mut br).unwrap_err().contains("no function"));
        // Right for every argument the game passes, wrong for others: only
        // a varied call finds it.
        let Some((dir, b)) = built(&c_with("counter = a < 100 ? a + 1 : a;")) else { return };
        let b = b.unwrap();
        let saved = dir.join("cases");
        let plain =
            CheckOpts { cases: 3, fields: 10, steps: 10_000, save: Some(&saved), tag: "twice", body: None, varied: 0 };
        assert!(check(&mut game(), &n, &b, "twice", &plain, &mut br).unwrap().iter().all(|c| c.differences.is_empty()));
        let varied = CheckOpts { varied: 4, ..plain };
        let cases = check(&mut game(), &n, &b, "twice", &varied, &mut br).unwrap();
        assert_eq!(cases.len(), 15);
        assert_eq!(cases.iter().filter(|c| c.varied.is_none()).map(|c| c.args[0]).collect::<Vec<_>>(), [20, 21, 22]);
        let failed: Vec<_> = cases.iter().filter(|c| !c.differences.is_empty()).collect();
        assert!(!failed.is_empty() && failed.iter().all(|c| c.varied.is_some() && c.saved.is_some()), "{cases:?}");
        assert!(failed[0].saved.as_ref().unwrap().to_str().unwrap().contains("-varied-"));
    }

    #[test]
    fn a_callees_arguments_are_its_signatures_registers() {
        let (_d, n) = names(
            "function 0x80010000 int two(int a, int b)\nfunction 0x80010100 longlong wide(int a, longlong b)\n\
function 0x80010200 void none(void)\nfunction 0x80010300 unknown\nfunction 0x80010400 int many(int, int, int, int, int)\n",
        );
        assert_eq!(argument_slots(&n, 0x8001_0000, || None), Some(2));
        assert_eq!(argument_slots(&n, 0xA001_0000, || None), Some(2), "by the physical address");
        assert_eq!(argument_slots(&n, 0x8001_0100, || None), Some(4), "a 64-bit value takes an even pair");
        assert_eq!(argument_slots(&n, 0x8001_0200, || None), Some(0));
        assert_eq!(argument_slots(&n, 0x8001_0300, || None), None);
        assert_eq!(argument_slots(&n, 0x8001_0400, || None), Some(5));
        assert_eq!(argument_slots(&n, 0x8001_0500, || None), None);
        // A module's name is its signature too; two modules disagreeing
        // take the one whose code is there.
        let (_d, n) = names(
            "function 0x80020000 in RACE.OVL int one(int a)\nfunction 0x80030000 in RACE.OVL int one2(int a)\n\
function 0x80030000 in MENU.OVL int three(int a, int b, int c)\n",
        );
        assert_eq!(argument_slots(&n, 0x8002_0000, || None), Some(1));
        assert_eq!(argument_slots(&n, 0x8003_0000, || None), None, "unknown which is there");
        assert_eq!(argument_slots(&n, 0x8003_0000, || Some("three".into())), Some(3));
        assert_eq!(argument_slots(&n, 0x8003_0000, || Some("one2".into())), Some(1));
    }

    #[test]
    fn an_enum_argument_varies_within_its_values() {
        let (_d, n) = names("enum Slot 0x1 {\n  FIRST = 0\n  SECOND = 1\n  FIFTH = 4\n}\n");
        let mut rng = Rng(7);
        let ty = Type::Named { name: "Slot".into() };
        let seen: BTreeSet<u32> =
            (0..64).map(|_| rng.of(&n, &ty).unwrap()).inspect(|v| assert_eq!(v.0, 1)).map(|v| v.1).collect();
        assert_eq!(seen, BTreeSet::from([0, 1, 4]));
        let int = Type::Named { name: "int".into() };
        assert!((0..64).any(|_| rng.of(&n, &int).unwrap().1 > 4));
    }

    #[test]
    fn cases_spread_over_the_calls() {
        assert_eq!(spread((1..=10).collect(), 4), [1, 3, 6, 8]);
        assert_eq!(spread(vec![1, 2], 4), [1, 2]);
    }

    #[test]
    fn a_recording_reaches_what_its_coverage_executed() {
        let (dir, _) = names("");
        let rec = dir.join("walk.jsonl");
        assert!(reaches(&rec, 0x8002_817C), "no coverage kept: tried");
        let cov = r#"{"images": [{"executed": [["0x80028100", "0x80028180", 3]]}]}"#;
        std::fs::write(dir.join("walk.coverage.json"), cov).unwrap();
        assert!(reaches(&rec, 0x8002_817C));
        assert!(!reaches(&rec, 0x8002_8180));
    }

    fn b_none() -> Build {
        Build { image: Vec::new(), functions: BTreeMap::new() }
    }

    fn c_with(line: &str) -> String {
        format!("#include \"names.h\"\nint twice(int a) {{\n  {line}\n  helper(5);\n  return a * 2;\n}}\n")
    }
}

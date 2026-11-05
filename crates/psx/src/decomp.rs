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
fn declare(ty: &Type, decl: &str) -> String {
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
            "INCLUDE names.ld\nSECTIONS {{\n  . = 0x{BASE:08X};\n  .text : {{ *(.text*) }}\n  .rodata : {{ *(.rodata*) }}\n  .data : {{ *(.data*) *(.sdata*) }}\n  .bss : {{ *(.bss*) *(.sbss*) *(COMMON) }}\n  /DISCARD/ : {{ *(.MIPS.*) *(.reginfo) *(.comment) *(.pdr) *(.gnu.*) }}\n}}\n",
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
            (matches!(kind, "T" | "t") && a >= BASE).then(|| (name.to_string(), a))
        })
        .collect();
    Ok(Build { image, functions })
}

/// One call of the function the game made, checked.
#[derive(Debug, Clone)]
pub struct Case {
    /// Which of its calls (1: the first from the state).
    pub hit: u64,
    /// `a0`..`a3` at the call.
    pub args: [u32; 4],
    pub differences: Vec<String>,
    /// Where the state at the call was saved, when the case failed.
    pub saved: Option<PathBuf>,
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

/// `line` with each `byte 0xADDR` a global in `names` holds named.
fn named(names: &Names, line: &str) -> String {
    let Some(rest) = line.strip_prefix("byte 0x") else { return line.to_string() };
    let Some((hex, tail)) = rest.split_once(':') else { return line.to_string() };
    let Ok(addr) = u32::from_str_radix(hex, 16) else { return line.to_string() };
    match crate::names_bind::global_at(names, addr) {
        Some(g) => format!("byte 0x{hex} ({g}):{tail}"),
        None => line.to_string(),
    }
}

/// From `m`, runs the game until it calls `function` (the original's) and
/// checks each call, up to `cases` of them, against `build`'s function of
/// that name: the original and the build each run from the moment of the
/// call (at most `steps` steps), and their effects are compared. Calls the
/// build makes to its own functions count as calls to the originals of
/// the same names. Gives up waiting for a call after `fields` fields. A
/// failed case's state is saved in `save` when given.
#[allow(clippy::too_many_arguments)]
pub fn check(
    mut m: psx_core::Machine,
    names: &Names,
    build: &Build,
    function: &str,
    cases: usize,
    fields: u64,
    steps: u64,
    save: Option<&Path>,
) -> Result<Vec<Case>, String> {
    let original = entry(names, function)?;
    let bare = function.rsplit_once(':').map_or(function, |(_, n)| n);
    let replacement =
        *build.functions.get(bare).ok_or_else(|| format!("{bare:?}: decomp/ defines no function of that name"))?;
    // The build's own functions, as the originals they stand for.
    let aliases: BTreeMap<u32, u32> =
        build.functions.iter().filter_map(|(n, a)| entry(names, n).ok().map(|o| (*a, o))).collect();
    let returns = returns(names, bare);
    let limit = m.bus.frames + fields;
    let mut out = Vec::new();
    let at = |m: &psx_core::Machine| m.cpu.pc & 0x1FFF_FFFF == original & 0x1FFF_FFFF;
    while out.len() < cases && m.bus.frames < limit {
        if !at(&m) {
            m.step();
            continue;
        }
        let c = psx_core::callcheck::compare(&mut m, &build.image, replacement, returns, steps)?;
        let mut theirs = c.replacement.clone();
        for call in &mut theirs.calls {
            if let Some(o) = aliases.get(&call.0) {
                call.0 = *o;
            }
        }
        let differences: Vec<String> = psx_core::callcheck::differences(&c.original, &theirs, returns, steps)
            .iter()
            .map(|d| named(names, d))
            .collect();
        let hit = out.len() as u64 + 1;
        let saved = match (differences.is_empty(), save) {
            (false, Some(dir)) => {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                let p = dir.join(format!("{bare}-{hit}.state"));
                std::fs::write(&p, m.save_state(None)).map_err(|e| format!("{}: {e}", p.display()))?;
                Some(p)
            }
            _ => None,
        };
        let r = &m.cpu.regs;
        out.push(Case { hit, args: [r[4], r[5], r[6], r[7]], differences, saved });
        // On past this call's entry, to the next.
        m.step();
    }
    Ok(out)
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CodeCheckArgs {
    /// The function: its name in names/ (`[MODULE:]name`), and the name of its C in decomp/.
    pub function: String,
    /// The state to run from, named (`states list`) or a state file: the game's calls of the function from there are the cases.
    #[arg(long)]
    pub state: PathBuf,
    /// The BIOS (default: as run resolves it).
    #[arg(long)]
    #[serde(default)]
    pub bios: Option<PathBuf>,
    /// Calls to check.
    #[arg(long, default_value_t = 3)]
    #[serde(default = "d_cases")]
    pub cases: usize,
    /// Fields to wait for each call.
    #[arg(long, default_value_t = 600)]
    #[serde(default = "d_fields")]
    pub fields: u64,
}

fn d_cases() -> usize {
    3
}
fn d_fields() -> u64 {
    600
}

/// Steps one call may take before it counts as not returning.
const CALL_STEPS: u64 = 5_000_000;

fn messages_json(m: &[Message]) -> Value {
    json!(m.iter().map(|m| json!({"file": m.file, "line": m.line, "message": m.text})).collect::<Vec<_>>())
}

fn check_cmd(ctx: &Ctx, mut a: CodeCheckArgs) -> crate::Result<(bool, Value)> {
    let root = &ctx.root;
    let (states, _) = crate::states::Dirs::default().get(ctx);
    a.state = crate::run::named_state(&states, a.state);
    let mut sources: Vec<PathBuf> = std::fs::read_dir(root.join("decomp"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    sources.sort();
    if sources.is_empty() {
        return Err(Error::invalid("decomp/ holds no C: write the function's C as decomp/<name>.c"));
    }
    let names = crate::names_bind::project_names();
    let build = match build(names, &sources, &root.join(".psx/decomp")) {
        Ok(b) => b,
        Err(m) => {
            return Ok((
                false,
                json!({"function": a.function, "status": "build_failed", "messages": messages_json(&m)}),
            ));
        }
    };
    let resolved = crate::context::resolve(root, None, a.bios.clone(), Some(&a.state), false)?;
    if let Some(e) = resolved.setup_error() {
        return Err(e);
    }
    let bios_path = resolved.bios.unwrap();
    let bios = std::fs::read(&bios_path).map_err(|e| Error::io(&bios_path, e))?;
    let bytes = std::fs::read(&a.state).map_err(|e| Error::io(&a.state, e))?;
    let (_, mut m) = psx_core::Machine::load_state(&bytes, bios).map_err(|e| Error::new("invalid_state", e))?;
    m.bus.cdrom.disc = crate::run_native::open_media(resolved.media.as_deref())?.disc;
    let save = root.join(".psx/decomp/cases");
    let cases =
        check(m, names, &build, &a.function, a.cases, a.fields, CALL_STEPS, Some(&save)).map_err(Error::invalid)?;
    let failed = cases.iter().filter(|c| !c.differences.is_empty()).count();
    let ok = !cases.is_empty() && failed == 0;
    let mut v = json!({
        "function": a.function,
        "status": if cases.is_empty() { "no_calls" } else if ok { "passed" } else { "failed" },
        "passed": cases.len() - failed, "failed": failed,
        "cases": cases.iter().map(|c| {
            let mut j = json!({"hit": c.hit, "args": c.args.map(|a| format!("0x{a:08X}")), "passed": c.differences.is_empty()});
            if !c.differences.is_empty() {
                j["differences"] = json!(c.differences);
            }
            if let Some(p) = &c.saved {
                j["state"] = json!(p);
            }
            j
        }).collect::<Vec<_>>(),
    });
    if cases.is_empty() {
        v["why"] = json!(format!("the game did not call it within {} fields of the state", a.fields));
    }
    save_check(root, &a.function, &a.state, &v);
    Ok((ok, v))
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

/// Keep a check's result: the function's last, and a line in the log.
fn save_check(root: &Path, function: &str, state: &Path, v: &Value) {
    let commit = std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let first = v["cases"].as_array().and_then(|c| c.iter().find(|c| c["passed"] == false)).cloned();
    let kept = json!({
        "function": function, "status": v["status"], "passed": v["passed"], "failed": v["failed"],
        "at": crate::now_ms() as u64, "commit": commit, "sources": sources_hash(root),
        "state": state, "first_failure": first, "messages": v["messages"],
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
    let now = sources_hash(root);
    let read = |p: PathBuf| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let c: Vec<Value> = c_functions(root)
        .into_iter()
        .map(|(name, file, line)| {
            let mut v = json!({"name": name, "file": format!("decomp/{file}"), "line": line,
                "names": addr(&name), "off": off.get(&name)});
            if let Some(mut k) = read(check_file(root, &name)) {
                k["stale"] = json!(k["sources"].as_str() != Some(now.as_str()));
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
        "replaced_runs": Value::Null,
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
        let h = sources_hash(&root);
        std::fs::write(root.join("decomp/world.c"), format!("{c}\n")).unwrap();
        assert_ne!(h, sources_hash(&root), "a change to the C changes the hash");
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
        let cases = check(game(), &n, &b.unwrap(), "twice", 3, 10, 10_000, Some(&dir.join("cases"))).unwrap();
        assert_eq!(cases.iter().map(|c| c.args[0]).collect::<Vec<_>>(), [20, 21, 22]);
        assert!(cases.iter().all(|c| c.differences.is_empty()), "{cases:?}");
        let Some((dir, b)) = built(&c_with("counter = a + 2;")) else { return };
        let cases = check(game(), &n, &b.unwrap(), "twice", 2, 10, 10_000, Some(&dir.join("cases"))).unwrap();
        assert_eq!(cases[1].differences, ["byte 0x80020000 (counter): original 0x16, replacement 0x17"]);
        assert!(cases[1].saved.as_ref().unwrap().is_file());
        assert!(check(game(), &n, &b_none(), "nobody", 1, 1, 10, None).unwrap_err().contains("no function"));
    }

    fn b_none() -> Build {
        Build { image: Vec::new(), functions: BTreeMap::new() }
    }

    fn c_with(line: &str) -> String {
        format!("#include \"names.h\"\nint twice(int a) {{\n  {line}\n  helper(5);\n  return a * 2;\n}}\n")
    }
}

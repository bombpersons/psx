//! Debug files a game's build may have left on its disc: SN Systems' SYM
//! (`MND`), psylink's MAP and ELF, found by their content, read into names,
//! and written as a names file (see [`crate::names`]).
//!
//! The SYM records are read as SN's DUMPSYM prints them: an 8-byte header
//! (`MND`, version, unit, 3 bytes), then records of a u32 value and a tag
//! byte. Its definitions carry the same fields as the definitions in
//! PsyQ's OBJ files (ghidra_psx_ldr's `Definition`/`Definition2`: class,
//! type and size, then the dimensions and the tag of `def2`), with COFF's
//! classes and type encoding.

use std::collections::BTreeMap;

/// A debug file's signature found in some bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// `sym`, `elf`, `map` or `cpe`.
    pub kind: &'static str,
    pub offset: usize,
}

/// Finds debug files' headers anywhere in `bytes`: SYM's `MND` with a
/// version of 1 or 2 and zero padding, an ELF header for MIPS, psylink's
/// map headings, and CPE's `CPE\x01`.
pub fn scan(bytes: &[u8]) -> Vec<Hit> {
    let mut out = Vec::new();
    let find = |needle: &[u8], f: &mut dyn FnMut(usize)| {
        if bytes.len() < needle.len() {
            return;
        }
        let mut i = 0;
        while i + needle.len() <= bytes.len() {
            match bytes[i..].windows(needle.len()).position(|w| w == needle) {
                Some(p) => {
                    f(i + p);
                    i += p + 1;
                }
                None => break,
            }
        }
    };
    find(b"MND", &mut |p| {
        let h = &bytes[p..];
        if h.len() >= 16 && matches!(h[3], 1 | 2) && h[5..8] == [0, 0, 0] {
            out.push(Hit { kind: "sym", offset: p });
        }
    });
    find(b"\x7FELF", &mut |p| {
        let h = &bytes[p..];
        // 32-bit, little-endian, MIPS (machine 8).
        if h.len() >= 52 && h[4] == 1 && h[5] == 1 && u16::from_le_bytes([h[18], h[19]]) == 8 {
            out.push(Hit { kind: "elf", offset: p });
        }
    });
    for heading in [&b"Names in address order"[..], b"Names alphabetically", b"Start     Stop      Length"] {
        find(heading, &mut |p| {
            if !out.iter().any(|h| h.kind == "map" && p.abs_diff(h.offset) < 1 << 20) {
                out.push(Hit { kind: "map", offset: p });
            }
        });
    }
    find(b"CPE\x01", &mut |p| {
        if p % 4 == 0 {
            out.push(Hit { kind: "cpe", offset: p });
        }
    });
    out.sort_by_key(|h| h.offset);
    out
}

/// Strings in `bytes` that look like source paths (`C:\PSX\GAME\MAIN.C`,
/// `src/obj.c`) and GCC's `GCC: (GNU) …` ident strings, each once.
pub fn strings(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut paths = std::collections::BTreeSet::new();
    let mut gcc = std::collections::BTreeSet::new();
    let mut start = 0;
    for i in 0..=bytes.len() {
        let printable = i < bytes.len() && (0x20..0x7F).contains(&bytes[i]);
        if printable {
            continue;
        }
        if i - start >= 5 {
            let s = String::from_utf8_lossy(&bytes[start..i]).into_owned();
            if let Some(p) = s.find("GCC: ") {
                gcc.insert(s[p..].trim().to_string());
            }
            for word in s.split([' ', '"', '\'', '(', ')', ',']) {
                if source_path(word) {
                    paths.insert(word.to_string());
                }
            }
        }
        start = i + 1;
    }
    (paths.into_iter().collect(), gcc.into_iter().collect())
}

fn source_path(w: &str) -> bool {
    let lower = w.to_ascii_lowercase();
    let Some((stem, ext)) = lower.rsplit_once('.') else { return false };
    if !["c", "h", "s", "cpp", "asm", "obj", "inc"].contains(&ext)
        || !stem.chars().all(|c| c.is_ascii_alphanumeric() || "\\/:_-.".contains(c))
    {
        return false;
    }
    let mut parts: Vec<&str> = stem.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let Some(file) = parts.pop() else { return false };
    // A file of three or more characters starting with a letter, in a
    // directory that reads as one (a drive, `..`, or a name with letters).
    file.len() >= 3
        && file.starts_with(|c: char| c.is_ascii_alphabetic())
        && !file.contains('.')
        && !parts.is_empty()
        && parts.iter().all(|d| {
            *d == ".."
                || *d == "."
                || (d.len() == 2 && d.ends_with(':'))
                || (d.len() >= 3 && d.chars().filter(|c| c.is_ascii_alphabetic()).count() * 2 >= d.len())
        })
}

/// Whether a file name is one debug files or sources go by.
pub fn debug_name(name: &str) -> Option<&'static str> {
    let ext = name.split(';').next().unwrap_or(name).rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "sym" => "sym",
        "map" => "map",
        "elf" => "elf",
        "cpe" => "cpe",
        "c" | "h" | "cpp" | "s" | "asm" | "inc" => "source",
        // PsyQ's objects and libraries, when their content says so.
        "obj" | "lib" => "object",
        _ => return None,
    })
}

/// A local of a function in a debug file.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugLocal {
    /// `sp+0x10`, or a register's name.
    pub slot: String,
    pub ty: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DebugFunction {
    pub address: u32,
    pub name: String,
    /// The return type, when the file gives one.
    pub ret: Option<String>,
    pub params: Vec<(String, String)>,
    pub locals: Vec<DebugLocal>,
    /// The source file and line it starts at.
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DebugStruct {
    pub name: String,
    pub size: u32,
    pub union: bool,
    /// (offset, type, name)
    pub fields: Vec<(u32, String, String)>,
}

/// What a debug file names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DebugNames {
    pub functions: Vec<DebugFunction>,
    /// (address, type, name); the type when the file gives one.
    pub globals: Vec<(u32, Option<String>, String)>,
    pub types: Vec<DebugStruct>,
    /// Source files named, with lines.
    pub sources: Vec<String>,
}

const REGS: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra",
];

/// A name as the names files take it: letters, digits and `_`, not
/// starting with a digit.
pub fn ident(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.b.get(self.i..self.i + n).ok_or_else(|| format!("ends inside a record at 0x{:X}", self.i))?;
        self.i += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn name(&mut self) -> Result<String, String> {
        let n = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// COFF's type: the base in the low 4 bits, then derivations 2 bits each
/// from bit 4, the first nearest the name (1 pointer, 2 function, 3 array).
fn coff_type(ty: u16, tag: &str, dims: &[u32], size: u32) -> (String, String) {
    let base = match ty & 0xF {
        0 | 1 => "void".to_string(),
        2 => "char".into(),
        3 => "short".into(),
        4 => "int".into(),
        5 => "long".into(),
        6 => "float".into(),
        7 => "double".into(),
        8 => format!("struct {}", ident(tag)),
        9 => format!("union {}", ident(tag)),
        10 => format!("enum {}", ident(tag)),
        12 => "uchar".into(),
        13 => "ushort".into(),
        14 => "uint".into(),
        15 => "ulong".into(),
        _ => "int".into(),
    };
    let mut derived = Vec::new();
    let mut t = ty >> 4;
    while t != 0 {
        derived.push(t & 3);
        t >>= 2;
    }
    // Arrays nearest the name come first, with pointers inside them; any
    // other order (a pointer to an array, a function) is a code pointer or
    // left at its size.
    let arrays = derived.iter().take_while(|&&d| d == 3).count();
    let rest = &derived[arrays..];
    let suffix: String = dims.iter().take(arrays).map(|d| format!("[{d}]")).collect();
    let ty = if rest.contains(&2) {
        match rest.iter().position(|&d| d == 2) {
            Some(0) => base,
            _ => "code *".into(),
        }
    } else if rest.contains(&3) {
        match size {
            1 => "u8".into(),
            2 => "u16".into(),
            _ => "undefined4".into(),
        }
    } else {
        format!("{base}{}", if rest.is_empty() { String::new() } else { format!(" {}", "*".repeat(rest.len())) })
    };
    (ty, suffix)
}

/// Reads an SN SYM file.
pub fn parse_sym(bytes: &[u8]) -> Result<DebugNames, String> {
    if bytes.len() < 8 || &bytes[..3] != b"MND" {
        return Err("not an SN SYM file (no MND header)".into());
    }
    let mut r = Reader { b: bytes, i: 8 };
    let mut out = DebugNames::default();
    let mut symbols: Vec<(u32, String)> = Vec::new();
    let mut defs: BTreeMap<u32, (String, String)> = BTreeMap::new();
    let mut current: Option<DebugFunction> = None;
    let mut fp = 29u16;
    let mut open_struct: Option<DebugStruct> = None;
    let mut sources = std::collections::BTreeSet::new();
    while r.i < bytes.len() {
        let value = r.u32()?;
        let tag = r.u8()?;
        match tag {
            1 | 2 => symbols.push((value, r.name()?)),
            0x80 | 0x8A | 0x9A => {}
            0x82 => {
                r.u8()?;
            }
            0x84 => {
                r.u16()?;
            }
            0x86 | 0x8E | 0x90 | 0x92 => {
                r.u32()?;
                if tag == 0x8E {
                    out.functions.extend(current.take());
                }
            }
            0x88 => {
                r.u32()?;
                sources.insert(r.name()?);
            }
            0x8C => {
                fp = r.u16()?;
                let _frame = r.u32()?;
                let _ret = r.u16()?;
                let _mask = r.u32()?;
                let _mask_offset = r.u32()?;
                let line = r.u32()?;
                let file = r.name()?;
                let name = r.name()?;
                sources.insert(file.clone());
                out.functions.extend(current.take());
                current = Some(DebugFunction {
                    address: value,
                    name,
                    file: Some(file),
                    line: Some(line),
                    ..Default::default()
                });
            }
            0x94 | 0x96 => {
                let class = r.u16()?;
                let ty = r.u16()?;
                let size = r.u32()?;
                let (dims, tag_name) = if tag == 0x96 {
                    let n = r.u16()?;
                    let dims = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>, _>>()?;
                    (dims, r.name()?)
                } else {
                    (Vec::new(), String::new())
                };
                let name = r.name()?;
                let (t, suffix) = coff_type(ty, &tag_name, &dims, size);
                let decl = (t.clone(), format!("{}{suffix}", ident(&name)));
                match class {
                    // A struct's or union's tag, its members, its end.
                    10 | 12 => {
                        open_struct = Some(DebugStruct { name: ident(&name), size, union: class == 12, fields: vec![] })
                    }
                    8 | 11 => {
                        if let Some(s) = open_struct.as_mut() {
                            s.fields.push((value, decl.0, decl.1));
                        }
                    }
                    102 => out.types.extend(open_struct.take()),
                    // Automatic, register, argument, register argument.
                    1 | 4 | 9 | 17 if current.is_some() => {
                        let f = current.as_mut().unwrap();
                        let slot = if class == 4 || class == 17 {
                            REGS.get(value as usize).copied().unwrap_or("zero").to_string()
                        } else if fp == 29 {
                            format!("sp+0x{value:X}")
                        } else {
                            // Offsets from a frame pointer have no slot here.
                            continue;
                        };
                        if class == 9 || class == 17 {
                            f.params.push((decl.0, decl.1));
                        } else {
                            f.locals.push(DebugLocal { slot, ty: decl.0, name: decl.1 });
                        }
                    }
                    // Extern and static: a global, or a function's return.
                    2 | 3 => {
                        let is_function = (ty >> 4) & 3 == 2;
                        if is_function {
                            if let Some(f) = current.as_mut().filter(|f| f.address == value) {
                                f.ret = Some(t);
                            } else {
                                defs.insert(value, (format!("fn:{t}"), name));
                            }
                        } else {
                            defs.insert(value, (format!("{}{suffix}", t), name));
                        }
                    }
                    _ => {}
                }
            }
            0x98 => {
                r.u32()?;
                r.u32()?;
            }
            other => return Err(format!("unknown SYM record 0x{other:02X} at 0x{:X}", r.i - 1)),
        }
    }
    out.functions.extend(current.take());
    // Functions without line records, and globals, come from the symbols.
    let mut named: std::collections::HashSet<u32> = out.functions.iter().map(|f| f.address).collect();
    for (address, name) in symbols {
        if named.contains(&address) {
            continue;
        }
        named.insert(address);
        match defs.get(&address) {
            Some((t, _)) if t.starts_with("fn:") => {
                out.functions.push(DebugFunction { address, name, ret: Some(t[3..].into()), ..Default::default() })
            }
            Some((t, n)) if *n == name && !t.contains('[') => out.globals.push((address, Some(t.clone()), name)),
            _ => out.globals.push((address, None, name)),
        }
    }
    for f in &mut out.functions {
        if f.ret.is_none() {
            if let Some((t, _)) = defs.get(&f.address).filter(|(t, _)| t.starts_with("fn:")) {
                f.ret = Some(t[3..].into());
            }
        }
    }
    out.sources = sources.into_iter().collect();
    Ok(out)
}

/// Reads psylink's map: every `ADDRESS name` line (the names in address
/// order and alphabetically list the same names).
pub fn parse_map(text: &str) -> DebugNames {
    let mut seen = BTreeMap::new();
    for line in text.lines() {
        let mut w = line.split_whitespace();
        let (Some(a), Some(n), None) = (w.next(), w.next(), w.next()) else { continue };
        let Ok(address) = u32::from_str_radix(a, 16) else { continue };
        if a.len() == 8 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !n.starts_with(|c: char| c.is_ascii_digit()) {
            seen.entry(address).or_insert_with(|| n.to_string());
        }
    }
    DebugNames { globals: seen.into_iter().map(|(a, n)| (a, None, n)).collect(), ..Default::default() }
}

/// Reads an ELF's symbol table: functions and objects with their names.
/// Its DWARF is not read.
pub fn parse_elf(b: &[u8]) -> Result<DebugNames, String> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or("cut short");
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or("cut short");
    if b.len() < 52 || &b[..4] != b"\x7FELF" || b[4] != 1 || b[5] != 1 {
        return Err("not a 32-bit little-endian ELF".into());
    }
    let shoff = u32_at(32)? as usize;
    let shentsize = u16_at(46)? as usize;
    let shnum = u16_at(48)? as usize;
    let section = |i: usize| -> Result<(u32, usize, usize, u32), &str> {
        let o = shoff + i * shentsize;
        Ok((u32_at(o + 4)?, u32_at(o + 16)? as usize, u32_at(o + 20)? as usize, u32_at(o + 24)?))
    };
    let mut out = DebugNames::default();
    for i in 0..shnum {
        let (kind, off, size, link) = section(i)?;
        if kind != 2 {
            continue;
        }
        let (_, stroff, strsize, _) = section(link as usize)?;
        let strtab = b.get(stroff..stroff + strsize).ok_or("string table past the end")?;
        for s in (off..off + size).step_by(16) {
            let name_at = u32_at(s)? as usize;
            let value = u32_at(s + 4)?;
            let info = *b.get(s + 12).ok_or("cut short")?;
            let name = strtab.get(name_at..).and_then(|t| t.split(|&c| c == 0).next()).unwrap_or_default();
            let name = String::from_utf8_lossy(name).into_owned();
            if name.is_empty() || value == 0 {
                continue;
            }
            match info & 0xF {
                2 => out.functions.push(DebugFunction { address: value, name, ..Default::default() }),
                1 => out.globals.push((value, None, name)),
                _ => {}
            }
        }
    }
    Ok(out)
}

impl DebugNames {
    /// Moves globals that start a function to the functions (MAP and SYM
    /// symbols do not say which names are code).
    pub fn split_functions(&mut self, is_start: impl Fn(u32) -> bool) {
        let (code, data): (Vec<_>, Vec<_>) = std::mem::take(&mut self.globals)
            .into_iter()
            .partition(|(a, t, _)| t.is_none() && is_start(*a));
        self.globals = data;
        for (address, _, name) in code {
            self.functions.push(DebugFunction { address, name, ..Default::default() });
        }
        self.functions.sort_by_key(|f| f.address);
    }

    /// How many functions start at a function start, of those whose
    /// address is in `range`.
    pub fn matching(&self, range: std::ops::Range<u32>, is_start: impl Fn(u32) -> bool) -> (usize, usize) {
        let inside: Vec<u32> = self.functions.iter().map(|f| f.address).filter(|a| range.contains(a)).collect();
        (inside.iter().filter(|&&a| is_start(a)).count(), inside.len())
    }

    /// The names file for these names, `module` (`in MODULE`) for an
    /// overlay; `skip` leaves out functions and globals already named.
    pub fn to_names(&self, header: &str, module: Option<&str>, skip: &dyn Fn(u32) -> bool) -> String {
        let mut out = String::new();
        for l in header.lines() {
            out.push_str(&format!("// {l}\n"));
        }
        let m = module.map(|m| format!(" in {m}")).unwrap_or_default();
        for s in &self.types {
            out.push_str(&format!("\n{} {} 0x{:X} {{\n", if s.union { "union" } else { "struct" }, s.name, s.size));
            for (o, t, n) in &s.fields {
                out.push_str(&format!("  0x{o:03X} {} {n}\n", t.trim_end()));
            }
            out.push_str("}\n");
        }
        out.push('\n');
        for (a, t, n) in &self.globals {
            if skip(*a) {
                continue;
            }
            match t {
                Some(t) => {
                    let (t, dims) = t.split_once('[').map(|(t, d)| (t, format!("[{d}"))).unwrap_or((t, String::new()));
                    out.push_str(&format!("global 0x{a:08X}{m} {} {}{dims}\n", t.trim_end(), ident(n)))
                }
                None => out.push_str(&format!("global 0x{a:08X}{m} {}\n", ident(n))),
            }
        }
        for f in &self.functions {
            if skip(f.address) {
                continue;
            }
            let at = match (&f.file, f.line) {
                (Some(file), Some(line)) => format!("  // {file}:{line}"),
                _ => String::new(),
            };
            match &f.ret {
                Some(ret) => {
                    let params: Vec<String> = f.params.iter().map(|(t, n)| format!("{} {n}", t.trim_end())).collect();
                    let params = if params.is_empty() { "void".into() } else { params.join(", ") };
                    out.push_str(&format!("function 0x{:08X}{m} {} {}({params}){at}\n", f.address, ret.trim_end(), ident(&f.name)));
                }
                None => out.push_str(&format!("function 0x{:08X}{m} {}{at}\n", f.address, ident(&f.name))),
            }
            for l in &f.locals {
                out.push_str(&format!("  {} {} {}\n", l.slot, l.ty.trim_end(), l.name));
            }
        }
        out
    }
}

/// Debug files for tests.
#[doc(hidden)]
pub mod testing {
    /// A SYM file with one struct, one function with a local and a
    /// parameter, and a global.
    pub fn planted_sym(function: u32) -> Vec<u8> {
        let mut b = b"MND\x01\x00\x00\x00\x00".to_vec();
        let name = |b: &mut Vec<u8>, s: &str| {
            b.push(s.len() as u8);
            b.extend_from_slice(s.as_bytes());
        };
        let rec = |b: &mut Vec<u8>, value: u32, tag: u8| {
            b.extend_from_slice(&value.to_le_bytes());
            b.push(tag);
        };
        let def2 = |b: &mut Vec<u8>, value: u32, class: u16, ty: u16, size: u32, dims: &[u32], tag: &str, n: &str| {
            rec(b, value, 0x96);
            b.extend_from_slice(&class.to_le_bytes());
            b.extend_from_slice(&ty.to_le_bytes());
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(&(dims.len() as u16).to_le_bytes());
            for d in dims {
                b.extend_from_slice(&d.to_le_bytes());
            }
            name(b, tag);
            name(b, n);
        };
        rec(&mut b, function, 1);
        name(&mut b, "world_collide");
        rec(&mut b, 0x8007_528C, 1);
        name(&mut b, "croc");
        // struct Object { int floor_height at 0x38; short pos[3] at 0x370 }
        def2(&mut b, 0, 10, 8, 0x988, &[], "Object", "Object");
        def2(&mut b, 0x38, 8, 4, 4, &[], "", "floor_height");
        def2(&mut b, 0x370, 8, 3 | (3 << 4), 6, &[3], "", "pos");
        def2(&mut b, 0x988, 102, 0, 0x988, &[], "", ".eos");
        // Object *croc
        def2(&mut b, 0x8007_528C, 2, 8 | (1 << 4), 4, &[], "Object", "croc");
        // int world_collide(Object *obj)
        rec(&mut b, function, 0x8C);
        b.extend_from_slice(&29u16.to_le_bytes());
        b.extend_from_slice(&0x30u32.to_le_bytes());
        b.extend_from_slice(&31u16.to_le_bytes());
        b.extend_from_slice(&0x8003_0000u32.to_le_bytes());
        b.extend_from_slice(&(-4i32).to_le_bytes());
        b.extend_from_slice(&120u32.to_le_bytes());
        name(&mut b, "C:\\CROC\\COLLIDE.C");
        name(&mut b, "world_collide");
        def2(&mut b, function, 2, 4 | (2 << 4), 0, &[], "", "world_collide");
        def2(&mut b, 4, 17, 8 | (1 << 4), 4, &[], "Object", "obj");
        def2(&mut b, 0x10, 1, 4, 4, &[], "", "i");
        rec(&mut b, function + 0x40, 0x8E);
        b.extend_from_slice(&150u32.to_le_bytes());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::testing::planted_sym;

    #[test]
    fn a_sym_file_reads_into_names_that_parse() {
        let d = parse_sym(&planted_sym(0x8003_1F64)).unwrap();
        assert_eq!(d.functions.len(), 1);
        let f = &d.functions[0];
        assert_eq!((f.address, f.name.as_str(), f.ret.as_deref()), (0x8003_1F64, "world_collide", Some("int")));
        assert_eq!(f.params, vec![("struct Object *".to_string(), "obj".to_string())]);
        assert_eq!(f.locals[0].slot, "sp+0x10");
        assert_eq!(d.globals, vec![(0x8007_528C, Some("struct Object *".into()), "croc".into())]);
        assert_eq!(d.types[0].fields[1], (0x370, "short".into(), "pos[3]".into()));
        let text = d.to_names("from TEST.SYM", None, &|_| false);
        let names = crate::names::from_texts(&[], &[(".psx/names/TEST.SYM.names".into(), text.clone())]);
        assert!(names.problems.is_empty(), "{text}\n{:?}", names.problems);
        assert_eq!(names.functions[0].name.text, "world_collide");
        assert!(text.contains("COLLIDE.C:120"), "{text}");
    }

    #[test]
    fn signatures_are_found_inside_other_bytes() {
        let mut b = vec![0u8; 100];
        b.extend(planted_sym(0x8001_0000));
        b.extend_from_slice(b"\0\0  Address  Names in address order\r\n  80010000 main\r\n");
        let hits = scan(&b);
        assert_eq!(hits.iter().map(|h| (h.kind, h.offset)).collect::<Vec<_>>()[0], ("sym", 100));
        assert!(hits.iter().any(|h| h.kind == "map"));
        let (paths, gcc) = strings(b"xx\0C:\\CROC\\COLLIDE.C\0GCC: (GNU) 2.7.2-SN\0readme.txt\0/m2.c\0E/u.C\0.C9Q_\\L.C\0ky\\w8O.s\0ydd/t0..H\0C:\\PSX\\lib.47\\include\\libgpu.h\0../game/TwoGun.c\0");
        assert_eq!(paths, ["../game/TwoGun.c", "C:\\CROC\\COLLIDE.C", "C:\\PSX\\lib.47\\include\\libgpu.h"]);
        assert_eq!(gcc, ["GCC: (GNU) 2.7.2-SN"]);
    }

    #[test]
    fn a_map_names_what_starts_a_function() {
        let mut d = parse_map("  Address  Names in address order\n  80010000 main\n  80074B38 _gp\n  junk line\n");
        d.split_functions(|a| a == 0x8001_0000);
        assert_eq!(d.functions[0].name, "main");
        assert_eq!(d.globals, vec![(0x8007_4B38, None, "_gp".into())]);
        assert_eq!(d.matching(0x8001_0000..0x8002_0000, |a| a == 0x8001_0000), (1, 1));
    }
}

//! Names files: what an agent (or a debug file on the disc) knows about the
//! original's code, one fact per line, in a syntax close to C.
//!
//! ```text
//! // Croc's object
//! struct Object 0x988 {
//!   0x038 int floor_height        // out of world_collide
//!   0x040 ProbeOffset probe_offset[6]
//! }
//! typedef u16 Angle
//! enum Gait { WALK = 0  RUN = 1 }   (one value per line inside braces)
//! global 0x8007528C Object *croc
//! global 0x80074D08 int speed_x?    // a trailing ? marks a guess
//! function 0x80031F64 void world_collide(Object *obj, int unk, Area *area)
//!   local_150 Object *+0x38 obj_38  // a pointer 0x38 into an Object
//!   s0 int i
//! function 0x80100000 in RACE.OVL race_update
//! ```
//!
//! A project's names are every `*.names` file under `names/` (the agent's)
//! and `.psx/names/` (imported from the disc). [`load`] reads them into one
//! [`Names`] and reports every problem with its file and line; nothing is
//! fatal, so what parses is still used. Where the agent and an import name
//! the same thing the agent's wins.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Serialize;

/// The directory of the agent's names, under the project root.
pub const AGENT_DIR: &str = "names";
/// The directory of names imported from the disc, under the project root.
pub const IMPORTED_DIR: &str = ".psx/names";

/// Where something was written: a file relative to the project root, and a
/// 1-based line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct At {
    pub file: String,
    pub line: u32,
}

impl std::fmt::Display for At {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub at: At,
    pub message: String,
}

/// A name, and whether it was marked as a guess (`name?`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Name {
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub guess: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Type {
    /// A base type (`int`, `u16`), or a struct, union, enum or typedef by name.
    Named {
        name: String,
    },
    /// A pointer; `offset` is non-zero for a pointer into the middle of the
    /// pointee (`Object *+0x38`), as the compiler keeps `&obj->field`.
    Pointer {
        to: Box<Type>,
        #[serde(skip_serializing_if = "is_zero")]
        offset: u32,
    },
    Array {
        of: Box<Type>,
        len: u32,
    },
    Function(Signature),
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Signature {
    pub ret: Box<Type>,
    pub params: Vec<Param>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub varargs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Param {
    #[serde(rename = "type")]
    pub ty: Type,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<Name>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Field {
    pub offset: u32,
    #[serde(rename = "type")]
    pub ty: Type,
    pub name: Name,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub at: At,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EnumValue {
    pub name: Name,
    pub value: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub at: At,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypeBody {
    Struct {
        fields: Vec<Field>,
    },
    Union {
        fields: Vec<Field>,
    },
    Enum {
        values: Vec<EnumValue>,
    },
    Typedef {
        #[serde(rename = "type")]
        ty: Type,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TypeDef {
    pub name: Name,
    /// The size written in the file; for a struct or union without one, its
    /// last field's end ([`Names::size_of`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    pub body: TypeBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Where it was declared first; a struct may be continued elsewhere.
    pub at: At,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Global {
    #[serde(with = "psx_media::memmap::addr")]
    pub address: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// None for a label alone (`global 0x8007528C croc`).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<Type>,
    pub name: Name,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub at: At,
}

/// Where a local lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub enum Slot {
    /// Relative to `sp` at the function's entry, as Ghidra names locals
    /// (`local_230` is -0x230).
    Stack(i32),
    /// Relative to `sp` after the prologue (`sp+0x10`), as debug files give them.
    Frame(i32),
    Register(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Local {
    pub slot: Slot,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<Type>,
    pub name: Name,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub at: At,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Function {
    #[serde(with = "psx_media::memmap::addr")]
    pub address: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub name: Name,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<Local>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub at: At,
}

/// Every name of a project, checked.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Names {
    pub types: Vec<TypeDef>,
    pub globals: Vec<Global>,
    pub functions: Vec<Function>,
    pub problems: Vec<Problem>,
}

/// Base types: their size, and what they are.
pub const BASE_TYPES: &[(&str, u32)] = &[
    ("void", 0),
    ("char", 1),
    ("i8", 1),
    ("u8", 1),
    ("s8", 1),
    ("byte", 1),
    ("uchar", 1),
    ("u_char", 1),
    ("bool", 1),
    ("i16", 2),
    ("u16", 2),
    ("s16", 2),
    ("short", 2),
    ("ushort", 2),
    ("u_short", 2),
    ("i32", 4),
    ("u32", 4),
    ("s32", 4),
    ("int", 4),
    ("uint", 4),
    ("long", 4),
    ("ulong", 4),
    ("u_long", 4),
    ("u_int", 4),
    ("float", 4),
    ("i64", 8),
    ("u64", 8),
    ("s64", 8),
    ("longlong", 8),
    ("double", 8),
    ("undefined", 1),
    ("undefined1", 1),
    ("undefined2", 2),
    ("undefined4", 4),
    ("undefined8", 8),
    // A function's code, for `code *` (any function pointer).
    ("code", 1),
];

pub fn base_size(name: &str) -> Option<u32> {
    BASE_TYPES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

const REGISTERS: &[&str] = &[
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "s8", "ra",
];

/// Reads `names/` and `.psx/names/` under `root` and checks them together.
/// The names files under `root` (names/ and .psx/names/), as `load` reads them.
pub fn files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for dir in [AGENT_DIR, IMPORTED_DIR] {
        let mut more = Vec::new();
        walk(&root.join(dir), &mut more);
        more.sort();
        files.extend(more);
    }
    files
}

pub fn load(root: &Path) -> Names {
    let mut agent = Vec::new();
    let mut imported = Vec::new();
    let mut unread = Vec::new();
    for (dir, into) in [(AGENT_DIR, &mut agent), (IMPORTED_DIR, &mut imported)] {
        let mut files = Vec::new();
        walk(&root.join(dir), &mut files);
        files.sort();
        for path in files {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            match std::fs::read_to_string(&path) {
                Ok(text) => into.push((rel, text)),
                Err(e) => unread.push(Problem { at: At { file: rel, line: 1 }, message: format!("unreadable: {e}") }),
            }
        }
    }
    let mut names = from_texts(&agent, &imported);
    names.problems.splice(0..0, unread);
    names
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        // Not through symlinked folders: a link back up would recurse forever.
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "names") {
            out.push(p);
        }
    }
}

/// Parses and checks files given as (path, text): the agent's, then the
/// imported ones, whose names give way to the agent's.
pub fn from_texts(agent: &[(String, String)], imported: &[(String, String)]) -> Names {
    let mut problems = Vec::new();
    let mut parse_all = |files: &[(String, String)]| {
        let mut items = Vec::new();
        for (file, text) in files {
            let (i, p) = parse(file, text);
            items.extend(i);
            problems.extend(p);
        }
        items
    };
    let agent = parse_all(agent);
    let imported = parse_all(imported);
    let mut names = merge(agent, imported, &mut problems);
    names.check(&mut problems);
    problems.sort_by(|a, b| (&a.at.file, a.at.line).cmp(&(&b.at.file, b.at.line)));
    names.problems = problems;
    names
}

/// One declaration as written; structs may come in pieces.
#[derive(Debug)]
enum Item {
    Type(TypeDef),
    Global(Global),
    Function(Function),
}

/// Combines the parsed items: pieces of one struct join, the agent's names
/// win over imported ones, and the agent naming one thing twice is reported.
fn merge(agent: Vec<Item>, imported: Vec<Item>, problems: &mut Vec<Problem>) -> Names {
    let mut types: Vec<TypeDef> = Vec::new();
    let mut type_ix: HashMap<String, (usize, bool)> = HashMap::new();
    let mut globals: Vec<Global> = Vec::new();
    let mut global_ix: HashMap<(Option<String>, u32), (usize, bool)> = HashMap::new();
    let mut functions: Vec<Function> = Vec::new();
    let mut function_ix: HashMap<(Option<String>, u32), (usize, bool)> = HashMap::new();
    for (items, from_agent) in [(agent, true), (imported, false)] {
        for item in items {
            match item {
                Item::Type(t) => match type_ix.get(&t.name.text).copied() {
                    None => {
                        type_ix.insert(t.name.text.clone(), (types.len(), from_agent));
                        types.push(t);
                    }
                    Some((_, agent_had)) if agent_had && !from_agent => {}
                    Some((i, _)) => join_type(&mut types[i], t, problems),
                },
                Item::Global(g) => {
                    let key = (g.module.clone(), g.address);
                    match global_ix.get(&key).copied() {
                        None => {
                            global_ix.insert(key, (globals.len(), from_agent));
                            globals.push(g);
                        }
                        Some((_, agent_had)) if agent_had && !from_agent => {}
                        Some((i, _)) => problems.push(Problem {
                            message: format!(
                                "{} is already named {} at {}",
                                psx_media::memmap::hex(g.address),
                                globals[i].name.text,
                                globals[i].at
                            ),
                            at: g.at,
                        }),
                    }
                }
                Item::Function(f) => {
                    let key = (f.module.clone(), f.address);
                    match function_ix.get(&key).copied() {
                        None => {
                            function_ix.insert(key, (functions.len(), from_agent));
                            functions.push(f);
                        }
                        Some((_, agent_had)) if agent_had && !from_agent => {}
                        Some((i, _)) => problems.push(Problem {
                            message: format!(
                                "function {} is already named {} at {}",
                                psx_media::memmap::hex(f.address),
                                functions[i].name.text,
                                functions[i].at
                            ),
                            at: f.at,
                        }),
                    }
                }
            }
        }
    }
    Names { types, globals, functions, problems: Vec::new() }
}

/// A struct or union continued in another place: its fields join the first
/// declaration's. Anything else declared twice is a problem.
fn join_type(first: &mut TypeDef, more: TypeDef, problems: &mut Vec<Problem>) {
    let same_kind = matches!(
        (&first.body, &more.body),
        (TypeBody::Struct { .. }, TypeBody::Struct { .. }) | (TypeBody::Union { .. }, TypeBody::Union { .. })
    );
    if !same_kind {
        problems
            .push(Problem { message: format!("{} is already declared at {}", more.name.text, first.at), at: more.at });
        return;
    }
    match (first.size, more.size) {
        (Some(a), Some(b)) if a != b => problems.push(Problem {
            message: format!("{} has size 0x{a:X} at {}, not 0x{b:X}", more.name.text, first.at),
            at: more.at.clone(),
        }),
        (None, Some(b)) => first.size = Some(b),
        _ => {}
    }
    if first.comment.is_none() {
        first.comment = more.comment;
    }
    let (TypeBody::Struct { fields } | TypeBody::Union { fields }) = &mut first.body else { unreachable!() };
    let (TypeBody::Struct { fields: extra } | TypeBody::Union { fields: extra }) = more.body else { unreachable!() };
    fields.extend(extra);
}

impl Names {
    pub fn type_named(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.name.text == name)
    }

    /// A type's size in bytes, when it is known.
    pub fn size_of(&self, ty: &Type) -> Option<u32> {
        self.size_guarded(ty, &mut Vec::new())
    }

    fn size_guarded<'a>(&'a self, ty: &'a Type, seen: &mut Vec<&'a str>) -> Option<u32> {
        match ty {
            Type::Pointer { .. } => Some(4),
            Type::Function(_) => Some(1),
            // Unknown past u32, rather than an overflow.
            Type::Array { of, len } => self.size_guarded(of, seen).and_then(|s| s.checked_mul(*len)),
            Type::Named { name } => {
                if let Some(s) = base_size(name) {
                    return Some(s);
                }
                let t = self.type_named(name)?;
                if seen.contains(&name.as_str()) {
                    return None;
                }
                seen.push(name);
                let size = match (&t.body, t.size) {
                    (_, Some(s)) => Some(s),
                    (TypeBody::Typedef { ty }, None) => self.size_guarded(ty, seen),
                    (TypeBody::Enum { .. }, None) => Some(4),
                    (TypeBody::Struct { fields } | TypeBody::Union { fields }, None) => {
                        let mut end = 0;
                        for f in fields {
                            end = end.max(f.offset + self.size_guarded(&f.ty, seen)?);
                        }
                        Some(end)
                    }
                };
                seen.pop();
                size
            }
        }
    }

    fn check(&mut self, problems: &mut Vec<Problem>) {
        let mut known: Vec<&str> = self.types.iter().map(|t| t.name.text.as_str()).collect();
        known.extend(BASE_TYPES.iter().map(|(n, _)| *n));
        let unknown = |ty: &Type, at: &At, problems: &mut Vec<Problem>| {
            let mut missing = Vec::new();
            named_in(ty, &mut missing);
            for m in missing {
                if !known.contains(&m.as_str()) {
                    let near = known.iter().find(|k| k.eq_ignore_ascii_case(&m));
                    let hint = near.map(|k| format!(" ({k}?)")).unwrap_or_default();
                    problems.push(Problem { at: at.clone(), message: format!("unknown type {m:?}{hint}") });
                }
            }
        };
        for t in &self.types {
            match &t.body {
                TypeBody::Typedef { ty } => unknown(ty, &t.at, problems),
                TypeBody::Struct { fields } | TypeBody::Union { fields } => {
                    for f in fields {
                        unknown(&f.ty, &f.at, problems);
                    }
                }
                TypeBody::Enum { .. } => {}
            }
        }
        for g in &self.globals {
            if let Some(ty) = &g.ty {
                unknown(ty, &g.at, problems);
            }
        }
        for f in &self.functions {
            if let Some(s) = &f.signature {
                unknown(&Type::Function(s.clone()), &f.at, problems);
            }
            for l in &f.locals {
                if let Some(ty) = &l.ty {
                    unknown(ty, &l.at, problems);
                }
            }
        }

        // A type that holds itself by value has no size.
        for t in &self.types {
            if t.size.is_none() && self.size_of(&Type::Named { name: t.name.text.clone() }).is_none() {
                let mut seen = vec![t.name.text.as_str()];
                if self.holds_itself(&t.body, &mut seen) {
                    problems.push(Problem { at: t.at.clone(), message: format!("{} contains itself", t.name.text) });
                }
            }
        }

        // Fields: sorted by offset; overlaps, the declared size, names used twice.
        let sizes: Vec<Vec<Option<u32>>> = self
            .types
            .iter()
            .map(|t| match &t.body {
                TypeBody::Struct { fields } | TypeBody::Union { fields } => {
                    fields.iter().map(|f| self.size_of(&f.ty)).collect()
                }
                _ => Vec::new(),
            })
            .collect();
        for (t, sizes) in self.types.iter_mut().zip(sizes) {
            let is_union = matches!(t.body, TypeBody::Union { .. });
            let (TypeBody::Struct { fields } | TypeBody::Union { fields }) = &mut t.body else { continue };
            let mut sized: Vec<(Field, Option<u32>)> = std::mem::take(fields).into_iter().zip(sizes).collect();
            sized.sort_by_key(|(f, _)| f.offset);
            // A second field of one name is left out, so the struct stays one
            // C can declare.
            let mut names_seen: HashMap<String, At> = HashMap::new();
            sized.retain(|(f, _)| match names_seen.get(&f.name.text) {
                Some(at) => {
                    problems.push(Problem {
                        at: f.at.clone(),
                        message: format!("{}.{} is already a field at {at}", t.name.text, f.name.text),
                    });
                    false
                }
                None => {
                    names_seen.insert(f.name.text.clone(), f.at.clone());
                    true
                }
            });
            for (i, (f, size)) in sized.iter().enumerate() {
                let end = f.offset + size.unwrap_or(1);
                if let Some(total) = t.size.filter(|&total| end > total) {
                    problems.push(Problem {
                        at: f.at.clone(),
                        message: format!(
                            "{}.{} ends at 0x{end:X}, past the size 0x{total:X}",
                            t.name.text, f.name.text
                        ),
                    });
                }
                if is_union {
                    continue;
                }
                if let Some((g, _)) = sized[i + 1..].iter().find(|(g, _)| g.offset < end) {
                    problems.push(Problem {
                        at: g.at.clone(),
                        message: format!(
                            "{}.{} at 0x{:X} overlaps {} (0x{:X}..0x{end:X}) at {}",
                            t.name.text, g.name.text, g.offset, f.name.text, f.offset, f.at
                        ),
                    });
                }
            }
            *fields = sized.into_iter().map(|(f, _)| f).collect();
        }

        for f in &self.functions {
            let mut slots: Vec<(&Slot, &At)> = Vec::new();
            for l in &f.locals {
                if let Some((_, at)) = slots.iter().find(|(s, _)| **s == l.slot) {
                    problems.push(Problem {
                        at: l.at.clone(),
                        message: format!("{} already names this local at {at}", f.name.text),
                    });
                }
                slots.push((&l.slot, &l.at));
            }
        }

        // One name for two functions or globals in the same module.
        let mut by_name: BTreeMap<(Option<&str>, &str), &At> = BTreeMap::new();
        for (module, name, at) in self
            .functions
            .iter()
            .map(|f| (f.module.as_deref(), f.name.text.as_str(), &f.at))
            .chain(self.globals.iter().map(|g| (g.module.as_deref(), g.name.text.as_str(), &g.at)))
        {
            if let Some(first) = by_name.insert((module, name), at) {
                problems
                    .push(Problem { at: at.clone(), message: format!("{name} already names something at {first}") });
            }
        }
    }

    fn holds_itself<'a>(&'a self, body: &'a TypeBody, seen: &mut Vec<&'a str>) -> bool {
        let fields = match body {
            TypeBody::Struct { fields } | TypeBody::Union { fields } => fields.iter().map(|f| &f.ty).collect(),
            TypeBody::Typedef { ty } => vec![ty],
            TypeBody::Enum { .. } => Vec::new(),
        };
        for mut ty in fields {
            while let Type::Array { of, .. } = ty {
                ty = of;
            }
            let Type::Named { name } = ty else { continue };
            if seen.contains(&name.as_str()) {
                return true;
            }
            if let Some(t) = self.type_named(name) {
                seen.push(name);
                if self.holds_itself(&t.body, seen) {
                    return true;
                }
                seen.pop();
            }
        }
        false
    }
}

fn named_in(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Named { name } => out.push(name.clone()),
        Type::Pointer { to, .. } => named_in(to, out),
        Type::Array { of, .. } => named_in(of, out),
        Type::Function(s) => {
            named_in(&s.ret, out);
            for p in &s.params {
                named_in(&p.ty, out);
            }
        }
    }
}

// ---------------------------------------------------------------- parsing

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    /// A number as written (`0x38`, `56`, `8007528C`); its base depends on
    /// where it stands.
    Num(String),
    Punct(char),
    Ellipsis,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push(Tok::Num(b[start..i].iter().collect()));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '.') {
                i += 1;
            }
            out.push(Tok::Ident(b[start..i].iter().collect()));
        } else if c == '.' && b.get(i + 1) == Some(&'.') && b.get(i + 2) == Some(&'.') {
            out.push(Tok::Ellipsis);
            i += 3;
        } else if "*+-()[],{}=?".contains(c) {
            out.push(Tok::Punct(c));
            i += 1;
        } else {
            return Err(format!("unexpected {c:?}"));
        }
    }
    Ok(out)
}

/// A number standing for a size, offset, count or value: `0x` hex, else decimal.
fn number(s: &str) -> Option<i64> {
    let s = s.replace('_', "");
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// An address: always hex, with or without `0x`; RAM mirrors fold to KSEG0.
fn address(s: &str) -> Option<u32> {
    let a = psx_media::memmap::parse_hex(&s.replace('_', ""))?;
    let r = psx_media::memmap::resolve(a);
    Some(if r.region == psx_media::memmap::Region::MainRam { 0x8000_0000 | r.offset } else { a })
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|c| c.is_alphabetic() || c == '_') && c.all(|c| c.is_alphanumeric() || c == '_')
}

struct Cursor<'a> {
    toks: &'a [Tok],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<&'a Tok> {
        self.toks.get(self.i)
    }
    fn next(&mut self) -> Option<&'a Tok> {
        let t = self.toks.get(self.i);
        self.i += 1;
        t
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn done(&self) -> bool {
        self.i >= self.toks.len()
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) { Ok(()) } else { Err(format!("expected '{c}'{}", self.found())) }
    }
    fn found(&self) -> String {
        match self.peek() {
            None => " at the end of the line".into(),
            Some(Tok::Ident(s) | Tok::Num(s)) => format!(", found {s:?}"),
            Some(Tok::Punct(c)) => format!(", found '{c}'"),
            Some(Tok::Ellipsis) => ", found '...'".into(),
        }
    }
    fn ident(&mut self, what: &str) -> Result<String, String> {
        match self.peek() {
            Some(Tok::Ident(s)) => {
                self.i += 1;
                Ok(s.clone())
            }
            _ => Err(format!("expected {what}{}", self.found())),
        }
    }
    fn name(&mut self, what: &str) -> Result<Name, String> {
        let text = self.ident(what)?;
        if !is_ident(&text) {
            return Err(format!("{text:?} is not a name"));
        }
        Ok(Name { text, guess: self.eat('?') })
    }
    fn num(&mut self, what: &str) -> Result<i64, String> {
        let neg = self.eat('-');
        match self.peek() {
            Some(Tok::Num(s)) => {
                self.i += 1;
                let n = number(s).ok_or_else(|| format!("{s:?} is not a number (hex needs 0x)"))?;
                Ok(if neg { -n } else { n })
            }
            _ => Err(format!("expected {what}{}", self.found())),
        }
    }
    fn unsigned(&mut self, what: &str) -> Result<u32, String> {
        let n = self.num(what)?;
        u32::try_from(n).map_err(|_| format!("{what} {n} is out of range"))
    }
}

/// A base type: one name, C's multi-word spellings folded (`unsigned short`
/// is `ushort`), `struct`/`union`/`enum` before a name ignored.
fn base_type(c: &mut Cursor) -> Result<Type, String> {
    let first = c.ident("a type")?;
    let name = match first.as_str() {
        "struct" | "union" | "enum" => c.ident("a type name")?,
        "unsigned" | "signed" => {
            let u = first == "unsigned";
            let rest = match c.peek() {
                Some(Tok::Ident(s)) if ["char", "short", "int", "long"].contains(&s.as_str()) => {
                    c.i += 1;
                    s.clone()
                }
                _ => "int".into(),
            };
            match (u, rest.as_str()) {
                (true, "char") => "uchar".into(),
                (true, "short") => "ushort".into(),
                (true, "long") => "ulong".into(),
                (true, _) => "uint".into(),
                (false, "char") => "i8".into(),
                (false, r) => r.into(),
            }
        }
        "long" if matches!(c.peek(), Some(Tok::Ident(s)) if s == "long") => {
            c.i += 1;
            "longlong".into()
        }
        _ => first,
    };
    if !is_ident(&name) {
        return Err(format!("{name:?} is not a type name"));
    }
    let mut ty = Type::Named { name };
    while c.eat('*') {
        let offset = if c.eat('+') { c.unsigned("a pointer offset")? } else { 0 };
        ty = Type::Pointer { to: Box::new(ty), offset };
    }
    Ok(ty)
}

/// `TYPE NAME[?][N]...` or `TYPE (*NAME)(PARAMS)`; the name may be optional.
fn declaration(c: &mut Cursor, name_required: bool) -> Result<(Type, Option<Name>), String> {
    let base = base_type(c)?;
    if c.eat('(') {
        // A function pointer: TYPE (*NAME)(PARAMS)
        let mut stars = 0;
        while c.eat('*') {
            stars += 1;
        }
        if stars == 0 {
            return Err("expected '*' in a function pointer".into());
        }
        let name = if matches!(c.peek(), Some(Tok::Ident(_))) { Some(c.name("a name")?) } else { None };
        c.expect(')')?;
        c.expect('(')?;
        let sig = params(c, base)?;
        let mut ty = Type::Function(sig);
        for _ in 0..stars {
            ty = Type::Pointer { to: Box::new(ty), offset: 0 };
        }
        if name.is_none() && name_required {
            return Err("expected a name in the function pointer".into());
        }
        return Ok((ty, name));
    }
    let name = match c.peek() {
        Some(Tok::Ident(_)) => Some(c.name("a name")?),
        _ if name_required => return Err(format!("expected a name{}", c.found())),
        _ => None,
    };
    let mut dims = Vec::new();
    while c.eat('[') {
        dims.push(c.unsigned("an array length")?);
        c.expect(']')?;
    }
    let mut ty = base;
    for len in dims.into_iter().rev() {
        ty = Type::Array { of: Box::new(ty), len };
    }
    Ok((ty, name))
}

/// The parameter list after its '(' up to and including ')'.
fn params(c: &mut Cursor, ret: Type) -> Result<Signature, String> {
    let mut sig = Signature { ret: Box::new(ret), params: Vec::new(), varargs: false };
    if c.eat(')') {
        return Ok(sig);
    }
    if matches!(c.peek(), Some(Tok::Ident(s)) if s == "void") && c.toks.get(c.i + 1) == Some(&Tok::Punct(')')) {
        c.i += 2;
        return Ok(sig);
    }
    loop {
        if c.peek() == Some(&Tok::Ellipsis) {
            c.i += 1;
            sig.varargs = true;
            c.expect(')')?;
            return Ok(sig);
        }
        let (ty, name) = declaration(c, false)?;
        sig.params.push(Param { ty, name });
        if c.eat(')') {
            return Ok(sig);
        }
        c.expect(',')?;
    }
}

/// `in MODULE` after an address.
fn module(c: &mut Cursor) -> Result<Option<String>, String> {
    if matches!(c.peek(), Some(Tok::Ident(s)) if s == "in") && c.toks.len() > c.i + 2 {
        c.i += 1;
        return Ok(Some(c.ident("a module")?));
    }
    Ok(None)
}

fn addr_tok(c: &mut Cursor) -> Result<u32, String> {
    match c.next() {
        Some(Tok::Num(s) | Tok::Ident(s)) => address(s).ok_or_else(|| format!("{s:?} is not an address")),
        _ => Err("expected an address".into()),
    }
}

fn end(c: &Cursor) -> Result<(), String> {
    if c.done() { Ok(()) } else { Err(format!("unexpected text{}", c.found())) }
}

const KEYWORDS: [&str; 6] = ["struct", "union", "enum", "typedef", "global", "function"];

#[derive(Clone, Copy)]
enum Open {
    None,
    Fields { ix: usize, union: bool },
    Values { ix: usize },
    Function { ix: usize },
}

/// Parses one file. Each line stands alone; what fails is reported and
/// skipped.
fn parse(file: &str, text: &str) -> (Vec<Item>, Vec<Problem>) {
    let mut items: Vec<Item> = Vec::new();
    let mut problems = Vec::new();
    let mut open = Open::None;
    let mut pending: Vec<String> = Vec::new();
    let mut open_at: Option<At> = None;
    for (n, raw) in text.lines().enumerate() {
        let at = At { file: file.to_string(), line: n as u32 + 1 };
        let (code, comment) = match raw.find("//") {
            Some(i) => (&raw[..i], Some(raw[i + 2..].trim().to_string()).filter(|s| !s.is_empty())),
            None => (raw, None),
        };
        if code.trim().is_empty() {
            match comment {
                Some(c) => pending.push(c),
                None => pending.clear(),
            }
            continue;
        }
        let indented = code.starts_with([' ', '\t']);
        let doc = (!pending.is_empty()).then(|| pending.join("\n"));
        pending.clear();
        let comment = match (doc, comment) {
            (Some(d), Some(c)) => Some(format!("{d}\n{c}")),
            (d, c) => d.or(c),
        };
        let toks = match tokenize(code) {
            Ok(t) => t,
            Err(e) => {
                problems.push(Problem { at, message: e });
                continue;
            }
        };
        let mut c = Cursor { toks: &toks, i: 0 };
        let keyword = matches!(toks.first(), Some(Tok::Ident(k)) if KEYWORDS.contains(&k.as_str()));
        if let (Open::Fields { .. } | Open::Values { .. }, true) = (open, keyword) {
            if let Some(at) = open_at.take() {
                problems.push(Problem { at, message: "no closing '}'".into() });
            }
            open = Open::None;
        }
        let result = (|| -> Result<(), String> {
            match open {
                Open::Fields { ix, union } => {
                    if c.eat('}') {
                        open = Open::None;
                        return end(&c);
                    }
                    let offset =
                        if union && !matches!(c.peek(), Some(Tok::Num(_))) { 0 } else { c.unsigned("a field offset")? };
                    let (ty, name) = declaration(&mut c, true)?;
                    end(&c)?;
                    let Item::Type(t) = &mut items[ix] else { unreachable!() };
                    let (TypeBody::Struct { fields } | TypeBody::Union { fields }) = &mut t.body else {
                        unreachable!()
                    };
                    fields.push(Field { offset, ty, name: name.unwrap(), comment, at: at.clone() });
                    Ok(())
                }
                Open::Values { ix } => {
                    if c.eat('}') {
                        open = Open::None;
                        return end(&c);
                    }
                    let name = c.name("a value name")?;
                    c.expect('=')?;
                    let value = c.num("a value")?;
                    end(&c)?;
                    let Item::Type(t) = &mut items[ix] else { unreachable!() };
                    let TypeBody::Enum { values } = &mut t.body else { unreachable!() };
                    values.push(EnumValue { name, value, comment, at: at.clone() });
                    Ok(())
                }
                Open::Function { ix } if indented => {
                    let key = c.ident("a local (local_230, sp+0x10 or a register)")?;
                    let slot = if key == "sp" {
                        let neg = if c.eat('+') {
                            false
                        } else if c.eat('-') {
                            true
                        } else {
                            return Err("expected sp+OFFSET".into());
                        };
                        let n = i32::try_from(c.unsigned("a stack offset")?).map_err(|_| "offset too large")?;
                        Slot::Frame(if neg { -n } else { n })
                    } else if let Some(h) = key.strip_prefix("local_") {
                        let n = i64::from_str_radix(h, 16).map_err(|_| format!("{key:?} is not local_HEX"))?;
                        Slot::Stack(-i32::try_from(n).map_err(|_| "offset too large")?)
                    } else if REGISTERS.contains(&key.as_str()) {
                        Slot::Register(key)
                    } else {
                        return Err(format!(
                            "{key:?} is not a local's place: local_HEX (as the decompile names it), sp+OFFSET or a register"
                        ));
                    };
                    // `KEY NAME` or `KEY TYPE NAME`.
                    let (ty, name) = if c.toks.len() - c.i <= 2
                        && matches!(c.peek(), Some(Tok::Ident(_)))
                        && c.toks[c.i + 1..].iter().all(|t| *t == Tok::Punct('?'))
                    {
                        (None, c.name("a name")?)
                    } else {
                        let (t, n) = declaration(&mut c, true)?;
                        (Some(t), n.unwrap())
                    };
                    end(&c)?;
                    let Item::Function(f) = &mut items[ix] else { unreachable!() };
                    f.locals.push(Local { slot, ty, name, comment, at: at.clone() });
                    Ok(())
                }
                _ => {
                    open = Open::None;
                    if indented && !keyword {
                        return Err("an indented line belongs under a function (its locals)".into());
                    }
                    let kw = c.ident("struct, union, enum, typedef, global or function")?;
                    match kw.as_str() {
                        "struct" | "union" | "enum" => {
                            let name = c.name("a type name")?;
                            let size = match c.peek() {
                                Some(Tok::Num(_)) => Some(c.unsigned("a size")?),
                                _ => None,
                            };
                            let braces = c.eat('{');
                            end(&c)?;
                            let body = match kw.as_str() {
                                "struct" => TypeBody::Struct { fields: Vec::new() },
                                "union" => TypeBody::Union { fields: Vec::new() },
                                _ => TypeBody::Enum { values: Vec::new() },
                            };
                            let ix = items.len();
                            items.push(Item::Type(TypeDef { name, size, body, comment, at: at.clone() }));
                            if braces {
                                open_at = Some(at.clone());
                                open = match kw.as_str() {
                                    "enum" => Open::Values { ix },
                                    k => Open::Fields { ix, union: k == "union" },
                                };
                            }
                            Ok(())
                        }
                        "typedef" => {
                            let (ty, name) = declaration(&mut c, true)?;
                            end(&c)?;
                            items.push(Item::Type(TypeDef {
                                name: name.unwrap(),
                                size: None,
                                body: TypeBody::Typedef { ty },
                                comment,
                                at: at.clone(),
                            }));
                            Ok(())
                        }
                        "global" => {
                            let address = addr_tok(&mut c)?;
                            let module = module(&mut c)?;
                            let (ty, name) = if c.toks.len() - c.i <= 2
                                && matches!(c.peek(), Some(Tok::Ident(_)))
                                && c.toks[c.i + 1..].iter().all(|t| *t == Tok::Punct('?'))
                            {
                                (None, c.name("a name")?)
                            } else {
                                let (t, n) = declaration(&mut c, true)?;
                                (Some(t), n.unwrap())
                            };
                            end(&c)?;
                            items.push(Item::Global(Global { address, module, ty, name, comment, at: at.clone() }));
                            Ok(())
                        }
                        "function" => {
                            let address = addr_tok(&mut c)?;
                            let module = module(&mut c)?;
                            let (name, signature) = if c.toks.len() - c.i <= 2
                                && matches!(c.peek(), Some(Tok::Ident(_)))
                                && c.toks[c.i + 1..].iter().all(|t| *t == Tok::Punct('?'))
                            {
                                (c.name("a name")?, None)
                            } else {
                                let ret = base_type(&mut c)?;
                                let name = c.name("the function's name")?;
                                c.expect('(')?;
                                (name, Some(params(&mut c, ret)?))
                            };
                            end(&c)?;
                            open = Open::Function { ix: items.len() };
                            items.push(Item::Function(Function {
                                address,
                                module,
                                name,
                                signature,
                                locals: Vec::new(),
                                comment,
                                at: at.clone(),
                            }));
                            Ok(())
                        }
                        other => {
                            Err(format!("expected struct, union, enum, typedef, global or function, found {other:?}"))
                        }
                    }
                }
            }
        })();
        if let Err(message) = result {
            problems.push(Problem { at, message });
        }
    }
    if let (Open::Fields { .. } | Open::Values { .. }, Some(at)) = (open, open_at) {
        problems.push(Problem { at, message: "no closing '}'".into() });
    }
    (items, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_in_names_is_not_followed() {
        let dir = std::env::temp_dir().join(format!("psx-names-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.names"), "").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("sub/loop")).unwrap();
        let mut out = Vec::new();
        walk(&dir, &mut out);
        assert_eq!(out, vec![dir.join("sub/a.names")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn one(text: &str) -> Names {
        from_texts(&[("names/a.names".into(), text.into())], &[])
    }

    fn messages(n: &Names) -> Vec<String> {
        n.problems.iter().map(|p| format!("{}: {}", p.at, p.message)).collect()
    }

    const CROC: &str = "\
// Croc's object
struct Object 0x988 {
  0x038 int floor_height        // out of world_collide
  0x040 ProbeOffset probe_offset[6]
  0x084 ProbeHint probe_hint[6]
  0x076 u16 wall_bits?
}
struct ProbeOffset 8 {
  0 i16 x
  2 i16 y
  4 i16 z
}
struct ProbeHint 8 {
  0x0 i16 floor_piece
  0x2 u16 floor_poly
}
typedef unsigned short Angle
enum Gait {
  WALK = 0
  RUN = 1   // held
}
global 0x8007528C Object *croc
global 0x80074D08 int speed_x?    // gp+0x1D0
global 0x8007_5448 resources
function 0x80031F64 void world_collide(Object *obj, int unk, Area *area)
  local_150 Object *+0x38 obj_38
  local_230 int p_x
  s0 i
  sp+0x10 int arg4
function 0x800306B0 floor_find
function 0x80100000 in RACE.OVL race_update?
function 0x80040000 void set_handler(void (*fn)(Object *, int), ...)
struct Area 0x74 {
  0x54 void *placements
}
";

    #[test]
    fn every_kind_parses() {
        let n = one(CROC);
        assert_eq!(messages(&n), Vec::<String>::new());
        let obj = n.type_named("Object").unwrap();
        assert_eq!(obj.comment.as_deref(), Some("Croc's object"));
        let TypeBody::Struct { fields } = &obj.body else { panic!() };
        // Sorted by offset.
        assert_eq!(fields.iter().map(|f| f.offset).collect::<Vec<_>>(), [0x38, 0x40, 0x76, 0x84]);
        assert!(fields[2].name.guess);
        assert_eq!(fields[0].comment.as_deref(), Some("out of world_collide"));
        assert_eq!(fields[1].ty, Type::Array { of: Box::new(Type::Named { name: "ProbeOffset".into() }), len: 6 });
        assert_eq!(n.size_of(&Type::Named { name: "Object".into() }), Some(0x988));
        assert_eq!(n.size_of(&fields[3].ty), Some(48));
        let angle = n.type_named("Angle").unwrap();
        assert_eq!(angle.body, TypeBody::Typedef { ty: Type::Named { name: "ushort".into() } });
        let TypeBody::Enum { values } = &n.type_named("Gait").unwrap().body else { panic!() };
        assert_eq!((values[1].name.text.as_str(), values[1].value), ("RUN", 1));

        assert_eq!(n.globals.len(), 3);
        assert_eq!(n.globals[1].name, Name { text: "speed_x".into(), guess: true });
        assert_eq!(n.globals[2].address, 0x8007_5448);
        assert_eq!(n.globals[2].ty, None);

        let wc = &n.functions[0];
        let sig = wc.signature.as_ref().unwrap();
        assert_eq!(sig.params.len(), 3);
        assert_eq!(sig.params[2].name.as_ref().unwrap().text, "area");
        assert_eq!(wc.locals[0].slot, Slot::Stack(-0x150));
        assert_eq!(
            wc.locals[0].ty,
            Some(Type::Pointer { to: Box::new(Type::Named { name: "Object".into() }), offset: 0x38 })
        );
        assert_eq!(wc.locals[2].slot, Slot::Register("s0".into()));
        assert_eq!(wc.locals[2].ty, None);
        assert_eq!(wc.locals[3].slot, Slot::Frame(0x10));
        assert_eq!(n.functions[1].signature, None);
        assert_eq!(n.functions[2].module.as_deref(), Some("RACE.OVL"));
        assert!(n.functions[2].name.guess);
        let sh = n.functions[3].signature.as_ref().unwrap();
        assert!(sh.varargs);
        let Type::Pointer { to, .. } = &sh.params[0].ty else { panic!() };
        let Type::Function(fp) = &**to else { panic!() };
        assert_eq!(fp.params.len(), 2);
    }

    #[test]
    fn addresses_fold_mirrors_and_take_bare_hex() {
        let n = one("global 0007528C a\nglobal A007528D b\nglobal 1F800010 c\n");
        assert_eq!(n.globals.iter().map(|g| g.address).collect::<Vec<_>>(), [0x8007_528C, 0x8007_528D, 0x1F80_0010]);
    }

    #[test]
    fn overlapping_and_oversized_fields_are_reported() {
        let n = one("struct S 8 {\n  0 int a\n  2 u16 b\n  6 int c\n}\n");
        assert_eq!(
            messages(&n),
            [
                "names/a.names:3: S.b at 0x2 overlaps a (0x0..0x4) at names/a.names:2",
                "names/a.names:4: S.c ends at 0xA, past the size 0x8",
            ]
        );
    }

    #[test]
    fn a_field_name_used_twice_keeps_the_first() {
        let n = one("struct S {\n  0x10 u16 shift\n  0x4 u16 shift\n}\n");
        assert_eq!(messages(&n), ["names/a.names:2: S.shift is already a field at names/a.names:3"]);
        let TypeBody::Struct { fields } = &n.type_named("S").unwrap().body else { panic!() };
        assert_eq!(fields.iter().map(|f| f.offset).collect::<Vec<_>>(), [0x4]);
    }

    #[test]
    fn unions_overlap_freely() {
        let n = one("union U {\n  int a\n  u16 b\n  4 u8 c\n}\n");
        assert_eq!(messages(&n), Vec::<String>::new());
        assert_eq!(n.size_of(&Type::Named { name: "U".into() }), Some(5));
    }

    #[test]
    fn a_struct_continues_in_another_file() {
        let n = from_texts(
            &[
                ("names/a.names".into(), "struct Object 0x988 {\n  0x38 int floor\n}\n".into()),
                ("names/b.names".into(), "struct Object {\n  0x370 int x\n  0x38 int floor2\n}\n".into()),
            ],
            &[],
        );
        assert_eq!(
            messages(&n),
            ["names/b.names:3: Object.floor2 at 0x38 overlaps floor (0x38..0x3C) at names/a.names:2"]
        );
        let TypeBody::Struct { fields } = &n.type_named("Object").unwrap().body else { panic!() };
        assert_eq!(fields.len(), 3);
    }

    #[test]
    fn duplicates_across_files_name_both_lines() {
        let n = from_texts(
            &[
                ("names/a.names".into(), "function 0x80031F64 world_collide\nglobal 0x8007528C croc\n".into()),
                ("names/b.names".into(), "\nfunction 0x80031F64 collide\nglobal 0x80070000 croc\n".into()),
            ],
            &[],
        );
        assert_eq!(
            messages(&n),
            [
                "names/b.names:2: function 0x80031F64 is already named world_collide at names/a.names:1",
                "names/b.names:3: croc already names something at names/a.names:2",
            ]
        );
    }

    #[test]
    fn the_agent_wins_over_imports() {
        let n = from_texts(
            &[("names/a.names".into(), "function 0x80031F64 world_collide\nstruct Object 4 {\n  0 int a\n}\n".into())],
            &[(
                ".psx/names/sym.names".into(),
                "function 0x80031F64 Collide\nfunction 0x80031000 Other\nstruct Object 8 {\n  4 int b\n}\n".into(),
            )],
        );
        assert_eq!(messages(&n), Vec::<String>::new());
        assert_eq!(n.functions.iter().map(|f| f.name.text.as_str()).collect::<Vec<_>>(), ["world_collide", "Other"]);
        assert_eq!(n.type_named("Object").unwrap().size, Some(4));
    }

    #[test]
    fn overlays_keep_separate_names_at_one_address() {
        let n = one("function 0x80100000 in RACE.OVL race_update\nfunction 0x80100000 in MENU.OVL menu_update\n");
        assert_eq!(messages(&n), Vec::<String>::new());
    }

    #[test]
    fn unknown_types_suggest_a_spelling() {
        let n = one("struct ProbeHint 8 {\n}\nglobal 0x80070000 Probehint *h\nfunction 0x80070100 Thing *f(int)\n");
        assert_eq!(
            messages(&n),
            ["names/a.names:3: unknown type \"Probehint\" (ProbeHint?)", "names/a.names:4: unknown type \"Thing\"",]
        );
    }

    #[test]
    fn errors_name_the_line_and_what_was_expected() {
        let n = one(
            "struct S {\n  int a\n  0x4 int\n}\nfunction 0x80031F64 void f(int a\nglobal zz x\nobject 0x80000000 y\n  local_10 int i\nfunction 0x80000000 g\n  t int i\n  local_zz i\nstruct T {\n  0 int a\n",
        );
        assert_eq!(
            messages(&n),
            [
                "names/a.names:2: expected a field offset, found \"int\"",
                "names/a.names:3: expected a name at the end of the line",
                "names/a.names:5: expected ',' at the end of the line",
                "names/a.names:6: \"zz\" is not an address",
                "names/a.names:7: expected struct, union, enum, typedef, global or function, found \"object\"",
                "names/a.names:8: an indented line belongs under a function (its locals)",
                "names/a.names:10: \"t\" is not a local's place: local_HEX (as the decompile names it), sp+OFFSET or a register",
                "names/a.names:11: \"local_zz\" is not local_HEX",
                "names/a.names:12: no closing '}'",
            ]
        );
    }

    #[test]
    fn a_local_named_twice_and_a_type_holding_itself() {
        let n = one("function 0x80000000 f\n  local_10 a\n  local_10 b\nstruct L {\n  0 L next\n}\n");
        assert_eq!(
            messages(&n),
            ["names/a.names:3: f already names this local at names/a.names:2", "names/a.names:4: L contains itself",]
        );
    }

    #[test]
    fn comments_above_attach_and_blank_lines_detach() {
        let n = one("// file header\n\n// the player\n// (evidence)\nglobal 0x80000000 int p\n");
        assert_eq!(n.globals[0].comment.as_deref(), Some("the player\n(evidence)"));
    }

    #[test]
    fn load_reads_both_directories_recursively() {
        let root = std::env::temp_dir().join(format!("psx-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("names/walls")).unwrap();
        std::fs::create_dir_all(root.join(".psx/names")).unwrap();
        std::fs::write(root.join("names/walls/object.names"), "global 0x80000000 int a\n").unwrap();
        std::fs::write(root.join("names/notes.txt"), "not names").unwrap();
        std::fs::write(root.join(".psx/names/sym.names"), "global 0x80000000 int b\nglobal 0x80000004 c\nbad\n")
            .unwrap();
        let n = load(&root);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(n.globals.iter().map(|g| g.name.text.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(n.globals[0].at, At { file: "names/walls/object.names".into(), line: 1 });
        assert_eq!(
            messages(&n),
            [".psx/names/sym.names:3: expected struct, union, enum, typedef, global or function, found \"bad\""]
        );
    }

    #[test]
    fn the_model_serializes() {
        let n = one(CROC);
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["functions"][0]["locals"][0]["type"]["offset"], 0x38);
        assert_eq!(v["globals"][0]["address"], "0x8007528C");
        assert_eq!(v["functions"][0]["locals"][0]["slot"], serde_json::json!({"kind": "stack", "at": -0x150}));
    }
}

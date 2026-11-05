//! The glossary written as Rust (docs/scenarios.md): a crate of plain,
//! dependency-free Rust whose crate documentation and `Quantities` struct
//! declare it. It is read from source, without building it, into
//! [`Glossary`], and held to a portable subset of Rust. The glue the
//! glossary's binding and the port need is generated from the same source,
//! in their `build.rs`.

use std::collections::BTreeMap;

use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::scenario::Glossary;
use crate::value::{Field, Kind};

/// The crate's root file, relative to the crate.
pub const LIB: &str = "src/lib.rs";

/// Macros the glossary may call: the standard library's checks and `vec!`.
const MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "matches",
    "panic",
    "unreachable",
    "vec",
];

/// Derives the glossary may use: the standard library's own.
const DERIVES: &[&str] = &["Clone", "Copy", "Debug", "Default", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash"];

/// Types the glossary may not use: maps and sets with an order of their own,
/// and shared or interior mutability.
const TYPES: &[&str] = &[
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "BinaryHeap",
    "LinkedList",
    "Rc",
    "Arc",
    "RefCell",
    "Cell",
    "Mutex",
    "RwLock",
];

/// A crate's files: `Cargo.toml`, and each `.rs` file under `src/`
/// by its path relative to the crate (`src/lib.rs` among them).
pub struct Crate<'a> {
    pub cargo_toml: &'a str,
    pub files: &'a [(String, String)],
}

struct Problems<'a> {
    file: &'a str,
    out: &'a mut Vec<String>,
}

impl Problems<'_> {
    fn at(&mut self, line: usize, what: &str) {
        self.out.push(format!("{}:{line}: {what}", self.file));
    }
}

fn line<T: Spanned>(t: &T) -> usize {
    t.span().start().line
}

/// What is wrong with the glossary's `Cargo.toml`: it has no build script,
/// is no procedural macro crate and depends on nothing.
fn manifest(text: &str, p: &mut Vec<String>) {
    let toml: toml::Table = match toml::from_str(text) {
        Ok(t) => t,
        Err(e) => return p.push(format!("Cargo.toml: {}", e.message())),
    };
    let package = toml.get("package").and_then(|v| v.as_table());
    if package.is_some_and(|t| t.contains_key("build")) {
        p.push("Cargo.toml: the glossary has no build script".into());
    }
    if toml.get("lib").and_then(|l| l.get("proc-macro")).is_some() {
        p.push("Cargo.toml: the glossary is not a procedural macro crate".into());
    }
    let deps = ["dependencies", "dev-dependencies", "build-dependencies"].iter().any(|k| toml.contains_key(*k));
    if deps || toml.keys().any(|k| k.starts_with("target")) {
        p.push("Cargo.toml: the glossary depends on nothing".into());
    }
}

/// The items of `src/lib.rs` that declare types, by name.
struct Items<'f> {
    structs: BTreeMap<String, &'f syn::ItemStruct>,
    enums: BTreeMap<String, &'f syn::ItemEnum>,
    aliases: BTreeMap<String, &'f syn::ItemType>,
    consts: BTreeMap<String, &'f syn::ItemConst>,
}

impl<'f> Items<'f> {
    fn of(file: &'f syn::File) -> Items<'f> {
        let mut i = Items {
            structs: BTreeMap::new(),
            enums: BTreeMap::new(),
            aliases: BTreeMap::new(),
            consts: BTreeMap::new(),
        };
        for item in &file.items {
            match item {
                syn::Item::Struct(s) => {
                    i.structs.insert(s.ident.to_string(), s);
                }
                syn::Item::Enum(e) => {
                    i.enums.insert(e.ident.to_string(), e);
                }
                syn::Item::Type(t) => {
                    i.aliases.insert(t.ident.to_string(), t);
                }
                syn::Item::Const(c) => {
                    i.consts.insert(c.ident.to_string(), c);
                }
                _ => {}
            }
        }
        i
    }

    /// The field a Rust type declares, with its documentation.
    fn field(&self, ty: &syn::Type, attrs: &[syn::Attribute], depth: u32) -> Result<Field, String> {
        let (description, unit) = field_docs(attrs);
        let mut f = self.ty(ty, depth)?;
        f.description = description;
        f.unit = unit;
        Ok(f)
    }

    fn ty(&self, ty: &syn::Type, depth: u32) -> Result<Field, String> {
        if depth > 16 {
            return Err("types nest more than 16 deep (a type that holds itself?)".into());
        }
        let shown = quote::ToTokens::to_token_stream(ty).to_string();
        let unsupported = || {
            format!(
                "`{shown}` is not a type a recording can hold: bool, u8 to u64, i8 to i64, f32, f64, a fixed-point alias (`type Fx16_16 = i32;`), an enum, a struct, `[T; N]` or `Vec<T>`"
            )
        };
        match ty {
            syn::Type::Array(a) => {
                let len = self.len(&a.len).ok_or_else(|| {
                    format!("`{shown}`: the length is an integer or a `const` of this file holding one")
                })?;
                let of = self.ty(&a.elem, depth + 1)?;
                Ok(Field { len: Some(len), of: Some(Box::new(of)), ..blank(Kind::Table) })
            }
            syn::Type::Paren(p) => self.ty(&p.elem, depth),
            syn::Type::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
                let seg = &p.path.segments[0];
                let name = seg.ident.to_string();
                if name == "Vec" {
                    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else { return Err(unsupported()) };
                    let Some(syn::GenericArgument::Type(of)) = args.args.first().filter(|_| args.args.len() == 1)
                    else {
                        return Err(unsupported());
                    };
                    let of = self.ty(of, depth + 1)?;
                    return Ok(Field { of: Some(Box::new(of)), ..blank(Kind::List) });
                }
                if !seg.arguments.is_none() {
                    return Err(unsupported());
                }
                if let Some(f) = primitive(&name) {
                    return Ok(f);
                }
                if let Some(a) = self.aliases.get(&name) {
                    if let Some((signed, int_bits, frac_bits)) = fixed_name(&name) {
                        let target = quote::ToTokens::to_token_stream(&a.ty).to_string();
                        return match primitive(&target) {
                            Some(Field { kind: Kind::Int, bits: Some(bits), signed: Some(s), .. })
                                if s == signed && bits == int_bits + frac_bits =>
                            {
                                Ok(Field {
                                    signed: Some(signed),
                                    int_bits: Some(int_bits),
                                    frac_bits: Some(frac_bits),
                                    ..blank(Kind::Fixed)
                                })
                            }
                            _ => Err(format!(
                                "`{name}` is {int_bits}.{frac_bits} fixed point, {}: alias {}{}",
                                if signed { "signed" } else { "unsigned" },
                                if signed { "i" } else { "u" },
                                int_bits + frac_bits
                            )),
                        };
                    }
                    return self.ty(&a.ty, depth + 1);
                }
                if let Some(e) = self.enums.get(&name) {
                    let mut variants = Vec::new();
                    for v in &e.variants {
                        if !matches!(v.fields, syn::Fields::Unit) {
                            return Err(format!(
                                "enum `{name}`: variant {} carries data; a field's enum has unit variants only",
                                v.ident
                            ));
                        }
                        variants.push(snake(&v.ident.to_string()));
                    }
                    return Ok(Field { variants: Some(variants), ..blank(Kind::Enum) });
                }
                if let Some(s) = self.structs.get(&name) {
                    let syn::Fields::Named(named) = &s.fields else {
                        return Err(format!("struct `{name}` has named fields to be a record"));
                    };
                    let mut fields = BTreeMap::new();
                    for f in &named.named {
                        let n = f.ident.as_ref().unwrap().to_string();
                        let field = self.field(&f.ty, &f.attrs, depth + 1).map_err(|e| format!("{name}.{n}: {e}"))?;
                        fields.insert(n, field);
                    }
                    return Ok(Field { fields: Some(fields), ..blank(Kind::Record) });
                }
                Err(unsupported())
            }
            _ => Err(unsupported()),
        }
    }

    /// An array length: a literal, or a `const` of this file holding one.
    fn len(&self, e: &syn::Expr) -> Option<u32> {
        match e {
            syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => i.base10_parse().ok(),
            syn::Expr::Path(p) => {
                let c = self.consts.get(&p.path.get_ident()?.to_string())?;
                self.len(&c.expr)
            }
            _ => None,
        }
    }

    /// Code giving the value of `place`, a value of type `ty` (as `ty`
    /// reads it), in the recording's encoding, as `out`'s `Value`. Types are
    /// named through `krate`, the glossary's crate.
    fn encode(&self, ty: &syn::Type, place: &str, krate: &str, out: Out, depth: u32) -> Result<String, String> {
        let v = out.value();
        if depth > 16 {
            return Err("types nest more than 16 deep".into());
        }
        let shown = quote::ToTokens::to_token_stream(ty).to_string();
        let each = |of: &syn::Type| -> Result<String, String> {
            let e = format!("e{depth}");
            let inner = self.encode(of, &format!("(*{e})"), krate, out, depth + 1)?;
            Ok(format!("{v}::List({place}.iter().map(|{e}| {inner}).collect())"))
        };
        match ty {
            syn::Type::Array(a) => each(&a.elem),
            syn::Type::Paren(p) => self.encode(&p.elem, place, krate, out, depth),
            syn::Type::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
                let seg = &p.path.segments[0];
                let name = seg.ident.to_string();
                if name == "Vec" {
                    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                        if let Some(syn::GenericArgument::Type(of)) = args.args.first() {
                            return each(of);
                        }
                    }
                    return Err(format!("`{shown}`"));
                }
                match primitive(&name).map(|f| f.kind) {
                    Some(Kind::Bool) => return Ok(format!("{v}::Bool({place})")),
                    Some(Kind::Float) => return Ok(format!("{v}::Int({place}.to_bits() as i128)")),
                    Some(_) => return Ok(format!("{v}::Int({place} as i128)")),
                    None => {}
                }
                if let Some(a) = self.aliases.get(&name) {
                    if fixed_name(&name).is_some() {
                        return Ok(format!("{v}::Int({place} as i128)"));
                    }
                    return self.encode(&a.ty, place, krate, out, depth + 1);
                }
                if let Some(e) = self.enums.get(&name) {
                    let arms: Vec<String> = e
                        .variants
                        .iter()
                        .map(|v| {
                            format!("{krate}::{name}::{} => {},", v.ident, out.variant(&snake(&v.ident.to_string())))
                        })
                        .collect();
                    return Ok(format!("match &{place} {{ {} }}", arms.join(" ")));
                }
                if let Some(s) = self.structs.get(&name) {
                    let syn::Fields::Named(named) = &s.fields else { return Err(format!("`{shown}`")) };
                    let mut fields = Vec::new();
                    for f in &named.named {
                        let n = f.ident.as_ref().unwrap().to_string();
                        fields.push(format!(
                            "({n:?}, {})",
                            self.encode(&f.ty, &format!("{place}.{n}"), krate, out, depth + 1)?
                        ));
                    }
                    return Ok(out.record(&fields));
                }
                Err(format!("`{shown}`"))
            }
            _ => Err(format!("`{shown}`")),
        }
    }
}

/// Which `Value` generated code builds: `psx_capture`'s, which a binding
/// answers psx with, or `psx_scenario`'s, which the port's scenarios
/// compare.
#[derive(Clone, Copy)]
enum Out {
    Capture,
    Recording,
}

impl Out {
    fn value(self) -> &'static str {
        match self {
            Out::Capture => "psx_capture::Value",
            Out::Recording => "psx_scenario::Value",
        }
    }

    fn variant(self, name: &str) -> String {
        match self {
            Out::Capture => format!("psx_capture::Value::Variant({name:?})"),
            Out::Recording => format!("psx_scenario::Value::Variant({name:?}.into())"),
        }
    }

    fn record(self, fields: &[String]) -> String {
        match self {
            Out::Capture => format!("psx_capture::Value::Record(vec![{}])", fields.join(", ")),
            Out::Recording => format!("psx_scenario::Value::record([{}])", fields.join(", ")),
        }
    }
}

/// The crate at `dir`: its `Cargo.toml` and each `.rs` file under `src/`,
/// which cargo is told to watch.
fn read_crate(dir: &std::path::Path) -> (String, Vec<(String, String)>) {
    println!("cargo::rerun-if-changed={}", dir.join("Cargo.toml").display());
    println!("cargo::rerun-if-changed={}", dir.join("src").display());
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let cargo_toml = read(&dir.join("Cargo.toml"));
    let mut files = Vec::new();
    let mut todo = vec![dir.join("src")];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("{}: {e}", d.display())).flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                files.push((rel, read(&p)));
            }
        }
    }
    files.sort();
    (cargo_toml, files)
}

/// The package's name.
fn package(cargo_toml: &str) -> Result<String, String> {
    package_name(cargo_toml).ok_or_else(|| "Cargo.toml: no package name".to_string())
}

/// The package's name, if `Cargo.toml` gives one.
pub fn package_name(cargo_toml: &str) -> Option<String> {
    toml::from_str::<toml::Table>(cargo_toml)
        .ok()
        .and_then(|t| t.get("package")?.get("name")?.as_str().map(String::from))
}

/// Writes `code` to `$OUT_DIR/<name>`.
fn write_out(name: &str, code: &str) {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("run from build.rs")).join(name);
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}

fn blank(kind: Kind) -> Field {
    Field {
        kind,
        bits: None,
        signed: None,
        int_bits: None,
        frac_bits: None,
        variants: None,
        of: None,
        len: None,
        fields: None,
        unit: None,
        description: None,
    }
}

fn primitive(name: &str) -> Option<Field> {
    let int = |bits, signed| Field { bits: Some(bits), signed: Some(signed), ..blank(Kind::Int) };
    Some(match name {
        "bool" => blank(Kind::Bool),
        "u8" => int(8, false),
        "u16" => int(16, false),
        "u32" => int(32, false),
        "u64" => int(64, false),
        "i8" => int(8, true),
        "i16" => int(16, true),
        "i32" => int(32, true),
        "i64" => int(64, true),
        "f32" => Field { bits: Some(32), ..blank(Kind::Float) },
        "f64" => Field { bits: Some(64), ..blank(Kind::Float) },
        _ => return None,
    })
}

/// `Fx<int>_<frac>` (signed) or `UFx<int>_<frac>` (unsigned): whether the
/// value is signed, its integer bits and its fraction bits.
fn fixed_name(name: &str) -> Option<(bool, u32, u32)> {
    let (signed, rest) = match name.strip_prefix("UFx") {
        Some(r) => (false, r),
        None => (true, name.strip_prefix("Fx")?),
    };
    let (i, f) = rest.split_once('_')?;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    (digits(i) && digits(f)).then_some(())?;
    Some((signed, i.parse().ok()?, f.parse().ok()?))
}

/// `BackingUp` → `backing_up`.
fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The text of `///` or `//!` comments, one entry per line.
fn doc_lines(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for a in attrs.iter().filter(|a| a.path().is_ident("doc")) {
        let syn::Meta::NameValue(nv) = &a.meta else { continue };
        let syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) = &nv.value else { continue };
        let text = s.value();
        // An empty `//!` line is an empty string: it still ends a paragraph.
        out.extend(text.split('\n').map(|l| l.strip_prefix(' ').unwrap_or(l).trim_end().to_string()));
    }
    out
}

/// A field's description, and its unit from a line `Unit: …`.
fn field_docs(attrs: &[syn::Attribute]) -> (Option<String>, Option<String>) {
    let mut unit = None;
    let mut text = Vec::new();
    for l in doc_lines(attrs) {
        match l.strip_prefix("Unit:") {
            Some(u) => unit = Some(u.trim().to_string()),
            None if !l.is_empty() => text.push(l),
            None => {}
        }
    }
    ((!text.is_empty()).then(|| text.join(" ")), unit)
}

/// Each file of `c`, parsed; `Err` with the problems when one does not
/// parse or there is no `src/lib.rs`.
fn parse<'c>(c: &'c Crate, problems: &mut Vec<String>) -> Result<Vec<(&'c str, syn::File)>, Vec<String>> {
    let mut parsed = Vec::new();
    for (path, text) in c.files {
        match syn::parse_file(text) {
            Ok(f) => parsed.push((path.as_str(), f)),
            Err(e) => problems.push(format!("{path}:{}: {e}", e.span().start().line)),
        }
    }
    if parsed.len() != c.files.len() {
        return Err(std::mem::take(problems));
    }
    if !parsed.iter().any(|(p, _)| *p == LIB) {
        problems.push(format!("{LIB}: no such file"));
        return Err(std::mem::take(problems));
    }
    Ok(parsed)
}

/// A crate's title (the crate documentation's first paragraph) and
/// description (the rest).
fn interface_docs(file: &syn::File, p: &mut Problems) -> (String, Option<String>) {
    let mut paragraphs: Vec<Vec<String>> = vec![Vec::new()];
    for l in doc_lines(&file.attrs) {
        if l.is_empty() {
            paragraphs.push(Vec::new());
        } else {
            paragraphs.last_mut().unwrap().push(l);
        }
    }
    let mut paragraphs = paragraphs.into_iter().filter(|p| !p.is_empty()).map(|p| p.join(" "));
    let title = paragraphs.next().unwrap_or_default();
    if title.is_empty() {
        p.at(1, "the crate's documentation (`//!`) starts with the glossary's title");
    }
    let rest: Vec<String> = paragraphs.collect();
    (title.trim_end_matches('.').to_string(), (!rest.is_empty()).then(|| rest.join("\n\n")))
}

/// The glossary the `glossary` crate declares (docs/scenarios.md): the
/// crate documentation's first paragraph as its title, and the quantities,
/// the fields of `pub struct Quantities`, each documented as a quantity's
/// fields are. The crate has no dependencies and keeps to the portable
/// subset.
pub fn glossary(c: &Crate) -> Result<(Glossary, Vec<String>), Vec<String>> {
    let mut problems = Vec::new();
    manifest(c.cargo_toml, &mut problems);
    let parsed = parse(c, &mut problems)?;
    let lib = &parsed.iter().find(|(p, _)| *p == LIB).expect("parse found it").1;
    let items = Items::of(lib);
    let mut p = Problems { file: LIB, out: &mut problems };
    let (title, _) = interface_docs(lib, &mut p);
    let mut quantities = BTreeMap::new();
    match items.structs.get("Quantities") {
        None => p.at(0, "no `pub struct Quantities`, whose fields are the quantities"),
        Some(s) => {
            if !matches!(s.vis, syn::Visibility::Public(_)) {
                p.at(line(*s), "`Quantities` is pub, for the port");
            }
            match &s.fields {
                syn::Fields::Named(fields) => {
                    for f in &fields.named {
                        let n = f.ident.as_ref().unwrap().to_string();
                        match items.field(&f.ty, &f.attrs, 0) {
                            Ok(field) => {
                                quantities.insert(n, field);
                            }
                            Err(e) => p.at(line(f), &format!("Quantities.{n}: {e}")),
                        }
                    }
                }
                _ => p.at(line(*s), "`Quantities` has named fields"),
            }
        }
    }
    for (path, file) in &parsed {
        let mut v = Subset { problems: Vec::new() };
        v.visit_file(file);
        problems.extend(v.problems.into_iter().map(|(l, e)| format!("{path}:{l}: {e}")));
    }
    let g = Glossary { title, quantities };
    problems.extend(g.check().into_iter().map(|e| format!("{LIB}: {e}")));
    Ok((g, problems))
}

/// The glossary crate's `Quantities` and its package name, checked.
fn glossary_items(c: &Crate) -> Result<(Glossary, String, syn::File), Vec<String>> {
    let (g, problems) = glossary(c)?;
    if !problems.is_empty() {
        return Err(problems);
    }
    let krate = package(c.cargo_toml).map_err(|e| vec![e])?.replace('-', "_");
    let lib = c.files.iter().find(|(p, _)| p == LIB).map(|(_, t)| t).expect("read by glossary");
    let file = syn::parse_file(lib).map_err(|e| vec![format!("{LIB}: {e}")])?;
    Ok((g, krate, file))
}

/// The glue the glossary's binding needs: `Glossary`, a
/// `psx_capture::Quantities` implementation giving the glossary's
/// fingerprint and its `Quantities` in the recording's encoding.
pub fn glossary_glue(c: &Crate) -> Result<String, Vec<String>> {
    let (g, krate, file) = glossary_items(c)?;
    let items = Items::of(&file);
    let ty: syn::Type = syn::parse_str("Quantities").unwrap();
    let body = items.encode(&ty, "(*v)", &krate, Out::Capture, 0).map_err(|e| vec![format!("{LIB}: {e}")])?;
    Ok(format!(
        "// Generated from the glossary's source by psx_scenario::rust::write_glossary_glue: not for editing.\n\
         pub struct Glossary;\n\n\
         #[allow(unused_variables, clippy::all)]\n\
         impl psx_capture::Quantities for Glossary {{\n    \
         const FINGERPRINT: &'static str = {:?};\n    \
         type Quantities = {krate}::Quantities;\n    \
         fn quantities(v: &Self::Quantities) -> psx_capture::Value {{\n        {body}\n    }}\n}}\n",
        g.fingerprint(),
    ))
}

/// For the glossary binding's `build.rs`: writes the [`glossary_glue`] for
/// the glossary crate at `glossary_dir` to `$OUT_DIR/glue.rs`.
pub fn write_glossary_glue(glossary_dir: impl AsRef<std::path::Path>) {
    let dir = glossary_dir.as_ref();
    let (cargo_toml, files) = read_crate(dir);
    match glossary_glue(&Crate { cargo_toml: &cargo_toml, files: &files }) {
        Ok(code) => write_out("glue.rs", &code),
        Err(problems) => panic!("the glossary at {} cannot be read:\n{}", dir.display(), problems.join("\n")),
    }
}

/// The port's view of the glossary (docs/scenarios.md): `Observed`, each
/// quantity as an `Option` of its type in the glossary crate, `None` where
/// the port has no such thing, with `Observed::none()` and its values in
/// the recording's encoding for `psx_bevy::scenario`.
pub fn observed(c: &Crate) -> Result<String, Vec<String>> {
    let (_, krate, file) = glossary_items(c)?;
    let items = Items::of(&file);
    let s = items.structs["Quantities"];
    let syn::Fields::Named(named) = &s.fields else { unreachable!("checked by glossary") };
    let (mut fields, mut nones, mut values) = (Vec::new(), Vec::new(), Vec::new());
    for f in &named.named {
        let n = f.ident.as_ref().unwrap().to_string();
        let docs: String = doc_lines(&f.attrs).iter().map(|l| format!("    /// {l}\n")).collect();
        fields.push(format!("{docs}    pub {n}: Option<{}>,", qualified(&f.ty, &items, &krate)));
        nones.push(format!("            {n}: None,"));
        let e = items.encode(&f.ty, "(*v)", &krate, Out::Recording, 0).map_err(|e| vec![format!("{LIB}: {n}: {e}")])?;
        values.push(format!("            ({n:?}, self.{n}.as_ref().map(|v| {e})),"));
    }
    Ok(format!(
        "// Generated from the glossary's source by psx_scenario::rust::write_observed: not for editing.\n\n\
         /// The glossary's quantities as the port has them, in the original's\n\
         /// raw units: `None` where the port has no such thing (yet).\n\
         #[derive(Default)]\n\
         pub struct Observed {{\n{}\n}}\n\n\
         impl Observed {{\n    \
         /// Nothing observed: the start of `Observed {{ player_x: …, ..Observed::none() }}`.\n    \
         pub fn none() -> Observed {{\n        Observed {{\n{}\n        }}\n    }}\n}}\n\n\
         #[allow(unused_variables, unused_parens, clippy::all)]\n\
         impl psx_bevy::scenario::Observation for Observed {{\n    \
         fn values(&self) -> Vec<(&'static str, Option<psx_scenario::Value>)> {{\n        vec![\n{}\n        ]\n    }}\n}}\n",
        fields.join("\n"),
        nones.join("\n"),
        values.join("\n"),
    ))
}

/// For the port's `build.rs`: writes [`observed`] for the glossary crate at
/// `glossary_dir` to `$OUT_DIR/observed.rs`.
pub fn write_observed(glossary_dir: impl AsRef<std::path::Path>) {
    let dir = glossary_dir.as_ref();
    let (cargo_toml, files) = read_crate(dir);
    match observed(&Crate { cargo_toml: &cargo_toml, files: &files }) {
        Ok(code) => write_out("observed.rs", &code),
        Err(problems) => panic!("the glossary at {} cannot be read:\n{}", dir.display(), problems.join("\n")),
    }
}

/// `ty` as code outside its crate: the crate's own types through `krate`.
fn qualified(ty: &syn::Type, items: &Items, krate: &str) -> String {
    match ty {
        syn::Type::Paren(p) => qualified(&p.elem, items, krate),
        syn::Type::Path(p) if p.qself.is_none() && p.path.segments.len() == 1 => {
            let seg = &p.path.segments[0];
            let name = seg.ident.to_string();
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                if let Some(syn::GenericArgument::Type(of)) = args.args.first() {
                    return format!("{name}<{}>", qualified(of, items, krate));
                }
            }
            let own = items.structs.contains_key(&name)
                || items.enums.contains_key(&name)
                || items.aliases.contains_key(&name);
            if own { format!("{krate}::{name}") } else { name }
        }
        syn::Type::Array(a) => {
            let len = items.len(&a.len).map(|n| n.to_string()).unwrap_or_default();
            format!("[{}; {len}]", qualified(&a.elem, items, krate))
        }
        other => quote::ToTokens::to_token_stream(other).to_string(),
    }
}

/// The portable subset: what the glossary's code may not use, so it can be
/// read on its own and translated into any language.
struct Subset {
    problems: Vec<(usize, String)>,
}

impl Subset {
    fn no(&mut self, at: usize, what: &str) {
        self.problems.push((at, what.to_string()));
    }

    fn generics(&mut self, g: &syn::Generics, what: &str, lifetimes: bool) {
        for param in &g.params {
            match param {
                syn::GenericParam::Lifetime(_) if lifetimes => {}
                _ => self.no(line(param), &format!("{what}: no generic parameters (write the type out)")),
            }
        }
    }
}

impl<'ast> Visit<'ast> for Subset {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if f.sig.unsafety.is_some() {
            self.no(line(&f.sig), "no unsafe code");
        }
        self.generics(&f.sig.generics, &format!("fn {}", f.sig.ident), true);
        syn::visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if f.sig.unsafety.is_some() {
            self.no(line(&f.sig), "no unsafe code");
        }
        self.generics(&f.sig.generics, &format!("fn {}", f.sig.ident), true);
        syn::visit::visit_impl_item_fn(self, f);
    }

    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        self.generics(&s.generics, &format!("struct {}", s.ident), false);
        syn::visit::visit_item_struct(self, s);
    }

    fn visit_item_enum(&mut self, e: &'ast syn::ItemEnum) {
        self.generics(&e.generics, &format!("enum {}", e.ident), false);
        syn::visit::visit_item_enum(self, e);
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        if t.unsafety.is_some() {
            self.no(line(t), "no unsafe code");
        }
        self.generics(&t.generics, &format!("trait {}", t.ident), false);
        syn::visit::visit_item_trait(self, t);
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if i.unsafety.is_some() {
            self.no(line(i), "no unsafe code");
        }
        self.generics(&i.generics, "impl", false);
        syn::visit::visit_item_impl(self, i);
    }

    fn visit_expr_unsafe(&mut self, e: &'ast syn::ExprUnsafe) {
        self.no(line(e), "no unsafe code");
        syn::visit::visit_expr_unsafe(self, e);
    }

    fn visit_item_foreign_mod(&mut self, i: &'ast syn::ItemForeignMod) {
        self.no(line(i), "no foreign functions");
    }

    fn visit_item_extern_crate(&mut self, i: &'ast syn::ItemExternCrate) {
        self.no(line(i), "no extern crate: the glossary depends on nothing");
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if !matches!(s.mutability, syn::StaticMutability::None) {
            self.no(line(s), "no static mut (use const for tables)");
        }
        syn::visit::visit_item_static(self, s);
    }

    fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
        let name = m.mac.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
        self.no(line(m), &format!("no macro definitions or item macros ({name}!): write the code out"));
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let name = m.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
        if !MACROS.contains(&name.as_str()) {
            self.no(line(m), &format!("{name}!: the glossary calls only {}", MACROS.join("!, ") + "!"));
        }
    }

    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        if a.path().is_ident("derive") {
            let _ = a.parse_nested_meta(|meta| {
                let name = meta.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
                if !DERIVES.contains(&name.as_str()) {
                    self.no(
                        line(&meta.path),
                        &format!("derive({name}): the glossary derives only {}", DERIVES.join(", ")),
                    );
                }
                Ok(())
            });
        }
    }

    fn visit_type_trait_object(&mut self, t: &'ast syn::TypeTraitObject) {
        self.no(line(t), "no trait objects (dyn): call the type itself");
        syn::visit::visit_type_trait_object(self, t);
    }

    fn visit_path_segment(&mut self, s: &'ast syn::PathSegment) {
        let name = s.ident.to_string();
        if TYPES.contains(&name.as_str()) {
            self.no(line(s), &format!("{name}: the glossary keeps to Vec, arrays and its own structs"));
        }
        syn::visit::visit_path_segment(self, s);
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        let mut roots = Vec::new();
        use_roots(&u.tree, &mut roots);
        for (at, root) in roots {
            if !["std", "core", "alloc", "crate", "self", "super"].contains(&root.as_str()) {
                self.no(at, &format!("use {root}: not the standard library or this crate"));
            }
        }
        syn::visit::visit_item_use(self, u);
    }
}

fn use_roots(t: &syn::UseTree, out: &mut Vec<(usize, String)>) {
    match t {
        syn::UseTree::Path(p) => out.push((line(&p.ident), p.ident.to_string())),
        syn::UseTree::Name(n) => out.push((line(&n.ident), n.ident.to_string())),
        syn::UseTree::Rename(r) => out.push((line(&r.ident), r.ident.to_string())),
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| use_roots(t, out)),
        syn::UseTree::Glob(g) => out.push((line(g), "*".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = "[package]\nname = \"glossary\"\n";

    const GLOSSARY: &str = r#"//! Croc (Europe): the quantities scenarios compare.

/// Fixed point: 20 integer bits, 12 fraction bits.
pub type Fx20_12 = i32;

pub enum Mode {
    Idle,
    Walk,
}

pub struct Quantities {
    /// Croc's position across the level
    /// Unit: world units
    pub croc_x: Fx20_12,
    pub croc_mode: Mode,
}
"#;

    fn read(cargo: &str, lib: &str) -> Result<(Glossary, Vec<String>), Vec<String>> {
        glossary(&Crate { cargo_toml: cargo, files: &[(LIB.into(), lib.into())] })
    }

    #[test]
    fn the_glue_and_the_ports_view_encode_each_quantity() {
        let files = [(LIB.to_string(), GLOSSARY.to_string())];
        let c = Crate { cargo_toml: CARGO, files: &files };
        let g = glossary_glue(&c).unwrap();
        assert!(g.contains("impl psx_capture::Quantities for Glossary"), "{g}");
        assert!(g.contains("(\"croc_x\", psx_capture::Value::Int((*v).croc_x as i128))"), "{g}");
        assert!(g.contains("glossary::Mode::Walk => psx_capture::Value::Variant(\"walk\")"), "{g}");
        let o = observed(&c).unwrap();
        assert!(o.contains("pub croc_x: Option<glossary::Fx20_12>,"), "{o}");
        assert!(o.contains("pub croc_mode: Option<glossary::Mode>,"), "{o}");
        assert!(
            o.contains("(\"croc_x\", self.croc_x.as_ref().map(|v| psx_scenario::Value::Int((*v) as i128))),"),
            "{o}"
        );
    }

    #[test]
    fn the_portable_subset_is_held() {
        let lib = GLOSSARY.to_string()
            + r#"
use serde::Serialize;
static mut COUNT: u32 = 0;
macro_rules! twice { ($e:expr) => { $e * 2 } }
pub struct Bag<T> { pub items: Vec<T> }
#[derive(Clone, Serialize)]
pub struct Cache { pub seen: std::collections::HashMap<u32, u32> }
pub fn pick(x: &dyn Fn() -> u32) -> u32 { let y = format!("{}", 1); unsafe { 0 } }
"#;
        let cargo = CARGO.to_string() + "[dependencies]\nserde = \"1\"\n";
        let (_, problems) = read(&cargo, &lib).unwrap();
        let has = |s: &str| problems.iter().any(|p| p.contains(s));
        assert!(has("Cargo.toml: the glossary depends on nothing"), "{problems:#?}");
        assert!(has("use serde: not the standard library"), "{problems:#?}");
        assert!(has("no static mut"), "{problems:#?}");
        assert!(has("no macro definitions"), "{problems:#?}");
        assert!(has("struct Bag: no generic parameters"), "{problems:#?}");
        assert!(has("derive(Serialize)"), "{problems:#?}");
        assert!(has("HashMap: the glossary keeps to Vec"), "{problems:#?}");
        assert!(has("no trait objects"), "{problems:#?}");
        assert!(has("format!: the glossary calls only"), "{problems:#?}");
        assert!(has("no unsafe code"), "{problems:#?}");
        assert!(problems.iter().all(|p| p.starts_with("src/lib.rs:") || p.starts_with("Cargo.toml:")), "{problems:#?}");
    }

    #[test]
    fn quantities_it_cannot_record_are_named() {
        let lib = GLOSSARY
            .replace("pub croc_mode: Mode,", "pub croc_mode: Mode,\n    pub name: String,\n    pub odd: Fx8_8,")
            + "pub type Fx8_8 = i32;\n";
        let (_, problems) = read(CARGO, &lib).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("Quantities.name: `String` is not a type a recording can hold")),
            "{problems:#?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("Quantities.odd: `Fx8_8` is 8.8 fixed point, signed: alias i16")),
            "{problems:#?}"
        );
        let problems = read(CARGO, "pub fn (").unwrap_err();
        assert!(problems[0].starts_with("src/lib.rs:1: "), "{problems:#?}");
    }
}

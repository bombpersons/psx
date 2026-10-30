//! Mechanics written as Rust (docs/rust-mechanics-plan.md, section 3): a
//! crate of plain, dependency-free Rust whose header constants, crate
//! documentation and `State`, `Inputs` and `Params` structs declare what
//! `mechanic.toml` declares. It is read from source, without building it,
//! into the same [`Mechanic`], and held to a portable subset of Rust.

use std::collections::{BTreeMap, BTreeSet};

use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::mechanic::{Exactness, Mechanic, Per, Rate};
use crate::value::{Field, Kind, Value, is_id};

/// The crate's root file, relative to the crate.
pub const LIB: &str = "src/lib.rs";

/// Macros a mechanic may call: the standard library's checks and `vec!`.
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

/// Derives a mechanic may use: the standard library's own.
const DERIVES: &[&str] = &["Clone", "Copy", "Debug", "Default", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash"];

/// Types a mechanic may not use: maps and sets with an order of their own,
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

/// The header's constants.
const HEADER: &[&str] = &["ID", "VERSION", "RATE", "RATE_HZ", "EXACTNESS", "TOLERANCE", "REQUIRES", "USES"];

/// A mechanic crate's files: `Cargo.toml`, and each `.rs` file under `src/`
/// by its path relative to the crate (`src/lib.rs` among them).
pub struct Crate<'a> {
    pub cargo_toml: &'a str,
    pub files: &'a [(String, String)],
}

/// The mechanic the crate declares, and what is wrong with it (`<file>:<line>:
/// <problem>`, files relative to the crate). `Err` when no mechanic can be
/// read at all: a file that does not parse, or no `ID` or `VERSION`.
pub fn mechanic(c: &Crate) -> Result<(Mechanic, Vec<String>), Vec<String>> {
    let mut problems = Vec::new();
    let deps = manifest(c.cargo_toml, &mut problems);
    let mut parsed = Vec::new();
    for (path, text) in c.files {
        match syn::parse_file(text) {
            Ok(f) => parsed.push((path.as_str(), f)),
            Err(e) => problems.push(format!("{path}:{}: {e}", e.span().start().line)),
        }
    }
    if parsed.len() != c.files.len() {
        return Err(problems);
    }
    let Some((_, lib)) = parsed.iter().find(|(p, _)| *p == LIB) else {
        problems.push(format!("{LIB}: no such file"));
        return Err(problems);
    };
    let items = Items::of(lib);
    let mut p = Problems { file: LIB, out: &mut problems };
    let consts = header(&items, &mut p);
    let (Some(id), Some(version)) = (consts.id.clone(), consts.version) else {
        if consts.id.is_none() {
            p.at(0, "no `pub const ID: &str` (the mechanic's id, `<game>.<name>`)");
        }
        if consts.version.is_none() {
            p.at(0, "no `pub const VERSION: u32`");
        }
        return Err(problems);
    };
    let (title, summary) = crate_docs(lib, &mut p);
    let mut m = Mechanic {
        id,
        version,
        title,
        summary,
        exactness: consts.exactness,
        rate: consts.rate,
        requires: consts.requires,
        uses: consts.uses,
        state: BTreeMap::new(),
        inputs: BTreeMap::new(),
        params: BTreeMap::new(),
        tolerance: consts.tolerance,
    };
    for (name, into) in [("State", &mut m.state), ("Inputs", &mut m.inputs), ("Params", &mut m.params)] {
        let Some(s) = items.structs.get(name) else { continue };
        if !matches!(s.vis, syn::Visibility::Public(_)) {
            p.at(line(s), &format!("`{name}` is pub, for the port and the replay"));
        }
        let syn::Fields::Named(fields) = &s.fields else {
            p.at(line(s), &format!("`{name}` has named fields"));
            continue;
        };
        for f in &fields.named {
            let n = f.ident.as_ref().unwrap().to_string();
            match items.field(&f.ty, &f.attrs, 0) {
                Ok(field) => {
                    into.insert(n, field);
                }
                Err(e) => p.at(line(f), &format!("{name}.{n}: {e}")),
            }
        }
    }
    if let Err(e) = step_signature(lib) {
        problems.push(format!("{LIB}:{e}"));
    }
    let floats_declared = [&m.state, &m.inputs, &m.params].iter().any(|fs| fs.values().any(has_float));
    for (path, file) in &parsed {
        let mut v = Subset { deps: &deps, problems: Vec::new(), floats: Vec::new() };
        v.visit_file(file);
        problems.extend(v.problems.into_iter().map(|(l, e)| format!("{path}:{l}: {e}")));
        if m.exactness == Exactness::Exact && !floats_declared {
            problems.extend(v.floats.into_iter().map(|l| {
                format!("{path}:{l}: a float in an exact mechanic whose fields are all integers: compute as the original does")
            }));
        }
    }
    Ok((m, problems))
}

fn has_float(f: &Field) -> bool {
    f.kind == Kind::Float
        || f.of.as_deref().is_some_and(has_float)
        || f.fields.as_ref().is_some_and(|fs| fs.values().any(has_float))
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

/// The crate's dependencies, checked: each a workspace mechanic
/// (`path = "../<id>"`) or interface (`path = "../../interfaces/<id>"`).
/// Returns their crate names as `use` sees them.
fn manifest(text: &str, p: &mut Vec<String>) -> BTreeSet<String> {
    let mut deps = BTreeSet::new();
    let toml: toml::Table = match toml::from_str(text) {
        Ok(t) => t,
        Err(e) => {
            p.push(format!("Cargo.toml: {}", e.message()));
            return deps;
        }
    };
    let package = toml.get("package").and_then(|v| v.as_table());
    if package.is_some_and(|t| t.contains_key("build")) {
        p.push("Cargo.toml: a mechanic has no build script".into());
    }
    if toml.get("lib").and_then(|l| l.get("proc-macro")).is_some() {
        p.push("Cargo.toml: a mechanic is not a procedural macro crate".into());
    }
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        for (name, d) in toml.get(key).and_then(|v| v.as_table()).into_iter().flatten() {
            let path = d.get("path").and_then(|v| v.as_str());
            let ok = path.is_some_and(|path| {
                let id = path.strip_prefix("../../interfaces/").or_else(|| path.strip_prefix("../"));
                id.is_some_and(|id| {
                    is_id(id) && d.as_table().is_some_and(|t| t.keys().all(|k| k == "path" || k == "package"))
                })
            });
            if ok {
                deps.insert(name.replace('-', "_"));
            } else {
                p.push(format!(
                    "Cargo.toml: {key}.{name}: a mechanic depends only on the workspace's mechanics (path = \"../<id>\") and interfaces (path = \"../../interfaces/<id>\")"
                ));
            }
        }
    }
    for key in toml.keys().filter(|k| k.starts_with("target")) {
        p.push(format!("Cargo.toml: {key}: a mechanic has no platform-specific dependencies"));
    }
    deps
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
                "`{shown}` is not a type a trace can hold: bool, u8 to u64, i8 to i64, f32, f64, a fixed-point alias (`type Fx16_16 = i32;`), an enum, a struct, `[T; N]` or `Vec<T>`"
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
    /// reads it), in the trace's encoding, as `out`'s `Value`. Types are
    /// named through `krate`, the mechanic's crate.
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

    /// Code giving a `Result<T, String>` for `ty` read from `place`, a
    /// `&psx_mechanics::Value` in the trace's encoding, naming the field or
    /// element at fault. Types are named through `krate`.
    fn decode(&self, ty: &syn::Type, place: &str, krate: &str, depth: u32) -> Result<String, String> {
        const R: &str = "psx_mechanics::replay";
        if depth > 16 {
            return Err("types nest more than 16 deep".into());
        }
        let shown = quote::ToTokens::to_token_stream(ty).to_string();
        let each = |of: &syn::Type| -> Result<String, String> {
            let (e, i) = (format!("e{depth}"), format!("i{depth}"));
            let inner = self.decode(of, &e, krate, depth + 1)?;
            Ok(format!(
                "{R}::list({place}).and_then(|l| l.iter().enumerate().map(|({i}, {e})| {inner}.map_err({R}::index({i}))).collect::<Result<Vec<_>, String>>())"
            ))
        };
        match ty {
            syn::Type::Array(a) => Ok(format!("{}.and_then({R}::array)", each(&a.elem)?)),
            syn::Type::Paren(p) => self.decode(&p.elem, place, krate, depth),
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
                match primitive(&name) {
                    Some(f) if f.kind == Kind::Bool => return Ok(format!("{R}::boolean({place})")),
                    Some(f) if f.kind == Kind::Float => {
                        let bits = if f.bits == Some(32) { "u32" } else { "u64" };
                        return Ok(format!("{R}::int::<{bits}>({place}).map({name}::from_bits)"));
                    }
                    Some(_) => return Ok(format!("{R}::int::<{name}>({place})")),
                    None => {}
                }
                if let Some(a) = self.aliases.get(&name) {
                    return self.decode(&a.ty, place, krate, depth + 1);
                }
                if let Some(e) = self.enums.get(&name) {
                    let arms: Vec<String> = e
                        .variants
                        .iter()
                        .map(|v| format!("{:?} => Ok({krate}::{name}::{}),", snake(&v.ident.to_string()), v.ident))
                        .collect();
                    return Ok(format!(
                        "{R}::variant({place}).and_then(|n| match n {{ {} n => {R}::unknown(n) }})",
                        arms.join(" ")
                    ));
                }
                if self.structs.contains_key(&name) {
                    let r = format!("r{depth}");
                    let body = self.decode_struct(&name, &r, krate, depth + 1)?;
                    return Ok(format!("{R}::record({place}).and_then(|{r}| {body})"));
                }
                Err(format!("`{shown}`"))
            }
            _ => Err(format!("`{shown}`")),
        }
    }

    /// Code giving a `Result` of the struct `name` from `r`, its fields (a
    /// `&psx_mechanics::Values`).
    fn decode_struct(&self, name: &str, r: &str, krate: &str, depth: u32) -> Result<String, String> {
        const R: &str = "psx_mechanics::replay";
        let s = self.structs[name];
        let syn::Fields::Named(named) = &s.fields else { return Err(format!("`{name}` has no named fields")) };
        let (mut names, mut fields) = (Vec::new(), Vec::new());
        for f in &named.named {
            let n = f.ident.as_ref().unwrap().to_string();
            let v = format!("v{depth}");
            let inner = self.decode(&f.ty, &v, krate, depth + 1)?;
            names.push(format!("{n:?}"));
            fields.push(format!("{n}: {{ let {v} = {R}::field({r}, {n:?}); {inner}.map_err({R}::at({n:?}))? }},"));
        }
        Ok(format!(
            "{R}::fields({r}, &[{}]).and_then(|{r}| Ok({krate}::{name} {{ {} }}))",
            names.join(", "),
            fields.join(" ")
        ))
    }
}

/// Which `Value` generated code builds: `psx_capture`'s, which a binding
/// answers psx with, or `psx_mechanics`', which replay compares.
#[derive(Clone, Copy)]
enum Out {
    Capture,
    Trace,
}

impl Out {
    fn value(self) -> &'static str {
        match self {
            Out::Capture => "psx_capture::Value",
            Out::Trace => "psx_mechanics::Value",
        }
    }

    fn variant(self, name: &str) -> String {
        match self {
            Out::Capture => format!("psx_capture::Value::Variant({name:?})"),
            Out::Trace => format!("psx_mechanics::Value::Variant({name:?}.into())"),
        }
    }

    fn record(self, fields: &[String]) -> String {
        match self {
            Out::Capture => format!("psx_capture::Value::Record(vec![{}])", fields.join(", ")),
            Out::Trace => format!("psx_mechanics::replay::make_record([{}])", fields.join(", ")),
        }
    }
}

/// What `step` takes, in order: `"State"`, `"Inputs"`, `"Params"` or
/// `"Calls"` (`&mut impl` the trait of its interfaces and used mechanics).
/// `Ok(None)` when the crate has no `pub fn step`.
fn step_signature(file: &syn::File) -> Result<Option<Vec<&'static str>>, String> {
    let Some(f) = file.items.iter().find_map(|i| match i {
        syn::Item::Fn(f) if f.sig.ident == "step" => Some(f),
        _ => None,
    }) else {
        return Ok(None);
    };
    let wrong = || {
        format!(
            "{}: `step` takes `&mut State`, then `&Inputs`, `&Params` and `&mut impl` its calls' trait as needed, and returns nothing",
            line(&f.sig)
        )
    };
    if !matches!(f.vis, syn::Visibility::Public(_)) || !matches!(f.sig.output, syn::ReturnType::Default) {
        return Err(wrong());
    }
    let mut takes = Vec::new();
    for (i, arg) in f.sig.inputs.iter().enumerate() {
        let syn::FnArg::Typed(t) = arg else { return Err(wrong()) };
        let syn::Type::Reference(r) = &*t.ty else { return Err(wrong()) };
        let name = quote::ToTokens::to_token_stream(&r.elem).to_string();
        let calls = matches!(&*r.elem, syn::Type::ImplTrait(_));
        let want = match (i, name.as_str(), r.mutability.is_some()) {
            _ if takes.contains(&"Calls") => return Err(wrong()),
            (0, "State", true) => "State",
            (_, "Inputs", false) if !takes.contains(&"Inputs") && !takes.contains(&"Params") => "Inputs",
            (_, "Params", false) if !takes.contains(&"Params") => "Params",
            (1.., _, true) if calls => "Calls",
            _ => return Err(wrong()),
        };
        takes.push(want);
    }
    if takes.first() != Some(&"State") {
        return Err(wrong());
    }
    Ok(Some(takes))
}

/// The glue a binding needs for the mechanic `c` declares: a
/// `psx_capture::Mechanic` implementation, `Mechanic`, giving its id,
/// version and fingerprint and its `State`, `Inputs` and `Params` in the
/// trace's encoding. Its types are named through the crate the package is
/// (`croc-turn` is `croc_turn`).
pub fn glue(c: &Crate) -> Result<String, Vec<String>> {
    let (m, _) = mechanic(c)?;
    let krate = package(c.cargo_toml).map_err(|e| vec![e])?.replace('-', "_");
    let lib = c.files.iter().find(|(p, _)| p == LIB).map(|(_, t)| t).expect("read by mechanic");
    let file = syn::parse_file(lib).map_err(|e| vec![format!("{LIB}: {e}")])?;
    let items = Items::of(&file);
    let mut types = Vec::new();
    let mut fns = Vec::new();
    for (name, f) in [("State", "state"), ("Inputs", "inputs"), ("Params", "params")] {
        let (ty, body) = match items.structs.get(name) {
            None => ("()".to_string(), "psx_capture::Value::Record(Vec::new())".to_string()),
            Some(_) => {
                let path: syn::Type = syn::parse_str(name).unwrap();
                let body = items
                    .encode(&path, "(*v)", &krate, Out::Capture, 0)
                    .map_err(|e| vec![format!("{LIB}: {name}: {e}")])?;
                (format!("{krate}::{name}"), body)
            }
        };
        types.push(format!("    type {name} = {ty};"));
        fns.push(format!("    fn {f}(v: &Self::{name}) -> psx_capture::Value {{\n        {body}\n    }}"));
    }
    Ok(format!(
        "// Generated from the mechanic's source by psx_mechanics::rust::write_glue: not for editing.\n\
         pub struct Mechanic;\n\n\
         #[allow(unused_variables)]\n\
         impl psx_capture::Mechanic for Mechanic {{\n    \
         const ID: &'static str = {:?};\n    \
         const VERSION: u32 = {};\n    \
         const FINGERPRINT: &'static str = {:?};\n{}\n{}\n}}\n",
        m.id,
        m.version,
        m.fingerprint(),
        types.join("\n"),
        fns.join("\n"),
    ))
}

/// For a binding's `build.rs`: writes the [`glue`] for the mechanic crate
/// at `mechanic_dir` to `$OUT_DIR/glue.rs`, and has cargo run it again when
/// the mechanic changes. A mechanic it cannot read fails the build.
pub fn write_glue(mechanic_dir: impl AsRef<std::path::Path>) {
    let dir = mechanic_dir.as_ref();
    let (cargo_toml, files) = read_crate(dir);
    match glue(&Crate { cargo_toml: &cargo_toml, files: &files }) {
        Ok(code) => write_out("glue.rs", &code),
        Err(problems) => panic!("the mechanic at {} cannot be read:\n{}", dir.display(), problems.join("\n")),
    }
}

/// The replay crate's code for the mechanic `c` declares: a module named
/// after its crate holding `Mechanic`, a `psx_mechanics::replay::Mechanic`
/// implementation that converts between its structs and the trace's values
/// and calls its `step`. Types are named through the crate the package is.
pub fn replay_glue(c: &Crate) -> Result<String, Vec<String>> {
    const R: &str = "psx_mechanics::replay";
    let (m, _) = mechanic(c)?;
    let krate = package(c.cargo_toml).map_err(|e| vec![e])?.replace('-', "_");
    let lib = c.files.iter().find(|(p, _)| p == LIB).map(|(_, t)| t).expect("read by mechanic");
    let file = syn::parse_file(lib).map_err(|e| vec![format!("{LIB}: {e}")])?;
    let takes = match step_signature(&file) {
        Ok(Some(t)) => t,
        Ok(None) => return Err(vec![format!("{LIB}: no `pub fn step(state: &mut State, ...)`")]),
        Err(e) => return Err(vec![format!("{LIB}:{e}")]),
    };
    if takes.contains(&"Calls") {
        return Err(vec![format!("{LIB}: `step` makes calls, and calls are not replayed yet")]);
    }
    let items = Items::of(&file);
    let path = format!("::{krate}");
    let mut fns = Vec::new();
    let mut types = Vec::new();
    for (name, f) in [("State", "state"), ("Inputs", "inputs"), ("Params", "params")] {
        let (ty, body) = match items.structs.get(name) {
            None => ("()".to_string(), format!("{R}::fields(v, &[]).map(|_| ())")),
            Some(_) => {
                let body = items.decode_struct(name, "v", &path, 0).map_err(|e| vec![format!("{LIB}: {name}: {e}")])?;
                (format!("{path}::{name}"), body)
            }
        };
        types.push(format!("    type {name} = {ty};"));
        fns.push(format!(
            "    fn {f}(v: &psx_mechanics::Values) -> Result<Self::{name}, String> {{\n        {body}\n    }}"
        ));
    }
    let record = match items.structs.get("State") {
        None => "psx_mechanics::Values::new()".to_string(),
        Some(_) => {
            let ty: syn::Type = syn::parse_str("State").unwrap();
            let e = items.encode(&ty, "(*v)", &path, Out::Trace, 0).map_err(|e| vec![format!("{LIB}: State: {e}")])?;
            format!("{R}::fields_of({e})")
        }
    };
    let args: Vec<&str> = takes
        .iter()
        .map(|t| match *t {
            "State" => "state",
            "Inputs" => "inputs",
            _ => "params",
        })
        .collect();
    let tolerance = match m.exactness {
        Exactness::Exact => "None".to_string(),
        Exactness::Faithful => {
            let each: Vec<String> = m
                .tolerance
                .iter()
                .filter_map(|(k, v)| match v {
                    crate::Value::Int(n) => Some(format!("({k:?}, {n})")),
                    _ => None,
                })
                .collect();
            format!("Some(&[{}])", each.join(", "))
        }
    };
    Ok(format!(
        "pub mod {krate} {{\n\
         pub struct Mechanic;\n\n\
         #[allow(unused_variables, clippy::all)]\n\
         impl {R}::Mechanic for Mechanic {{\n    \
         const ID: &'static str = {:?};\n    \
         const VERSION: u32 = {};\n    \
         const FINGERPRINT: &'static str = {:?};\n    \
         const TOLERANCE: Option<&'static [(&'static str, i128)]> = {tolerance};\n\
         {}\n{}\n    \
         fn record(v: &Self::State) -> psx_mechanics::Values {{\n        {record}\n    }}\n    \
         fn step(state: &mut Self::State, inputs: &Self::Inputs, params: &Self::Params) {{\n        {path}::step({});\n    }}\n\
         }}\n}}\n",
        m.id,
        m.version,
        m.fingerprint(),
        types.join("\n"),
        fns.join("\n"),
        args.join(", "),
    ))
}

/// For the replay crate's `build.rs`: writes to `$OUT_DIR/replay.rs` the
/// [`replay_glue`] of each mechanic written as Rust under
/// `<workspace>/mechanics/`, and `mechanics()`, their
/// `psx_mechanics::replay::Entry`s. A mechanic that cannot be replayed (it
/// does not read, has no step, or is not a dependency of the replay crate)
/// gets an entry that fails, so the others still replay.
pub fn write_replay(workspace: impl AsRef<std::path::Path>) {
    let root = workspace.as_ref().join("mechanics");
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("run from build.rs"));
    println!("cargo::rerun-if-changed={}", manifest.join("Cargo.toml").display());
    println!("cargo::rerun-if-changed={}", root.display());
    let replay_toml = std::fs::read_to_string(manifest.join("Cargo.toml")).unwrap_or_default();
    let deps: BTreeSet<String> = toml::from_str::<toml::Table>(&replay_toml)
        .ok()
        .and_then(|t| t.get("dependencies")?.as_table().map(|d| d.keys().cloned().collect()))
        .unwrap_or_default();
    let mut dirs: Vec<_> = std::fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()).collect();
    dirs.sort();
    let (mut modules, mut entries) = (String::new(), Vec::new());
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let at = format!("mechanics/{name}");
        let entry =
            |replayer: String| format!("        psx_mechanics::replay::Entry {{ dir: {at:?}, replay: {replayer} }},");
        let fails = |why: String| entry(format!("psx_mechanics::replay::Replayer::Fails({why:?})"));
        if !dir.join("Cargo.toml").is_file() {
            if dir.join("mechanic.toml").is_file() {
                entries.push(entry(format!(
                    "psx_mechanics::replay::Replayer::Skipped({:?})",
                    "written as mechanic.toml, which has no step"
                )));
            }
            continue;
        }
        let (cargo_toml, files) = read_crate(dir);
        let package = match package(&cargo_toml) {
            Ok(p) => p,
            Err(e) => {
                entries.push(fails(e));
                continue;
            }
        };
        if !deps.contains(&package) {
            entries.push(fails(format!(
                "not a dependency of the replay crate: add `{package} = {{ path = \"../mechanics/{name}\" }}` to replay/Cargo.toml"
            )));
            continue;
        }
        match replay_glue(&Crate { cargo_toml: &cargo_toml, files: &files }) {
            Ok(code) => {
                modules.push_str(&code);
                modules.push('\n');
                entries.push(entry(format!(
                    "psx_mechanics::replay::Replayer::Run(psx_mechanics::replay::trace::<{}::Mechanic>)",
                    package.replace('-', "_")
                )));
            }
            Err(problems) => entries.push(fails(problems.join("; "))),
        }
    }
    write_out(
        "replay.rs",
        &format!(
            "// Generated from the mechanics' source by psx_mechanics::rust::write_replay: not for editing.\n\n\
             {modules}\
             /// The workspace's mechanics, as this crate was built.\n\
             pub fn mechanics() -> Vec<psx_mechanics::replay::Entry> {{\n    vec![\n{}\n    ]\n}}\n",
            entries.join("\n")
        ),
    );
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
    toml::from_str::<toml::Table>(cargo_toml)
        .ok()
        .and_then(|t| t.get("package")?.get("name")?.as_str().map(String::from))
        .ok_or_else(|| "Cargo.toml: no package name".to_string())
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

/// The title (the crate documentation's first paragraph) and summary (its
/// second).
fn crate_docs(file: &syn::File, p: &mut Problems) -> (String, String) {
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
    let summary = paragraphs.next().unwrap_or_default();
    if title.is_empty() {
        p.at(1, "the crate's documentation (`//!`) starts with the mechanic's title");
    }
    if summary.is_empty() {
        p.at(1, "the crate's documentation's second paragraph is the mechanic's summary");
    }
    (title.trim_end_matches('.').to_string(), summary)
}

struct Header {
    id: Option<String>,
    version: Option<u32>,
    rate: Rate,
    exactness: Exactness,
    requires: Vec<String>,
    uses: Vec<String>,
    tolerance: BTreeMap<String, Value>,
}

fn header(items: &Items, p: &mut Problems) -> Header {
    let mut h = Header {
        id: None,
        version: None,
        rate: Rate { per: Per::Tick, hz: None },
        exactness: Exactness::Exact,
        requires: Vec::new(),
        uses: Vec::new(),
        tolerance: BTreeMap::new(),
    };
    for name in HEADER {
        let Some(c) = items.consts.get(*name) else { continue };
        let at = line(*c);
        if !matches!(c.vis, syn::Visibility::Public(_)) {
            p.at(at, &format!("`{name}` is pub"));
        }
        let e = &*c.expr;
        let bad = |p: &mut Problems, what: &str| p.at(at, &format!("`{name}` is {what}"));
        match *name {
            "ID" => match string(e) {
                Some(s) => h.id = Some(s),
                None => bad(p, "a string literal"),
            },
            "VERSION" => match int(e) {
                Some(v) => h.version = Some(v as u32),
                None => bad(p, "an integer literal"),
            },
            "RATE" => match string(e).as_deref() {
                Some("tick") => h.rate.per = Per::Tick,
                Some("vblank") => h.rate.per = Per::Vblank,
                Some("event") => h.rate.per = Per::Event,
                _ => bad(p, "\"tick\", \"vblank\" or \"event\""),
            },
            "RATE_HZ" => match e {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Float(f), .. }) => h.rate.hz = f.base10_parse().ok(),
                _ => match int(e) {
                    Some(n) => h.rate.hz = Some(n as f64),
                    None => bad(p, "a number"),
                },
            },
            "EXACTNESS" => match string(e).as_deref() {
                Some("exact") => h.exactness = Exactness::Exact,
                Some("faithful") => h.exactness = Exactness::Faithful,
                _ => bad(p, "\"exact\" or \"faithful\""),
            },
            "REQUIRES" | "USES" => match list(e).and_then(|l| l.iter().map(string).collect::<Option<Vec<_>>>()) {
                Some(l) if *name == "REQUIRES" => h.requires = l,
                Some(l) => h.uses = l,
                None => bad(p, "a list of strings, `&[\"<id>@<version>\"]`"),
            },
            "TOLERANCE" => {
                let pairs = list(e).and_then(|l| {
                    l.iter()
                        .map(|t| match t {
                            syn::Expr::Tuple(t) if t.elems.len() == 2 => {
                                Some((string(&t.elems[0])?, int(&t.elems[1])?))
                            }
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>()
                });
                match pairs {
                    Some(l) => h.tolerance = l.into_iter().map(|(k, v)| (k, Value::Int(v as i128))).collect(),
                    None => bad(p, "a list of field names and raw differences, `&[(\"speed\", 16)]`"),
                }
            }
            _ => unreachable!(),
        }
    }
    h
}

fn string(e: &syn::Expr) -> Option<String> {
    match e {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

fn int(e: &syn::Expr) -> Option<u64> {
    match e {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => i.base10_parse().ok(),
        _ => None,
    }
}

/// `&[a, b]` or `[a, b]`.
fn list(e: &syn::Expr) -> Option<Vec<syn::Expr>> {
    match e {
        syn::Expr::Reference(r) => list(&r.expr),
        syn::Expr::Array(a) => Some(a.elems.iter().cloned().collect()),
        _ => None,
    }
}

/// The portable subset: what a mechanic's code may not use, so it can be
/// read on its own and translated into any language.
struct Subset<'d> {
    deps: &'d BTreeSet<String>,
    problems: Vec<(usize, String)>,
    /// Lines where a float type or literal appears.
    floats: Vec<usize>,
}

impl Subset<'_> {
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

impl<'ast> Visit<'ast> for Subset<'_> {
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
        self.no(line(i), "no extern crate: a mechanic depends only on the workspace's mechanics and interfaces");
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if !matches!(s.mutability, syn::StaticMutability::None) {
            self.no(line(s), "no static mut: a mechanic's step has no globals (use const for tables)");
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
            self.no(line(m), &format!("{name}!: a mechanic calls only {}", MACROS.join("!, ") + "!"));
        }
    }

    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        if a.path().is_ident("derive") {
            let _ = a.parse_nested_meta(|meta| {
                let name = meta.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
                if !DERIVES.contains(&name.as_str()) {
                    self.no(
                        line(&meta.path),
                        &format!("derive({name}): a mechanic derives only {}", DERIVES.join(", ")),
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
            self.no(line(s), &format!("{name}: a mechanic keeps to Vec, arrays and its own structs"));
        }
        if name == "f32" || name == "f64" {
            self.floats.push(line(s));
        }
        syn::visit::visit_path_segment(self, s);
    }

    fn visit_lit_float(&mut self, l: &'ast syn::LitFloat) {
        self.floats.push(line(l));
    }

    fn visit_lit_int(&mut self, l: &'ast syn::LitInt) {
        if matches!(l.suffix(), "f32" | "f64") {
            self.floats.push(line(l));
        }
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        let mut roots = Vec::new();
        use_roots(&u.tree, &mut roots);
        for (at, root) in roots {
            let ok = ["std", "core", "alloc", "crate", "self", "super"].contains(&root.as_str())
                || self.deps.contains(&root);
            if !ok {
                self.no(
                    at,
                    &format!("use {root}: not the standard library, this crate, or a dependency in Cargo.toml"),
                );
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
    use crate::value::Type;

    const CARGO: &str = r#"
[package]
name = "croc-walk"
version = "0.1.0"
edition = "2024"

[dependencies]
world-floor-height = { path = "../../interfaces/world.floor-height" }
"#;

    const WALK: &str = r#"
//! Walking.
//!
//! Speed closes a quarter of the gap to four times the target each tick.
//!
//! Notes for readers are not part of the summary.

use world_floor_height::FloorHeight;

pub const ID: &str = "croc.walk";
pub const VERSION: u32 = 1;
pub const RATE: &str = "tick";
pub const RATE_HZ: u32 = 25;
pub const REQUIRES: &[&str] = &["world.floor-height@1"];

/// Fixed point: 20 integer bits, 12 fraction bits.
pub type Fx20_12 = i32;
pub type Angle = u16;
pub const CURVE_LEN: usize = 8;

pub struct State {
    /// How fast Croc walks.
    /// Unit: units/tick
    pub speed: Fx20_12,
    pub heading: Angle,
    pub mode: Mode,
}

pub enum Mode { Still, Walking, BackingUp }

pub struct Inputs {
    /// Forward is held, as the step sees it.
    pub forward: bool,
}

pub struct Piece { pub heading: u16, pub x: i32 }

pub struct Params {
    pub curve: [UFx4_12; CURVE_LEN],
    pub pieces: Vec<Piece>,
}

pub type UFx4_12 = u16;

pub fn step(state: &mut State, inputs: &Inputs, params: &Params, world: &mut impl FloorHeight) {
    if inputs.forward {
        state.speed = state.speed.wrapping_add((params.curve[0] as i32) >> 2);
    }
    debug_assert!(state.speed >= 0);
}
"#;

    fn read(cargo: &str, lib: &str) -> Result<(Mechanic, Vec<String>), Vec<String>> {
        mechanic(&Crate { cargo_toml: cargo, files: &[(LIB.into(), lib.into())] })
    }

    #[test]
    fn the_glue_encodes_each_struct() {
        let files = [(LIB.to_string(), WALK.to_string())];
        let g = glue(&Crate { cargo_toml: CARGO, files: &files }).unwrap();
        let (m, _) = read(CARGO, WALK).unwrap();
        assert!(g.contains(&format!("const FINGERPRINT: &'static str = {:?};", m.fingerprint())), "{g}");
        assert!(g.contains("type State = croc_walk::State;"), "{g}");
        assert!(g.contains("(\"speed\", psx_capture::Value::Int((*v).speed as i128))"), "{g}");
        assert!(g.contains("croc_walk::Mode::BackingUp => psx_capture::Value::Variant(\"backing_up\"),"), "{g}");
        assert!(
            g.contains(
                "psx_capture::Value::List((*v).curve.iter().map(|e1| psx_capture::Value::Int((*e1) as i128)).collect())"
            ),
            "{g}"
        );
        assert!(g.contains("(\"x\", psx_capture::Value::Int((*e1).x as i128))"), "{g}");
    }

    #[test]
    fn the_replay_glue_converts_both_ways_and_calls_step() {
        let lib = WALK.replace(", world: &mut impl FloorHeight", "");
        let files = [(LIB.to_string(), lib)];
        let g = replay_glue(&Crate { cargo_toml: CARGO, files: &files }).unwrap();
        let (m, _) = read(CARGO, WALK).unwrap();
        assert!(g.starts_with("pub mod croc_walk {"), "{g}");
        assert!(g.contains(&format!("const FINGERPRINT: &'static str = {:?};", m.fingerprint())), "{g}");
        assert!(g.contains("const TOLERANCE: Option<&'static [(&'static str, i128)]> = None;"), "{g}");
        assert!(g.contains("type Params = ::croc_walk::Params;"), "{g}");
        assert!(g.contains("speed: { let v0 = psx_mechanics::replay::field(v, \"speed\"); psx_mechanics::replay::int::<i32>(v0).map_err(psx_mechanics::replay::at(\"speed\"))? },"), "{g}");
        assert!(g.contains("\"backing_up\" => Ok(::croc_walk::Mode::BackingUp),"), "{g}");
        assert!(g.contains(".and_then(psx_mechanics::replay::array)"), "{g}");
        assert!(
            g.contains(
                "psx_mechanics::replay::make_record([(\"speed\", psx_mechanics::Value::Int((*v).speed as i128))"
            ),
            "{g}"
        );
        assert!(g.contains("::croc_walk::Mode::Still => psx_mechanics::Value::Variant(\"still\".into()),"), "{g}");
        assert!(g.contains("::croc_walk::step(state, inputs, params);"), "{g}");

        let problems = replay_glue(&Crate { cargo_toml: CARGO, files: &[(LIB.into(), WALK.into())] }).unwrap_err();
        assert_eq!(problems, ["src/lib.rs: `step` makes calls, and calls are not replayed yet"]);
        let no_step = WALK.split("pub fn step").next().unwrap().to_string();
        let problems = replay_glue(&Crate { cargo_toml: CARGO, files: &[(LIB.into(), no_step)] }).unwrap_err();
        assert_eq!(problems, ["src/lib.rs: no `pub fn step(state: &mut State, ...)`"]);
    }

    #[test]
    fn a_step_that_takes_something_else_is_named() {
        let lib = WALK.replace(
            "pub fn step(state: &mut State, inputs: &Inputs,",
            "pub fn step(inputs: &Inputs, state: &mut State,",
        );
        let (_, problems) = read(CARGO, &lib).unwrap();
        assert!(problems.iter().any(|p| p.contains("`step` takes `&mut State`, then")), "{problems:#?}");
    }

    #[test]
    fn a_rust_mechanic_reads_into_the_same_model() {
        let (m, problems) = read(CARGO, WALK).unwrap();
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!((m.id.as_str(), m.version, m.title.as_str()), ("croc.walk", 1, "Walking"));
        assert_eq!(m.summary, "Speed closes a quarter of the gap to four times the target each tick.");
        assert_eq!(m.rate, Rate { per: Per::Tick, hz: Some(25.0) });
        assert_eq!(m.requires, ["world.floor-height@1"]);
        let types = m.types(&mut Vec::new());
        assert_eq!(types.state["speed"], Type::Fixed { signed: true, int_bits: 20, frac_bits: 12 });
        assert_eq!(types.state["heading"], Type::Int { bits: 16, signed: false });
        assert_eq!(types.state["mode"], Type::Enum(vec!["still".into(), "walking".into(), "backing_up".into()]));
        assert_eq!(types.inputs["forward"], Type::Bool);
        assert_eq!(
            types.params["curve"],
            Type::Table { of: Box::new(Type::Fixed { signed: false, int_bits: 4, frac_bits: 12 }), len: 8 }
        );
        assert!(matches!(&types.params["pieces"], Type::List { of } if matches!(**of, Type::Record(_))));
        assert_eq!(m.state["speed"].unit.as_deref(), Some("units/tick"));
        assert_eq!(m.state["speed"].description.as_deref(), Some("How fast Croc walks."));
        assert_eq!(m.inputs["forward"].description.as_deref(), Some("Forward is held, as the step sees it."));
    }

    #[test]
    fn the_portable_subset_is_held() {
        let lib = WALK.to_string()
            + r#"
use serde::Serialize;
static mut COUNT: u32 = 0;
macro_rules! twice { ($e:expr) => { $e * 2 } }
pub struct Bag<T> { pub items: Vec<T> }
#[derive(Clone, Serialize)]
pub struct Cache { pub seen: std::collections::HashMap<u32, u32> }
pub fn pick(x: &dyn Fn() -> u32) -> u32 { let y = format!("{}", 1); unsafe { 0 } }
pub fn half(x: i32) -> f32 { x as f32 * 0.5 }
"#;
        let cargo = CARGO.to_string() + "serde = \"1\"\n";
        let (_, problems) = read(&cargo, &lib).unwrap();
        let has = |s: &str| problems.iter().any(|p| p.contains(s));
        assert!(has("dependencies.serde: a mechanic depends only on"), "{problems:#?}");
        assert!(has("use serde: not the standard library"), "{problems:#?}");
        assert!(has("no static mut"), "{problems:#?}");
        assert!(has("no macro definitions"), "{problems:#?}");
        assert!(has("struct Bag: no generic parameters"), "{problems:#?}");
        assert!(has("derive(Serialize)"), "{problems:#?}");
        assert!(has("HashMap: a mechanic keeps to Vec"), "{problems:#?}");
        assert!(has("no trait objects"), "{problems:#?}");
        assert!(has("format!: a mechanic calls only"), "{problems:#?}");
        assert!(has("no unsafe code"), "{problems:#?}");
        assert!(has("a float in an exact mechanic"), "{problems:#?}");
        assert!(problems.iter().all(|p| p.starts_with("src/lib.rs:") || p.starts_with("Cargo.toml:")), "{problems:#?}");
    }

    #[test]
    fn fields_it_cannot_record_and_a_missing_header_are_named() {
        let lib = WALK.replace("pub forward: bool,", "pub forward: bool,\n    pub name: String,\n    pub odd: Fx8_8,")
            + "pub type Fx8_8 = i32;\n";
        let (_, problems) = read(CARGO, &lib).unwrap();
        assert!(
            problems.iter().any(|p| p.contains("Inputs.name: `String` is not a type a trace can hold")),
            "{problems:#?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("Inputs.odd: `Fx8_8` is 8.8 fixed point, signed: alias i16")),
            "{problems:#?}"
        );
        let problems = read(CARGO, &WALK.replace("pub const VERSION: u32 = 1;", "")).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("no `pub const VERSION: u32`")), "{problems:#?}");
        let problems = read(CARGO, "pub fn (").unwrap_err();
        assert!(problems[0].starts_with("src/lib.rs:1: "), "{problems:#?}");
    }
}

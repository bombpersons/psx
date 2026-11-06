//! `code names`: where a name is defined and what it resolves to; with no
//! query, the names files' problems and what is most worth naming next,
//! from the functions the recordings run most.

use std::collections::BTreeMap;
use std::path::Path;

use psx_symbols::names::{Names, Type, TypeBody};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

#[derive(clap::Args, Deserialize, schemars::JsonSchema)]
pub struct CodeNamesArgs {
    /// A function, global, type, `Type.field` or `function.local` to look up, or a hex address; none lists the names files' problems and what is most worth naming next.
    pub query: Option<String>,
    #[command(flatten)]
    #[serde(flatten)]
    pub input: crate::ghidra::Input,
    /// With no query: how many of the functions the captured recordings run most to decompile for what they leave unnamed.
    #[arg(long, default_value_t = 20)]
    #[serde(default = "d_functions")]
    pub functions: usize,
    /// Per-function decompiler budget in seconds (1..=300).
    #[arg(long, default_value_t = 30)]
    #[serde(default = "d_decompile_timeout")]
    pub decompile_timeout: u32,
}

fn d_functions() -> usize {
    20
}

fn d_decompile_timeout() -> u32 {
    30
}

impl Op for CodeNamesArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report(run(ctx, self).map(|v| (true, v)))
    }
}

fn run(ctx: &Ctx, a: CodeNamesArgs) -> crate::Result<Value> {
    let names = psx_symbols::names::load(&ctx.root);
    let problems: Vec<String> = names.problems.iter().map(|p| format!("{}: {}", p.at, p.message)).collect();
    match &a.query {
        Some(q) => {
            let found = look_up(&names, q);
            let status = if found.is_empty() { "not_found" } else { "found" };
            Ok(json!({"status": status, "query": q, "found": found, "problems": problems}))
        }
        None => {
            if a.input.source.is_none() && a.input.analysis.is_none() && a.input.ram.is_none() {
                return Err(Error::invalid(
                    "give source (the EXE or disc.cue::/PATH) or analysis for what to name next",
                ));
            }
            let exe = a.input.source.as_deref().and_then(|s| s.rsplit_once("::").map(|(_, p)| p.to_string()));
            let runs = runs(&ctx.root, exe.as_deref());
            let mut hot: Vec<(u32, u64)> = runs.into_iter().collect();
            hot.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            hot.truncate(a.functions);
            let mut v = json!({"status": "complete", "problems": problems});
            if hot.is_empty() {
                v["why"] = json!(
                    "no captured recording has coverage of the analysed executable: take code_inventory, then record"
                );
                v["next"] = json!([]);
                return Ok(v);
            }
            let addrs: Vec<u32> = hot.iter().map(|h| h.0).collect();
            let got = crate::ghidra::decompile_many(&a.input, &addrs, a.decompile_timeout, ctx)?;
            let weight: BTreeMap<u32, u64> = hot.iter().copied().collect();
            let (next, functions) = next_to_name(&got, &weight);
            v["next"] = json!(next);
            v["functions"] = json!(functions);
            Ok(v)
        }
    }
}

/// Each function of the inventory's executable `exe` (all of them when
/// None) that captured recordings ran, with how often: the runs of the
/// block at its entry (calls, not its loops), summed over the recordings'
/// coverage.
fn runs(root: &Path, exe: Option<&str>) -> BTreeMap<u32, u64> {
    let hex = |v: &Value| v.as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let read = |p: &Path| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let inventory = read(&root.join(".psx/inventory.json")).unwrap_or_default();
    // Function extents per executable.
    let mut extents: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    for e in inventory["executables"].as_array().into_iter().flatten() {
        let path = e["path"].as_str().unwrap_or("").to_string();
        if exe.is_some_and(|x| !x.eq_ignore_ascii_case(&path)) {
            continue;
        }
        let mut fs: Vec<(u32, u32)> = e["functions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| Some((hex(&f["entry"])?, hex(&f["end"])?)))
            .collect();
        fs.sort();
        extents.insert(path, fs);
    }
    let mut out = BTreeMap::new();
    for id in crate::scenario::ids(root) {
        let l = crate::scenario::Loaded::load(root, &id);
        for (_, file, r) in &l.recordings {
            let Ok(r) = r else { continue };
            if r.header.source.kind != psx_scenario::scenario::SourceKind::Captured {
                continue;
            }
            let Some(cov) = read(&file.with_extension("coverage.json")) else { continue };
            for image in cov["images"].as_array().into_iter().flatten() {
                let Some(fs) = image["name"].as_str().and_then(|n| extents.get(n)) else { continue };
                let mut most: BTreeMap<u32, u64> = BTreeMap::new();
                for r in image["executed"].as_array().into_iter().flatten() {
                    let (Some(s), Some(e), Some(n)) = (hex(&r[0]), hex(&r[1]), r[2].as_u64()) else { continue };
                    // The functions whose entry this range ran.
                    for &(entry, _) in &fs[fs.partition_point(|f| f.0 < s)..] {
                        if entry >= e {
                            break;
                        }
                        let m = most.entry(entry).or_default();
                        *m = (*m).max(n);
                    }
                }
                for (f, n) in most {
                    *out.entry(f).or_default() += n;
                }
            }
        }
    }
    out
}

/// What the decompiles `got` leave unnamed, each weighted by how often the
/// functions it appears in ran (`weight`, by entry): `NAME (SCORE, in
/// FUNCTIONS)`, highest first; and the functions decompiled, with runs.
fn next_to_name(got: &[Value], weight: &BTreeMap<u32, u64>) -> (Vec<Value>, Vec<Value>) {
    let mut score: BTreeMap<String, (u64, u64, Vec<String>)> = BTreeMap::new();
    let mut functions = Vec::new();
    for f in got {
        let name = f["function"]["name"].as_str().unwrap_or("?").to_string();
        let entry =
            f["function"]["entry"].as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        let w = entry.and_then(|e| weight.get(&e).copied()).unwrap_or(1);
        functions.push(json!({"function": name, "entry": f["function"]["entry"], "runs": w, "status": f["status"]}));
        for (k, n) in f["unnamed"].as_object().into_iter().flatten() {
            let n = n.as_u64().unwrap_or(0);
            let s = score.entry(crate::ghidra::offset_upper(k)).or_default();
            s.0 += n * w;
            s.1 += n;
            s.2.push(name.clone());
        }
    }
    let mut v: Vec<_> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    let next = v
        .into_iter()
        .take(60)
        .map(|(k, (score, seen, mut fs))| {
            fs.truncate(5);
            json!({"name": k, "score": score, "seen": seen, "in": fs})
        })
        .collect();
    (next, functions)
}

/// A local's slot as the names files write it.
fn slot(s: &psx_symbols::names::Slot) -> String {
    use psx_symbols::names::Slot;
    match s {
        Slot::Stack(n) => format!("local_{:x}", n.unsigned_abs()),
        Slot::Frame(n) if *n < 0 => format!("sp-0x{:X}", n.unsigned_abs()),
        Slot::Frame(n) => format!("sp+0x{n:X}"),
        Slot::Register(r) => r.clone(),
    }
}

fn hex(v: u32) -> String {
    format!("0x{v:08X}")
}

fn bare(s: &str) -> &str {
    s.trim_end_matches('?')
}

/// Every definition `q` names (or, for an address, every name covering it).
fn look_up(names: &Names, q: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let q = q.trim();
    let addr = q
        .strip_prefix("0x")
        .or_else(|| q.strip_prefix("0X"))
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .map(|a| crate::names_bind::canonical(a).unwrap_or(a));
    for f in &names.functions {
        let body = crate::names_bind::body(f.address, f.module.as_deref());
        let hit = match addr {
            Some(a) => {
                crate::names_bind::canonical(f.address) == Some(a)
                    || body.as_ref().is_some_and(|b| {
                        b.iter()
                            .any(|&(s, e)| crate::names_bind::canonical(s).is_some_and(|c| a >= c && a < c + (e - s)))
                    })
            }
            None => {
                bare(&f.name.text) == bare(q)
                    || f.module.as_ref().is_some_and(|m| format!("{m}:{}", bare(&f.name.text)) == q)
            }
        };
        if hit {
            let mut j =
                json!({"kind": "function", "name": f.name.text, "at": f.at.to_string(), "address": hex(f.address)});
            if let Some(m) = &f.module {
                j["module"] = json!(m);
            }
            if let Some(sig) = &f.signature {
                j["declaration"] = json!(crate::decomp::declare(&Type::Function(sig.clone()), bare(&f.name.text)));
            }
            match body {
                Some(b) => {
                    j["body"] = json!(b.iter().map(|(s, e)| format!("{}-{}", hex(*s), hex(*e))).collect::<Vec<_>>())
                }
                None => j["note"] = json!("the inventory has no function starting there (or none is taken)"),
            }
            if let Some(c) = &f.comment {
                j["comment"] = json!(c);
            }
            out.push(j);
        }
        if let Some((_, local)) = q.split_once('.').filter(|(func, _)| bare(&f.name.text) == bare(func)) {
            for l in f.locals.iter().filter(|l| bare(&l.name.text) == bare(local)) {
                let mut j = json!({"kind": "local", "name": format!("{}.{}", f.name.text, l.name.text), "at": l.at.to_string(), "slot": slot(&l.slot)});
                if let Some(t) = &l.ty {
                    j["declaration"] = json!(crate::decomp::declare(t, bare(&l.name.text)));
                }
                out.push(j);
            }
        }
    }
    for g in &names.globals {
        let size = g.ty.as_ref().and_then(|t| names.size_of(t)).unwrap_or(1).max(1);
        let hit = match addr {
            Some(a) => a >= g.address && a < g.address + size,
            None => bare(&g.name.text) == bare(q),
        };
        if hit {
            let mut j = json!({"kind": "global", "name": g.name.text, "at": g.at.to_string(), "address": hex(g.address), "size": size});
            if let Some(t) = &g.ty {
                j["declaration"] = json!(crate::decomp::declare(t, bare(&g.name.text)));
            }
            if let Some(a) = addr {
                j["resolves"] = json!(crate::names_bind::global_at(names, a));
            }
            if let Some(m) = &g.module {
                j["module"] = json!(m);
            }
            out.push(j);
        }
    }
    if addr.is_none() {
        let (ty, field) = match q.split_once('.') {
            Some((t, f)) => (t, Some(f)),
            None => (q, None),
        };
        for t in names.types.iter().filter(|t| bare(&t.name.text) == bare(ty)) {
            let size = t.size.or_else(|| names.size_of(&Type::Named { name: bare(&t.name.text).to_string() }));
            let (kind, fields) = match &t.body {
                TypeBody::Struct { fields } => ("struct", Some(fields)),
                TypeBody::Union { fields } => ("union", Some(fields)),
                TypeBody::Enum { .. } => ("enum", None),
                TypeBody::Typedef { .. } => ("typedef", None),
            };
            match field {
                None => {
                    let mut j = json!({"kind": kind, "name": t.name.text, "at": t.at.to_string()});
                    if let Some(s) = size {
                        j["size"] = json!(format!("0x{s:X}"));
                    }
                    if let Some(fs) = fields {
                        j["fields"] = json!(fs.len());
                        let named: u32 = fs.iter().filter_map(|f| names.size_of(&f.ty)).sum();
                        if let Some(s) = size {
                            j["bytes_in_no_field"] = json!(s.saturating_sub(named));
                        }
                    }
                    if let TypeBody::Typedef { ty } = &t.body {
                        j["declaration"] = json!(crate::decomp::declare(ty, bare(&t.name.text)));
                    }
                    out.push(j);
                }
                Some(f) => {
                    for x in fields.into_iter().flatten().filter(|x| bare(&x.name.text) == bare(f)) {
                        out.push(json!({
                            "kind": "field",
                            "name": format!("{}.{}", t.name.text, x.name.text),
                            "at": x.at.to_string(),
                            "offset": format!("0x{:X}", x.offset),
                            "size": names.size_of(&x.ty),
                            "declaration": crate::decomp::declare(&x.ty, bare(&x.name.text)),
                        }));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Names {
        psx_symbols::names::from_texts(&[("names/a.names".into(), text.into())], &[])
    }

    const NAMES: &str = "struct Poly 0x2C {\n  0x0 u8 flags\n  0x2 u16 bits\n}\nglobal 0x80075160 Poly *polys\nfunction 0x80031F64 void world_collide(int *obj)\n  local_100 Poly *rec\n";

    #[test]
    fn a_name_is_found_with_its_file_and_line() {
        let n = names(NAMES);
        let f = look_up(&n, "world_collide");
        assert_eq!(f[0]["at"], "names/a.names:6");
        assert_eq!(f[0]["declaration"], "void world_collide(int *obj)");
        let f = look_up(&n, "Poly");
        assert_eq!(
            (f[0]["kind"].as_str(), f[0]["size"].as_str(), f[0]["bytes_in_no_field"].as_u64()),
            (Some("struct"), Some("0x2C"), Some(41))
        );
        let f = look_up(&n, "Poly.bits");
        assert_eq!((f[0]["offset"].as_str(), f[0]["at"].as_str()), (Some("0x2"), Some("names/a.names:3")));
        let f = look_up(&n, "world_collide.rec");
        assert_eq!(f[0]["declaration"], "Poly *rec");
        let f = look_up(&n, "0x80075162");
        assert_eq!((f[0]["kind"].as_str(), f[0]["resolves"].as_str()), (Some("global"), Some("polys+0x2")));
        assert!(look_up(&n, "nothing").is_empty());
    }

    #[test]
    fn what_to_name_next_is_weighted_by_runs() {
        let got = vec![
            json!({"function": {"name": "a", "entry": "0x80010000"}, "status": "complete", "unnamed": {"FUN_80020000": 2, "Poly+0x4": 1}}),
            json!({"function": {"name": "b", "entry": "0x80011000"}, "status": "complete", "unnamed": {"Poly+0x4": 1}}),
        ];
        let w = BTreeMap::from([(0x8001_0000, 1), (0x8001_1000, 100)]);
        let (next, functions) = next_to_name(&got, &w);
        assert_eq!(next[0], json!({"name": "Poly+0x4", "score": 101, "seen": 2, "in": ["a", "b"]}));
        assert_eq!(next[1]["name"], "FUN_80020000");
        assert_eq!(functions[1]["runs"], 100);
    }
}

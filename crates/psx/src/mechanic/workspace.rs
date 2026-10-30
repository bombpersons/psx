//! A workspace's mechanic files (docs/mechanics.md, section 1):
//! interfaces (`interfaces/<id>/`, Rust crates, or `interfaces/<id>.toml`)
//! and `mechanics/<id>/` with `mechanic.toml` or a Rust crate
//! (docs/rust-mechanics-plan.md), its Rust binding crate in `binding/`, and
//! `traces/`, read and checked together.

use std::path::{Path, PathBuf};

use psx_mechanics::trace::Header;
use psx_mechanics::{Interface, Mechanic, Trace};
use serde_json::Value;

use super::{CallSite, Capture, End};
use crate::Error;

pub const INTERFACES: &str = "interfaces";
pub const MECHANICS: &str = "mechanics";

/// One mechanic's files, with every problem found in them.
pub struct Loaded {
    pub dir: PathBuf,
    pub mechanic: Mechanic,
    /// The binding: its crate, `binding/`.
    pub plugin: Option<PathBuf>,
    /// Every interface in the workspace, and every mechanic's step.
    pub interfaces: Vec<Interface>,
    /// `<file>: <problem>`, files relative to the workspace.
    pub problems: Vec<String>,
}

fn read(path: &Path) -> crate::Result<String> {
    std::fs::read_to_string(path).map_err(|e| Error::io(path, e))
}

/// Every interface (`interfaces/<id>/`, a Rust crate, or
/// `interfaces/<id>.toml`) and every mechanic's step (what a mechanic that
/// uses it calls), and the problems in them.
pub fn interfaces(root: &Path) -> crate::Result<(Vec<Interface>, Vec<String>)> {
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    let dir = root.join(INTERFACES);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in &paths {
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
        let (i, file, mut found) = if is_crate(p) {
            let at = |e: String| format!("{INTERFACES}/{name}/{e}");
            let cargo_toml = read(&p.join("Cargo.toml"))?;
            let files = sources(p)?;
            match psx_mechanics::rust::interface(&psx_mechanics::rust::Crate { cargo_toml: &cargo_toml, files: &files })
            {
                Err(e) => {
                    problems.extend(e.into_iter().map(at));
                    continue;
                }
                Ok((i, found)) => {
                    let mut found: Vec<String> = found.into_iter().map(at).collect();
                    let want = name.replace('.', "-");
                    if psx_mechanics::rust::package_name(&cargo_toml).as_deref() != Some(want.as_str()) {
                        found.push(at(format!("Cargo.toml: the package is named {want:?}, after the interface's id")));
                    }
                    (i, format!("{INTERFACES}/{name}/{}", psx_mechanics::rust::LIB), found)
                }
            }
        } else if let Some(stem) = name.strip_suffix(".toml") {
            let file = format!("{INTERFACES}/{name}");
            match Interface::parse(&read(p)?) {
                Err(e) => {
                    problems.push(format!("{file}: {e}"));
                    continue;
                }
                Ok(i) => {
                    let mut found = Vec::new();
                    if i.id != stem {
                        found.push(format!("{file}: id {:?} differs from the file's name", i.id));
                    }
                    (i, file, found)
                }
            }
        } else {
            continue;
        };
        if is_crate(p) && i.id != name {
            found.push(format!("{file}: id {:?} differs from the directory's name", i.id));
        }
        if out.iter().any(|o: &Interface| o.id == i.id) {
            found.push(format!("{file}: interface {:?} is declared twice", i.id));
        }
        problems.extend(found);
        problems.extend(i.check().into_iter().map(|e| format!("{file}: {e}")));
        out.push(i);
    }
    // Each mechanic's step, for the mechanics that use it.
    for id in ids(root) {
        let Ok((m, file, _)) = mechanic(root, &id) else { continue };
        if out.iter().any(|i| i.id == m.id) {
            problems.push(format!("{file}: id {:?} is also an interface's", m.id));
            continue;
        }
        out.push(m.step_interface());
    }
    Ok((out, problems))
}

/// Whether `dir` holds a mechanic written as Rust: a crate with `src/lib.rs`.
fn is_crate(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join(psx_mechanics::rust::LIB).is_file()
}

/// The ids of `mechanics/*/` holding a `mechanic.toml` or a Rust crate.
pub fn ids(root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(root.join(MECHANICS))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("mechanic.toml").is_file() || is_crate(&e.path()))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    ids
}

/// The `.rs` files under `dir/src`, by path relative to `dir`, sorted.
fn sources(dir: &Path) -> crate::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut todo = vec![dir.join("src")];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| Error::io(&d, e))?.flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                out.push((rel, read(&p)?));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `mechanics/<id>/`'s mechanic, from `mechanic.toml` or its Rust crate:
/// the mechanic, the file problems about it are reported against, and the
/// problems found reading it (each `<file>: <problem>`).
pub fn mechanic(root: &Path, id: &str) -> crate::Result<(Mechanic, String, Vec<String>)> {
    let dir = root.join(MECHANICS).join(id);
    let toml = dir.join("mechanic.toml");
    let rel = format!("{MECHANICS}/{id}");
    match (toml.is_file(), is_crate(&dir)) {
        (true, true) => Err(Error::invalid(format!(
            "{rel}: both mechanic.toml and a Rust crate declare the mechanic; keep the crate"
        ))),
        (false, false) => {
            Err(Error::invalid(format!("{rel}: no mechanic here (a Rust crate with src/lib.rs, or mechanic.toml)")))
        }
        (true, false) => {
            let file = format!("{rel}/mechanic.toml");
            let m = Mechanic::parse(&read(&toml)?).map_err(|e| Error::invalid(format!("{file}: {e}")))?;
            Ok((m, file, Vec::new()))
        }
        (false, true) => {
            let cargo_toml = read(&dir.join("Cargo.toml"))?;
            let files = sources(&dir)?;
            let at = |p: Vec<String>| p.into_iter().map(|e| format!("{rel}/{e}")).collect::<Vec<_>>();
            let (m, problems) =
                psx_mechanics::rust::mechanic(&psx_mechanics::rust::Crate { cargo_toml: &cargo_toml, files: &files })
                    .map_err(|p| Error::invalid(at(p).join("\n")))?;
            Ok((m, format!("{rel}/{}", psx_mechanics::rust::LIB), at(problems)))
        }
    }
}

/// `mechanics/<id>/`'s mechanic and binding, checked against the
/// workspace's interfaces and inventory.
pub fn load(root: &Path, id: &str) -> crate::Result<Loaded> {
    let dir = root.join(MECHANICS).join(id);
    let file = |name: &str| format!("{MECHANICS}/{id}/{name}");
    let (mechanic, declared_in, read_problems) = self::mechanic(root, id)?;
    let (interfaces, mut problems) = interfaces(root)?;
    problems.extend(read_problems);
    if mechanic.id != id {
        problems.push(format!("{declared_in}: id {:?} differs from the directory's name", mechanic.id));
    }
    if is_crate(&dir) {
        let package =
            std::fs::read_to_string(dir.join("Cargo.toml")).ok().and_then(|t| psx_mechanics::rust::package_name(&t));
        let want = id.replace('.', "-");
        if package.as_deref() != Some(want.as_str()) {
            problems.push(format!("{}: the package is named {want:?}, after the mechanic's id", file("Cargo.toml")));
        }
    }
    problems.extend(mechanic.check(&interfaces).into_iter().map(|e| format!("{declared_in}: {e}")));
    if dir.join("binding.toml").is_file() {
        problems.push(format!(
            "{}: binding.toml is no longer read: bind the mechanic in Rust, in binding/",
            file("binding.toml")
        ));
    }
    let plugin_dir = dir.join("binding");
    let plugin = plugin_dir.join("Cargo.toml").is_file().then_some(plugin_dir);
    match &plugin {
        Some(p) => {
            let package =
                std::fs::read_to_string(p.join("Cargo.toml")).ok().and_then(|t| psx_mechanics::rust::package_name(&t));
            let want = super::plugin::package(id);
            if package.as_deref() != Some(want.as_str()) {
                problems.push(format!(
                    "{}: the package is named {want:?}, after the mechanic's id",
                    file("binding/Cargo.toml")
                ));
            }
        }
        None => problems.push(format!("{}: not written yet", file("binding"))),
    }
    Ok(Loaded { dir, mechanic, plugin, interfaces, problems })
}

/// A trace's name, its problems and, when it parses, its header.
pub type Checked = (String, Vec<String>, Option<Header>);

/// The traces in `mechanics/<id>/traces/`.
pub fn traces(l: &Loaded) -> crate::Result<Vec<Checked>> {
    let dir = l.dir.join("traces");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let Some(name) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trace.jsonl")) else {
            continue;
        };
        let (problems, header) = match Trace::parse(&read(&p)?) {
            Err(e) => (vec![e], None),
            Ok(t) => (t.check(&l.mechanic, &l.interfaces), Some(t.header)),
        };
        out.push((name.to_string(), problems, header));
    }
    Ok(out)
}

/// Every PC the binding names: what names it, the PC and its overlay.
pub fn pcs(c: &Capture) -> Vec<(String, u32, Option<&str>)> {
    let mut out = vec![("begin".to_string(), c.begin.pc, c.begin.overlay.as_deref())];
    if let End::At(p) = &c.end {
        out.push(("end".into(), p.pc, p.overlay.as_deref()));
    }
    for (i, call) in c.calls.iter().enumerate() {
        let (CallSite::Entry { entry: p } | CallSite::Read { read: p }) = &call.on;
        out.push((format!("calls[{i}] ({}.{})", call.interface, call.call), p.pc, p.overlay.as_deref()));
    }
    out
}

/// Code an overlay's PC runs in: its function's chunks and their bytes. The
/// PC is the overlay's when RAM holds them.
pub struct Resident {
    chunks: Vec<(u32, Vec<u8>)>,
}

impl Resident {
    pub fn holds(&self, ram: &[u8]) -> bool {
        self.chunks.iter().all(|(at, bytes)| {
            let o = (at & 0x1F_FFFF) as usize;
            ram.get(o..o + bytes.len()) == Some(bytes.as_slice())
        })
    }
}

fn inventory(root: &Path) -> Result<Value, String> {
    let p = root.join(".psx/inventory.json");
    let text =
        std::fs::read_to_string(&p).map_err(|_| "overlays are named by psx inventory: take it first".to_string())?;
    serde_json::from_str(&text).map_err(|e| format!(".psx/inventory.json: {e}"))
}

/// The function of inventory overlay `name` that holds `pc`.
fn resident_in(root: &Path, inv: &Value, name: &str, pc: u32) -> Result<Resident, String> {
    let hex = |v: &Value| v.as_str().and_then(|s| u32::from_str_radix(s, 16).ok());
    let overlays = inv["overlays"].as_array().into_iter().flatten();
    let Some(o) = overlays.clone().find(|o| o["name"] == name) else {
        let names: Vec<&str> = overlays.filter_map(|o| o["name"].as_str()).collect();
        return Err(format!("overlay {name:?} is not in the inventory ({})", names.join(", ")));
    };
    for f in o["functions"].as_array().into_iter().flatten() {
        let chunks: Vec<(u32, u32)> = match f["chunks"].as_array() {
            Some(cs) => cs.iter().filter_map(|c| hex(&c[0]).zip(hex(&c[1]))).collect(),
            None => hex(&f["entry"]).zip(hex(&f["end"])).into_iter().collect(),
        };
        if !chunks.iter().any(|&(s, e)| (s..e).contains(&pc)) {
            continue;
        }
        let sha = f["sha256"].as_str().unwrap_or_default();
        let bytes = std::fs::read(crate::inventory::overlay_bytes(root, sha))
            .ok()
            .filter(|b| crate::stamp::sha256(b) == sha)
            .ok_or_else(|| {
                format!("overlay {name:?}: its function's bytes are not in .psx/overlays; take the inventory again")
            })?;
        let mut rest = bytes.as_slice();
        let mut out = Vec::new();
        for (s, e) in chunks {
            let n = (e - s) as usize;
            if rest.len() < n {
                return Err(format!(
                    "overlay {name:?}: its function's bytes do not match its chunks; take the inventory again"
                ));
            }
            out.push((s, rest[..n].to_vec()));
            rest = &rest[n..];
        }
        return Ok(Resident { chunks: out });
    }
    Err(format!("pc {pc:#010x} is in none of overlay {name:?}'s functions"))
}

/// The code each overlay-qualified PC of the binding runs in, by PC.
pub fn residents(root: &Path, c: &Capture) -> Result<Vec<(u32, Option<Resident>)>, String> {
    let named = pcs(c);
    let inv = if named.iter().any(|(_, _, o)| o.is_some()) { Some(inventory(root)?) } else { None };
    named
        .into_iter()
        .map(|(what, pc, o)| match (o, &inv) {
            (Some(name), Some(inv)) => {
                resident_in(root, inv, name, pc).map(|r| (pc, Some(r))).map_err(|e| format!("{what}: {e}"))
            }
            _ => Ok((pc, None)),
        })
        .collect()
}

pub fn overlay_problems(root: &Path, c: &Capture) -> Vec<String> {
    let named = pcs(c);
    if named.iter().all(|(_, _, o)| o.is_none()) {
        return Vec::new();
    }
    let inv = match inventory(root) {
        Ok(i) => i,
        Err(e) => return vec![e],
    };
    named
        .into_iter()
        .filter_map(|(what, pc, o)| Some((what, pc, o?)))
        .filter_map(|(what, pc, name)| resident_in(root, &inv, name, pc).err().map(|e| format!("{what}: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mechanic_is_read_from_its_rust_crate() {
        let root = std::env::temp_dir().join(format!("psx-rust-mechanic-{}", std::process::id()));
        let dir = root.join("mechanics/croc.turn");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"croc-spin\"\n").unwrap();
        let lib = "//! Turning\n//!\n//! Croc turns.\n\npub const ID: &str = \"croc.turn\";\npub const VERSION: u32 = 1;\npub const RATE: &str = \"tick\";\npub const RATE_HZ: u32 = 25;\n\npub struct State {\n    pub heading: u16,\n}\n";
        std::fs::write(dir.join("src/lib.rs"), lib).unwrap();
        std::fs::write(dir.join("src/helpers.rs"), "pub fn f() -> u32 { let s = format!(\"\"); 0 }\n").unwrap();
        assert_eq!(ids(&root), ["croc.turn"]);
        let l = load(&root, "croc.turn").unwrap();
        assert_eq!(l.mechanic.title, "Turning");
        assert!(l.mechanic.state.contains_key("heading"));
        let has = |s: &str| l.problems.iter().any(|p| p.contains(s));
        assert!(has("mechanics/croc.turn/Cargo.toml: the package is named \"croc-turn\""), "{:?}", l.problems);
        assert!(has("mechanics/croc.turn/src/helpers.rs:1: format!"), "{:?}", l.problems);
        assert!(has("mechanics/croc.turn/binding: not written yet"), "{:?}", l.problems);
        // Both forms at once are refused.
        std::fs::write(dir.join("mechanic.toml"), "id = \"croc.turn\"\n").unwrap();
        assert!(load(&root, "croc.turn").err().unwrap().to_string().contains("keep the crate"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! A workspace's mechanic files (docs/mechanics.md, section 1):
//! `interfaces/<id>.toml` and `mechanics/<id>/` with `mechanic.toml`,
//! `binding.toml` and `traces/`, read and checked together.

use std::path::{Path, PathBuf};

use psx_mechanics::{Interface, Mechanic, Trace};
use serde_json::Value;

use super::{Begin, Binding, CallSite, End, Psx};
use crate::Error;

pub const INTERFACES: &str = "interfaces";
pub const MECHANICS: &str = "mechanics";

/// One mechanic's files, with every problem found in them.
pub struct Loaded {
    pub dir: PathBuf,
    pub mechanic: Mechanic,
    pub binding: Option<Binding>,
    /// Every interface in the workspace.
    pub interfaces: Vec<Interface>,
    /// `<file>: <problem>`, files relative to the workspace.
    pub problems: Vec<String>,
}

fn read(path: &Path) -> crate::Result<String> {
    std::fs::read_to_string(path).map_err(|e| Error::io(path, e))
}

/// Every `interfaces/*.toml`, and the problems in them.
pub fn interfaces(root: &Path) -> crate::Result<(Vec<Interface>, Vec<String>)> {
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    let dir = root.join(INTERFACES);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok((out, problems)) };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths.iter().filter(|p| p.extension().is_some_and(|e| e == "toml")) {
        let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let file = format!("{INTERFACES}/{stem}.toml");
        match Interface::parse(&read(p)?) {
            Err(e) => problems.push(format!("{file}: {e}")),
            Ok(i) => {
                if i.id != stem {
                    problems.push(format!("{file}: id {:?} differs from the file's name", i.id));
                }
                problems.extend(i.check().into_iter().map(|e| format!("{file}: {e}")));
                out.push(i);
            }
        }
    }
    Ok((out, problems))
}

/// The ids of `mechanics/*/` holding a `mechanic.toml`.
pub fn ids(root: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(root.join(MECHANICS))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("mechanic.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    ids.sort();
    ids
}

/// `mechanics/<id>/`'s mechanic and binding, checked against the
/// workspace's interfaces and inventory.
pub fn load(root: &Path, id: &str) -> crate::Result<Loaded> {
    let dir = root.join(MECHANICS).join(id);
    let file = |name: &str| format!("{MECHANICS}/{id}/{name}");
    let path = dir.join("mechanic.toml");
    if !path.is_file() {
        return Err(Error::invalid(format!("{}: no such file", file("mechanic.toml"))));
    }
    let mechanic =
        Mechanic::parse(&read(&path)?).map_err(|e| Error::invalid(format!("{}: {e}", file("mechanic.toml"))))?;
    let (interfaces, mut problems) = interfaces(root)?;
    if mechanic.id != id {
        problems.push(format!("{}: id {:?} differs from the directory's name", file("mechanic.toml"), mechanic.id));
    }
    problems.extend(mechanic.check(&interfaces).into_iter().map(|e| format!("{}: {e}", file("mechanic.toml"))));
    let binding_path = dir.join("binding.toml");
    let binding = if binding_path.is_file() {
        match toml::from_str::<Binding>(&read(&binding_path)?) {
            Err(e) => {
                problems.push(format!("{}: {}", file("binding.toml"), e.message()));
                None
            }
            Ok(b) => {
                let mut p = b.check::<Psx>(&mechanic, &interfaces);
                p.extend(overlay_problems(root, &b));
                problems.extend(p.into_iter().map(|e| format!("{}: {e}", file("binding.toml"))));
                Some(b)
            }
        }
    } else {
        problems.push(format!("{}: not written yet", file("binding.toml")));
        None
    };
    Ok(Loaded { dir, mechanic, binding, interfaces, problems })
}

/// The traces in `mechanics/<id>/traces/`, each with its problems.
pub fn traces(l: &Loaded) -> crate::Result<Vec<(String, Vec<String>)>> {
    let dir = l.dir.join("traces");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let Some(name) = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trace.jsonl")) else {
            continue;
        };
        let problems = match Trace::parse(&read(&p)?) {
            Err(e) => vec![e],
            Ok(t) => t.check(&l.mechanic, &l.interfaces),
        };
        out.push((name.to_string(), problems));
    }
    Ok(out)
}

/// Every PC the binding names: what names it, the PC and its overlay.
pub fn pcs(b: &Binding) -> Vec<(String, u32, Option<&str>)> {
    let Begin { pc, overlay, .. } = &b.capture.begin;
    let mut out = vec![("capture.begin".to_string(), *pc, overlay.as_deref())];
    if let End::At(p) = &b.capture.end {
        out.push(("capture.end".into(), p.pc, p.overlay.as_deref()));
    }
    for (r, calls) in &b.interfaces {
        for (name, c) in calls {
            let (CallSite::Entry { entry: p } | CallSite::Read { read: p }) = &c.on;
            out.push((format!("interfaces.{r:?}.{name}.on"), p.pc, p.overlay.as_deref()));
        }
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
pub fn residents(root: &Path, b: &Binding) -> Result<Vec<(u32, Option<Resident>)>, String> {
    let named = pcs(b);
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

fn overlay_problems(root: &Path, b: &Binding) -> Vec<String> {
    let named = pcs(b);
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

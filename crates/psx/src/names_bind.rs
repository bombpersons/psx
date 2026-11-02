//! Which of the project's names apply to an analysis. A function's name
//! applies where the analysed code is the code it was written for: the
//! inventory knows each function of the executables and overlays by the
//! sha256 of its bytes, so overlays loaded at one address are told apart by
//! what they hold, with nothing recorded. `in MODULE` names an overlay (or
//! an executable) by its file; a name without it is the executable's.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde_json::Value;

/// The analysed memory: its bytes from `base`.
pub(crate) struct Code<'a> {
    pub base: u32,
    pub bytes: &'a [u8],
}

impl Code<'_> {
    fn sha(&self, chunks: &[(u32, u32)]) -> Option<String> {
        let mut bytes = Vec::new();
        for &(s, e) in chunks {
            let o = s.checked_sub(self.base)? as usize;
            bytes.extend_from_slice(self.bytes.get(o..o + e.checked_sub(s)? as usize)?);
        }
        Some(crate::stamp::sha256(&bytes))
    }
}

/// A function of the inventory.
struct Known {
    /// The module as the inventory names it, for messages.
    module: String,
    /// What `in` may call it, lowercase: the overlay's name and its file's.
    labels: Vec<String>,
    executable: bool,
    chunks: Vec<(u32, u32)>,
    sha: String,
}

pub(crate) struct Inventory {
    by_entry: BTreeMap<u32, Vec<Known>>,
    /// Whether a module's code is what the analysis holds, once asked.
    present: HashMap<String, bool>,
}

pub(crate) enum Bind {
    Apply,
    /// Not in the analysed code: outside its memory, or another module's
    /// code is there. Nothing to say.
    Elsewhere,
    Skip(String),
}

fn hex(v: &Value) -> Option<u32> {
    crate::commands::addr_value(v)
}

fn chunks(f: &Value) -> Option<Vec<(u32, u32)>> {
    match f["chunks"].as_array() {
        Some(cs) => cs.iter().map(|c| Some((hex(&c[0])?, hex(&c[1])?))).collect(),
        None => Some(vec![(hex(&f["entry"])?, hex(&f["end"])?)]),
    }
}

fn base_name(path: &str) -> String {
    let first = path.split_whitespace().next().unwrap_or(path);
    first.rsplit(['/', '\\', ':']).next().unwrap_or(first).to_lowercase()
}

impl Inventory {
    /// The project's inventory, None without one. Executables' functions are
    /// hashed from the disc, as the inventory hashes overlays'.
    pub(crate) fn load(root: &Path) -> Option<Inventory> {
        let inv: Value = serde_json::from_slice(&std::fs::read(root.join(".psx/inventory.json")).ok()?).ok()?;
        let mut by_entry: BTreeMap<u32, Vec<Known>> = BTreeMap::new();
        let mut add = |module: &str, labels: Vec<String>, executable, f: &Value, sha: String| {
            let (Some(entry), Some(chunks)) = (hex(&f["entry"]), chunks(f)) else { return };
            by_entry.entry(entry).or_default().push(Known { module: module.into(), labels, executable, chunks, sha });
        };
        let exes = inv["executables"].as_array().cloned().unwrap_or_default();
        if !exes.is_empty() {
            use psx_media::disc::Disc;
            use psx_media::iso9660::FileSystem;
            let disc = Disc::open(root.join(inv["media"].as_str()?)).ok()?;
            let fs = FileSystem::read(&disc).ok()?;
            for e in &exes {
                let path = e["path"].as_str().unwrap_or_default();
                let Some(bytes) = fs.find(path).and_then(|f| fs.read_file(&disc, f).ok()) else { continue };
                let Ok(exe) = psx_media::PsxExe::parse(&bytes) else { continue };
                let code = Code { base: 0x8000_0000 | (exe.text_addr & 0x1F_FFFF), bytes: exe.payload(&bytes) };
                for f in e["functions"].as_array().into_iter().flatten() {
                    if let Some(sha) = chunks(f).and_then(|c| code.sha(&c)) {
                        add(path, vec![base_name(path)], true, f, sha);
                    }
                }
            }
        }
        for o in inv["overlays"].as_array().into_iter().flatten() {
            let name = o["name"].as_str().unwrap_or_default();
            let mut labels = vec![name.to_lowercase()];
            for p in [&o["file"]["path"], &o["file"]["unpacked"]["source"]] {
                if let Some(p) = p.as_str() {
                    labels.push(base_name(p));
                }
            }
            for f in o["functions"].as_array().into_iter().flatten() {
                if let Some(sha) = f["sha256"].as_str() {
                    add(name, labels.clone(), false, f, sha.into());
                }
            }
        }
        Some(Inventory { by_entry, present: HashMap::new() })
    }

    /// Whether a function name at `addr` (of `module`, or the executable's)
    /// applies to the analysed code.
    pub(crate) fn function(&self, code: &Code, addr: u32, module: Option<&str>) -> Bind {
        let all = self.by_entry.get(&addr).map(Vec::as_slice).unwrap_or_default();
        let known: Vec<&Known> = match module {
            Some(m) => {
                let m = m.to_lowercase();
                let k: Vec<_> = all.iter().filter(|k| k.labels.contains(&m)).collect();
                if k.is_empty() {
                    return Bind::Skip(format!("the inventory has no function of {} at {addr:08X}", module.unwrap()));
                }
                k
            }
            None => {
                let exe: Vec<_> = all.iter().filter(|k| k.executable).collect();
                if exe.is_empty() { all.iter().collect() } else { exe }
            }
        };
        if known.is_empty() {
            // A function the inventory missed: by its address.
            return Bind::Apply;
        }
        let modules: BTreeSet<&str> = known.iter().map(|k| k.module.as_str()).collect();
        if module.is_none() && modules.len() > 1 {
            return Bind::Skip(format!(
                "several overlays have a function at {addr:08X} ({}): say which with `in`",
                modules.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if known.iter().any(|k| code.sha(&k.chunks).is_some_and(|s| s == k.sha)) {
            Bind::Apply
        } else {
            Bind::Elsewhere
        }
    }

    /// Whether the analysis holds `module`'s code: most of its functions in
    /// the analysed memory are its own bytes.
    pub(crate) fn holds(&mut self, code: &Code, module: &str) -> bool {
        let m = module.to_lowercase();
        if let Some(&p) = self.present.get(&m) {
            return p;
        }
        let (mut same, mut other) = (0, 0);
        for k in self.by_entry.values().flatten().filter(|k| k.labels.contains(&m)) {
            match code.sha(&k.chunks) {
                Some(s) if s == k.sha => same += 1,
                Some(_) => other += 1,
                None => {}
            }
        }
        let p = same > 0 && same >= other;
        self.present.insert(m, p);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inventory(overlays: Value) -> Inventory {
        let mut by_entry: BTreeMap<u32, Vec<Known>> = BTreeMap::new();
        for o in overlays.as_array().unwrap() {
            let name = o["name"].as_str().unwrap();
            for f in o["functions"].as_array().unwrap() {
                by_entry.entry(hex(&f["entry"]).unwrap()).or_default().push(Known {
                    module: name.into(),
                    labels: vec![name.to_lowercase(), base_name(o["file"].as_str().unwrap())],
                    executable: o["exe"] == true,
                    chunks: chunks(f).unwrap(),
                    sha: f["sha256"].as_str().unwrap().into(),
                });
            }
        }
        Inventory { by_entry, present: HashMap::new() }
    }

    #[test]
    fn overlays_at_one_address_are_told_apart_by_their_bytes() {
        let a = [1u8, 0, 0, 0, 2, 0, 0, 0];
        let b = [3u8, 0, 0, 0, 4, 0, 0, 0];
        let f = |sha: &[u8]| json!([{"entry": "0x80100000", "end": "0x80100008", "sha256": crate::stamp::sha256(sha)}]);
        let mut inv = inventory(json!([
            {"name": "0x80100000 /RACE.OVL", "file": "/RACE.OVL", "functions": f(&a)},
            {"name": "0x80100000 /MENU.OVL", "file": "/MENU.OVL", "functions": f(&b)},
        ]));
        let race = Code { base: 0x8010_0000, bytes: &a };
        assert!(matches!(inv.function(&race, 0x8010_0000, Some("race.ovl")), Bind::Apply));
        assert!(matches!(inv.function(&race, 0x8010_0000, Some("MENU.OVL")), Bind::Elsewhere));
        assert!(matches!(inv.function(&race, 0x8010_0000, None), Bind::Skip(m) if m.contains("say which")));
        assert!(
            matches!(inv.function(&race, 0x8010_0000, Some("CAR.OVL")), Bind::Skip(m) if m.contains("no function of CAR.OVL"))
        );
        // A function the inventory does not know applies by its address.
        assert!(matches!(inv.function(&race, 0x8010_0004, None), Bind::Apply));
        let elsewhere = Code { base: 0x8020_0000, bytes: &a };
        assert!(matches!(inv.function(&elsewhere, 0x8010_0000, Some("RACE.OVL")), Bind::Elsewhere));
        assert!(inv.holds(&race, "RACE.OVL"));
        assert!(!inv.holds(&race, "MENU.OVL"));
    }

    #[test]
    fn a_name_without_a_module_is_the_executables() {
        let a = [1u8, 0, 0, 0];
        let b = [3u8, 0, 0, 0];
        let f = |sha: &[u8]| json!([{"entry": "0x80010000", "end": "0x80010004", "sha256": crate::stamp::sha256(sha)}]);
        let inv = inventory(json!([
            {"name": "/SCUS_944.88", "file": "/SCUS_944.88", "exe": true, "functions": f(&a)},
            {"name": "0x80010000 /GT2.OVL @ 30 (unpacked)", "file": "/GT2.OVL @ 30 (unpacked)", "functions": f(&b)},
        ]));
        assert!(matches!(inv.function(&Code { base: 0x8001_0000, bytes: &a }, 0x8001_0000, None), Bind::Apply));
        let ovl = Code { base: 0x8001_0000, bytes: &b };
        assert!(matches!(inv.function(&ovl, 0x8001_0000, None), Bind::Elsewhere));
        assert!(matches!(inv.function(&ovl, 0x8001_0000, Some("gt2.ovl")), Bind::Apply));
    }
}

//! `code symbols` on a disc: the debug files its build left on it, found by
//! name and by content in every file and in the sectors no file owns, and
//! those that describe the executable imported to `.psx/names/`.
use crate::Error;
use psx_media::PsxExe;
use psx_media::disc::Disc;
use psx_media::iso9660::FileSystem;
use psx_symbols::debug::{self, DebugNames};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

/// Where the executable's functions start: the inventory's, and words that
/// open a stack frame (`addiu sp,sp,-N`) or follow a `jr ra` and its delay
/// slot.
pub(crate) struct Starts {
    base: u32,
    words: Vec<u32>,
    known: BTreeSet<u32>,
}

impl Starts {
    pub(crate) fn new(root: &Path, exe_path: Option<&str>, exe: &PsxExe, bytes: &[u8]) -> Starts {
        let base = 0x8000_0000 | (exe.text_addr & 0x1F_FFFF);
        let words = exe.payload(bytes).as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
        let mut known = BTreeSet::new();
        let inv: Option<Value> =
            std::fs::read(root.join(".psx/inventory.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        for e in inv.iter().flat_map(|i| i["executables"].as_array().into_iter().flatten()) {
            if exe_path.is_some_and(|p| e["path"].as_str() == Some(p)) {
                for f in e["functions"].as_array().into_iter().flatten() {
                    if let Some(a) = f["entry"].as_str().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    {
                        known.insert(a);
                    }
                }
            }
        }
        Starts { base, words, known }
    }

    pub(crate) fn range(&self) -> std::ops::Range<u32> {
        self.base..self.base + self.words.len() as u32 * 4
    }

    pub(crate) fn is_start(&self, a: u32) -> bool {
        let a = 0x8000_0000 | (a & 0x1F_FFFF);
        if self.known.contains(&a) {
            return true;
        }
        if !a.is_multiple_of(4) || !self.range().contains(&a) {
            return false;
        }
        let i = ((a - self.base) / 4) as usize;
        let w = self.words[i];
        (w >> 16 == 0x27BD && w & 0x8000 != 0) || (i >= 2 && self.words[i - 2] == 0x03E0_0008)
    }
}

/// A debug file found: where, what kind, and whether it describes the
/// executable.
struct Found {
    at: String,
    kind: &'static str,
    by: &'static str,
    bytes: Vec<u8>,
}

/// Scans the disc and imports what describes the executable. `named`
/// gets every address a debug file names, so the PsyQ names give way.
pub(crate) fn scan(
    root: &Path,
    media: &Path,
    unpacked: &[(String, Vec<u8>)],
    out_dir: &Path,
    named: &mut BTreeSet<u32>,
) -> crate::Result<Value> {
    let disc = Disc::open(media)?;
    let fs = FileSystem::read(&disc)?;
    let mut found: Vec<Found> = Vec::new();
    let mut by_name = Vec::new();
    let mut paths = std::collections::BTreeMap::new();
    let mut gcc = BTreeSet::new();
    let mut owned: Vec<(u32, u32)> = Vec::new();
    let mut scanned = 0u64;
    let mut unreadable = Vec::new();
    let mut exes: Vec<(String, Starts)> = Vec::new();
    let mut look = |at: String, bytes: &[u8], found: &mut Vec<Found>| {
        let (p, g) = debug::strings(bytes);
        for p in p {
            paths.entry(p).or_insert_with(|| at.clone());
        }
        gcc.extend(g);
        for h in debug::scan(bytes) {
            // A debug file found inside another runs to the end of that one.
            found.push(Found { at: format!("{at}+0x{:X}", h.offset), kind: h.kind, by: "content", bytes: bytes[h.offset..].to_vec() });
        }
    };
    for e in fs.entries.iter().filter(|e| !e.is_dir) {
        owned.push((e.lba, e.lba + e.sectors()));
        let by = debug::debug_name(&e.path);
        if let Some(kind) = by.filter(|k| *k != "object") {
            by_name.push(json!({"path": e.path, "kind": kind, "bytes": e.size}));
        }
        if e.is_streamed() {
            continue;
        }
        match fs.read_file(&disc, e) {
            Ok(bytes) => {
                scanned += bytes.len() as u64;
                if by == Some("object") && (bytes.starts_with(b"LNK\x02") || bytes.starts_with(b"LIB\x01")) {
                    by_name.push(json!({"path": e.path, "kind": "object", "bytes": e.size}));
                }
                if bytes.starts_with(b"PS-X EXE") {
                    if let Ok(exe) = PsxExe::parse(&bytes) {
                        exes.push((e.path.clone(), Starts::new(root, Some(&e.path), &exe, &bytes)));
                    }
                }
                if let Some(kind @ ("sym" | "map" | "elf")) = debug::debug_name(&e.path) {
                    found.push(Found { at: e.path.clone(), kind, by: "name", bytes: bytes.clone() });
                }
                let before = found.len();
                look(e.path.clone(), &bytes, &mut found);
                // A file found by its name is not found again at its start.
                let named_here: Vec<&'static str> =
                    found[..before].iter().filter(|f| f.at == e.path).map(|f| f.kind).collect();
                let mut i = before;
                while i < found.len() {
                    if found[i].at == format!("{}+0x0", e.path) && named_here.contains(&found[i].kind) {
                        found.remove(i);
                    } else {
                        i += 1;
                    }
                }
            }
            Err(err) => unreadable.push(format!("{}: {err}", e.path)),
        }
    }
    for (name, bytes) in unpacked {
        scanned += bytes.len() as u64;
        look(name.clone(), bytes, &mut found);
    }
    // Sectors no file owns, in runs, read as Form 1 where they are.
    owned.sort_unstable();
    let end = fs.volume.volume_sectors.min(disc.end_lba());
    let mut free = Vec::new();
    let mut lba = 16 + 2;
    for &(s, e) in &owned {
        if s > lba {
            free.push((lba, s));
        }
        lba = lba.max(e);
    }
    if end > lba {
        free.push((lba, end));
    }
    let mut free_sectors = 0u32;
    for (s, e) in free {
        let mut run = Vec::new();
        for l in s..e {
            match disc.read_data(l) {
                Ok(d) => run.extend_from_slice(&d.data[..2048.min(d.data.len())]),
                Err(_) => break,
            }
        }
        free_sectors += (run.len() / 2048) as u32;
        if run.iter().any(|&b| b != 0) {
            look(format!("sectors {s}..{e}"), &run, &mut found);
        }
    }
    scanned += free_sectors as u64 * 2048;

    let mut files = Vec::new();
    for f in &found {
        let parsed = match f.kind {
            "sym" => debug::parse_sym(&f.bytes),
            "elf" => debug::parse_elf(&f.bytes),
            "map" => Ok(debug::parse_map(&String::from_utf8_lossy(&f.bytes))),
            _ => Err("an executable's format, not names".into()),
        };
        let mut row = json!({"at": f.at, "kind": f.kind, "found_by": f.by});
        match parsed {
            Err(e) => row["read"] = json!(e),
            Ok(mut d) => {
                // The executable most of its functions start functions in.
                let best = exes
                    .iter()
                    .map(|(p, st)| {
                        let mut t = d.clone();
                        t.split_functions(|a| st.range().contains(&a) && st.is_start(a));
                        (t.matching(st.range(), |a| st.is_start(a)), p, st)
                    })
                    .max_by_key(|((start, inside), _, _)| (*start, std::cmp::Reverse(*inside)));
                let Some(((start, inside), exe, starts)) = best else {
                    row["read"] = json!("no executable on the disc to check it against");
                    files.push(row);
                    continue;
                };
                d.split_functions(|a| starts.range().contains(&a) && starts.is_start(a));
                row["executable"] = json!(exe);
                row["functions"] = json!(d.functions.len());
                row["globals"] = json!(d.globals.len());
                row["types"] = json!(d.types.len());
                row["functions_at_function_starts"] = json!(format!("{start} of {inside} in {exe}"));
                if inside > 0 && start * 2 > inside {
                    let file = format!("{}.names", debug::ident(f.at.replace(['/', '\\'], "_").trim_start_matches('_')));
                    let path = out_dir.join(&file);
                    std::fs::create_dir_all(out_dir).map_err(|e| Error::io(out_dir, e))?;
                    let text = d.to_names(&format!("Imported by psx code symbols from {} on the disc", f.at), None, &|a| {
                        named.contains(&a)
                    });
                    std::fs::write(&path, text).map_err(|e| Error::io(&path, e))?;
                    named.extend(d.functions.iter().map(|f| f.address).chain(d.globals.iter().map(|g| g.0)));
                    row["imported"] = json!(path);
                } else {
                    row["imported"] = json!(null);
                    row["why_not"] = json!("it describes none of the disc's executables: most of its functions are not at function starts");
                }
                row["sources"] = json!(d.sources.iter().take(20).collect::<Vec<_>>());
            }
        }
        files.push(row);
    }
    Ok(json!({
        "media": media,
        "files_by_name": by_name,
        "debug_files": files,
        "bytes_scanned": scanned,
        "unowned_sectors_scanned": free_sectors,
        "unpacked_files_scanned": unpacked.len(),
        "unreadable": unreadable,
        "source_paths": {"count": paths.len(), "first": paths.iter().take(20).map(|(p, at)| json!({"path": p, "in": at})).collect::<Vec<_>>()},
        "compilers": gcc,
    }))
}

/// The PsyQ signatures' function names as a names file, leaving out what a
/// debug file named.
pub(crate) fn psyq_names(report: &psx_symbols::MatchReport, source: &str, out: &Path, named: &BTreeSet<u32>) -> crate::Result<Option<std::path::PathBuf>> {
    let mut d = DebugNames::default();
    let mut seen = BTreeSet::new();
    for s in &report.symbols {
        if s.kind == psx_symbols::Kind::Function && seen.insert(s.address) {
            d.functions.push(debug::DebugFunction { address: s.address, name: s.name.clone(), ..Default::default() });
        }
    }
    if d.functions.is_empty() {
        return Ok(None);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let text = d.to_names(&format!("PsyQ library functions in {source}, named by signature by psx code symbols"), None, &|a| {
        named.contains(&a)
    });
    std::fs::write(out, text).map_err(|e| Error::io(out, e))?;
    Ok(Some(out.to_path_buf()))
}

/// Where the last scan's result is kept, under the project root.
pub(crate) const RECORD: &str = ".psx/disc_debug.json";

/// Keeps a scan's result; `read` once code symbols has reported it.
pub(crate) fn record(root: &Path, scan: &Value, read: bool) -> crate::Result<()> {
    let p = root.join(RECORD);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
    }
    let v = json!({"read": read, "scan": scan});
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| Error::io(&p, e))
}

/// The debug files a scan found that code symbols has not reported yet.
pub(crate) fn unread(root: &Path) -> Vec<String> {
    let v: Option<Value> = std::fs::read(root.join(RECORD)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let Some(v) = v.filter(|v| v["read"] == false) else { return Vec::new() };
    let scan = &v["scan"];
    let mut out: Vec<String> = scan["debug_files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| Some(format!("{} ({})", f["at"].as_str()?, f["kind"].as_str()?)))
        .collect();
    for f in scan["files_by_name"].as_array().into_iter().flatten() {
        if let Some(p) = f["path"].as_str().filter(|p| !out.iter().any(|o| o.starts_with(&format!("{p} ")))) {
            out.push(format!("{p} ({})", f["kind"].as_str().unwrap_or("?")));
        }
    }
    out
}

/// Decompressed disc data to search: `dir`'s unpacked.json, else the files
/// the project's inventory was taken with, else none.
pub(crate) fn unpacked(media: &Path, dir: Option<&Path>) -> crate::Result<Vec<(String, Vec<u8>)>> {
    if let Some(dir) = dir {
        let disc = Disc::open(media)?;
        let fs = FileSystem::read(&disc)?;
        let files = crate::inventory::read_unpacked(dir, |p| fs.find(p).map(|e| e.size as u64))?;
        return Ok(files.into_iter().map(|u| (u.name, u.bytes)).collect());
    }
    let inv: Option<Value> = std::fs::read(".psx/inventory.json").ok().and_then(|b| serde_json::from_slice(&b).ok());
    Ok(inv
        .iter()
        .flat_map(|i| i["unpacked"].as_array().into_iter().flatten())
        .filter_map(|u| Some((u["name"].as_str()?.to_string(), std::fs::read(u["file"].as_str()?).ok()?)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cooked ISO holding `files` (name, bytes) in its root, then `free`
    /// in sectors no file owns.
    fn iso(files: &[(&str, Vec<u8>)], free: &[u8]) -> Vec<u8> {
        let record = |name: &[u8], lba: u32, size: u32, dir: bool| {
            let len = 33 + name.len() + (name.len() + 1) % 2;
            let mut r = vec![0u8; len];
            r[0] = len as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[6..10].copy_from_slice(&lba.to_be_bytes());
            r[10..14].copy_from_slice(&size.to_le_bytes());
            r[14..18].copy_from_slice(&size.to_be_bytes());
            r[18] = 99;
            r[19] = 1;
            r[20] = 1;
            r[25] = if dir { 2 } else { 0 };
            r[28] = 1;
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        };
        let mut lba = 19u32;
        let mut dir = record(&[0], 18, 2048, true);
        dir.extend(record(&[1], 18, 2048, true));
        let mut data = Vec::new();
        for (name, bytes) in files {
            dir.extend(record(format!("{name};1").as_bytes(), lba, bytes.len() as u32, false));
            let mut b = bytes.clone();
            b.resize(bytes.len().div_ceil(2048).max(1) * 2048, 0);
            lba += (b.len() / 2048) as u32;
            data.extend(b);
        }
        let mut free = free.to_vec();
        free.resize(free.len().div_ceil(2048) * 2048, 0);
        let total = lba + (free.len() / 2048) as u32;
        let mut out = vec![0u8; 16 * 2048];
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[80..84].copy_from_slice(&total.to_le_bytes());
        pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
        pvd[156..190].copy_from_slice(&record(&[0], 18, 2048, true));
        out.extend(pvd);
        let mut end = vec![0u8; 2048];
        end[0] = 255;
        end[1..6].copy_from_slice(b"CD001");
        out.extend(end);
        dir.resize(2048, 0);
        out.extend(dir);
        out.extend(data);
        out.extend(free);
        out
    }

    /// An EXE at 0x80010000 with functions at 0x80010000 and 0x80010100
    /// (each opening a frame) and plain words between.
    fn exe() -> Vec<u8> {
        let mut b = vec![0u8; 0x800];
        b[..8].copy_from_slice(b"PS-X EXE");
        b[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        b[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        b[0x1C..0x20].copy_from_slice(&0x800u32.to_le_bytes());
        let mut text = vec![0u8; 0x800];
        for at in [0, 0x100] {
            text[at..at + 4].copy_from_slice(&0x27BD_FFE8u32.to_le_bytes());
        }
        b.extend(text);
        b
    }

    #[test]
    fn debug_files_planted_on_a_disc_are_imported_when_they_describe_its_executable() {
        let dir = std::env::temp_dir().join(format!("psx-disc-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Matching: a SYM by name, naming the function at 0x80010000. Not
        // matching: a SYM inside a data file naming 0x80010008, mid-function.
        let mut wad = b"some data ".to_vec();
        wad.extend(psx_symbols::debug::testing::planted_sym(0x8001_0008));
        let map = b"  Address  Names in address order\r\n  80010100 draw_all\r\n  80010000 main\r\n".to_vec();
        let image = iso(
            &[("GAME.EXE", exe()), ("GAME.SYM", psx_symbols::debug::testing::planted_sym(0x8001_0000)), ("WAD.DAT", wad)],
            &map,
        );
        let media = dir.join("planted.iso");
        std::fs::write(&media, image).unwrap();
        let names = dir.join("names");
        let mut named = BTreeSet::new();
        let r = scan(&dir, &media, &[], &names, &mut named).unwrap();
        let files = r["debug_files"].as_array().unwrap();
        let at = |s: &str| files.iter().find(|f| f["at"].as_str().unwrap().starts_with(s)).unwrap();
        assert_eq!(at("/GAME.SYM")["found_by"], "name", "{r:#}");
        assert!(at("/GAME.SYM")["imported"].is_string(), "{r:#}");
        assert_eq!(at("/GAME.SYM")["executable"], "/GAME.EXE");
        assert_eq!(at("/WAD.DAT+0xA")["found_by"], "content");
        assert!(at("/WAD.DAT")["imported"].is_null(), "{r:#}");
        // The map in sectors no file owns names both functions.
        let map = at("sectors");
        assert_eq!(map["functions_at_function_starts"], "2 of 2 in /GAME.EXE", "{r:#}");
        assert_eq!(r["source_paths"]["first"][0]["path"], "C:\\CROC\\COLLIDE.C");
        // The imports parse together; the map gives way where the SYM named.
        let texts: Vec<(String, String)> = std::fs::read_dir(&names)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                (p.display().to_string(), std::fs::read_to_string(&p).unwrap())
            })
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        let n = psx_symbols::names::from_texts(&[], &texts);
        assert!(n.problems.is_empty(), "{:?}", n.problems);
        let mut f: Vec<_> = n.functions.iter().map(|f| (f.address, f.name.text.as_str())).collect();
        f.sort();
        assert_eq!(f, [(0x8001_0000, "world_collide"), (0x8001_0100, "draw_all")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

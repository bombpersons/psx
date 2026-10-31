//! The asset inventory (docs/port-design.md, section 4): each disc file
//! unknown, format found, imported, checked or unused.
//!
//! - **Format found**: a format doc (`formats/<id>.md`, TOML front matter
//!   between `+++` lines with the files it covers and the code that parses
//!   them) names it, or `assets scan` finds it whole as a standard format.
//! - **Imported**: the last `port check` at HEAD ran the importers on the
//!   disc, and an output's provenance names it.
//! - **Checked**: an importer test that compared the import with what the
//!   game built marked it (`import::checking`), and passed.
//! - **Unused**: `formats/unused.toml` lists it, with why.
//!
//! The boot executable, SYSTEM.CNF and the overlays the inventory reads
//! from disc files are code, not assets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Problem, entry_addr, problem, split_front};

pub const FORMATS: &str = "formats";
pub const UNUSED: &str = "formats/unused.toml";
/// What `port check` found of the importers, and the commit it judged.
pub const REPORT: &str = ".psx/port/assets.json";
/// Which disc files `assets scan` finds whole as standard formats, by disc.
const SCAN: &str = ".psx/port/assets-scan.json";
/// What the scan cache says of a PS-X executable: code, left out.
const EXE: &str = "PS-X EXE";

/// One format doc: `formats/<id>.md`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Disc files in this format: paths from `/`, where `*` matches any
    /// characters but `/` and `?` one.
    #[serde(default)]
    pub files: Vec<String>,
    /// The code that parses it, by function entry, as the inventory has it.
    #[serde(default)]
    pub loader: Vec<String>,
    /// The paragraphs after the front matter: the layout.
    #[serde(skip)]
    pub description: String,
    #[serde(skip)]
    pub file: String,
}

/// The format docs and the unused list, with what is wrong with them that
/// needs no disc.
pub(crate) fn read_formats(
    root: &Path,
    problems: &mut Vec<Problem>,
) -> crate::Result<(BTreeMap<String, Format>, BTreeMap<String, String>)> {
    let dir = root.join(FORMATS);
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(d) => d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(crate::Error::io(&dir, e)),
    };
    names.sort();
    let mut formats = BTreeMap::new();
    for name in names.iter().filter(|n| n.ends_with(".md") && *n != "README.md") {
        let file = format!("{FORMATS}/{name}");
        let text = std::fs::read_to_string(dir.join(name)).map_err(|e| crate::Error::io(dir.join(name), e))?;
        let parsed = split_front(&text).and_then(|(front, body)| {
            let mut f: Format = toml::from_str(&front).map_err(|e| format!("front matter: {}", e.message()))?;
            f.description = body.trim().to_string();
            Ok(f)
        });
        let mut f = match parsed {
            Ok(f) => f,
            Err(p) => {
                problems.push(problem(&file, p));
                continue;
            }
        };
        let stem = name.trim_end_matches(".md");
        if f.id != stem {
            problems.push(problem(&file, format!("its id is {:?}; a format's id is its file's name ({stem})", f.id)));
            continue;
        }
        if f.title.trim().is_empty() {
            problems.push(problem(&file, "no title"));
        }
        if f.files.is_empty() {
            problems.push(problem(&file, "files: none; the disc files in this format (/DIR/*.EXT)"));
        }
        for p in f.files.iter().filter(|p| !p.starts_with('/')) {
            problems.push(problem(&file, format!("files: {p:?} is not a disc path from /")));
        }
        for l in f.loader.iter().filter(|l| entry_addr(l).is_none()) {
            problems.push(problem(&file, format!("loader: {l:?} is not an address (0x8004A120)")));
        }
        if f.description.is_empty() {
            problems.push(problem(&file, "no description: the layout after the front matter"));
        }
        f.file = file;
        formats.insert(f.id.clone(), f);
    }
    let unused = match std::fs::read_to_string(root.join(UNUSED)) {
        Ok(text) => match toml::from_str::<BTreeMap<String, String>>(&text) {
            Ok(u) => u,
            Err(e) => {
                problems.push(problem(UNUSED, e.message()));
                BTreeMap::new()
            }
        },
        Err(_) => BTreeMap::new(),
    };
    for (path, why) in &unused {
        if !path.starts_with('/') {
            problems.push(problem(UNUSED, format!("{path:?} is not a disc path from /")));
        }
        if why.trim().is_empty() {
            problems.push(problem(UNUSED, format!("{path}: no reason: the runs that never load it")));
        }
    }
    Ok((formats, unused))
}

/// Whether `path` matches `pattern` (both from `/`, compared without case).
pub fn matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (s.first().is_some_and(|c| *c != b'/') && go(p, &s[1..])),
            (Some(b'?'), Some(c)) if *c != b'/' => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

/// A disc path as provenance or a person writes it: `/DIR/FILE.EXT`.
pub fn disc_path(s: &str) -> String {
    let s = s.split(';').next().unwrap_or(s).replace('\\', "/").to_ascii_uppercase();
    if s.starts_with('/') { s } else { format!("/{s}") }
}

/// What `port check` found: the importers' outputs' sources and the files
/// passing tests checked, at a commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub commit: String,
    pub clean: bool,
    pub at: String,
    pub imported: Vec<String>,
    pub checked: Vec<String>,
    #[serde(default)]
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Unknown,
    Found,
    Imported,
    Checked,
    Unused,
}

impl State {
    pub const ALL: [State; 5] = [State::Unknown, State::Found, State::Imported, State::Checked, State::Unused];

    pub fn name(self) -> &'static str {
        match self {
            State::Unknown => "unknown",
            State::Found => "format found",
            State::Imported => "imported",
            State::Checked => "checked",
            State::Unused => "unused",
        }
    }

    /// Nothing is left to do for the file.
    pub fn settled(self) -> bool {
        matches!(self, State::Checked | State::Unused)
    }
}

/// One disc file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub path: String,
    pub state: State,
    /// The format doc covering it, or the standard format it is.
    pub format: Option<String>,
}

/// The disc's files by state.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub files: Vec<Asset>,
    /// The last `port check`'s report, when it judged HEAD with no
    /// uncommitted changes; otherwise the commit it judged, if any.
    pub report: Option<Report>,
    pub stale: Option<String>,
    /// What the format docs and the unused list say that the disc does not
    /// bear out.
    pub problems: Vec<Problem>,
}

impl Inventory {
    pub fn count(&self, s: State) -> usize {
        self.files.iter().filter(|a| a.state == s).count()
    }

    pub fn first(&self, s: State) -> Option<&Asset> {
        self.files.iter().find(|a| a.state == s)
    }

    pub fn json(&self, formats: usize) -> Value {
        let states: BTreeMap<&str, usize> = State::ALL.iter().map(|s| (s.name(), self.count(*s))).collect();
        let list = |s: State| -> Vec<&str> {
            self.files.iter().filter(|a| a.state == s).take(10).map(|a| a.path.as_str()).collect()
        };
        let mut v = json!({
            "files": self.files.len(), "states": states, "formats": formats,
            "unknown": list(State::Unknown), "format_found": list(State::Found), "imported": list(State::Imported),
        });
        match (&self.report, &self.stale) {
            (Some(r), _) if !r.failures.is_empty() => v["check_failures"] = json!(r.failures),
            (None, Some(c)) => v["check"] = json!(format!("the last psx port check judged {c}, not HEAD")),
            (None, None) => v["check"] = json!("no psx port check has run the importers"),
            _ => {}
        }
        v
    }
}

/// The disc's files, which are code, and which `assets scan` finds whole
/// as standard formats (cached by the disc's file list).
fn disc_files(root: &Path, media: &Path, inv: Option<&Value>) -> Result<Vec<(String, Option<String>)>, String> {
    use psx_media::iso9660::{FileSystem, xa};
    let disc = psx_media::disc::Disc::open(media).map_err(|e| format!("{}: {e}", media.display()))?;
    let fs = FileSystem::read(&disc).map_err(|e| format!("{}: {e}", media.display()))?;
    let mut code: BTreeSet<String> = ["/SYSTEM.CNF".to_string()].into();
    let boot = fs.find("/SYSTEM.CNF").and_then(|e| fs.read_file(&disc, e).ok());
    if let Some(b) = boot.and_then(|b| psx_media::system_cnf::SystemCnf::parse(&b).boot) {
        let name = b.rsplit(['\\', ':']).next().unwrap_or(&b).to_string();
        code.insert(disc_path(&name));
    }
    for v in inv.iter().flat_map(|inv| inv["executables"].as_array().cloned().unwrap_or_default()) {
        code.extend(v["path"].as_str().map(disc_path));
    }
    for v in inv.iter().flat_map(|inv| inv["overlays"].as_array().cloned().unwrap_or_default()) {
        code.extend(v["file"]["path"].as_str().filter(|p| p.starts_with('/')).map(disc_path));
    }
    let entries: Vec<_> = fs.entries.iter().filter(|e| !e.is_dir && !code.contains(&disc_path(&e.path))).collect();
    let key = {
        use sha2::{Digest, Sha256};
        let list: Vec<String> = entries.iter().map(|e| format!("{} {} {}", e.path, e.lba, e.size)).collect();
        format!("{:x}", Sha256::digest(list.join("\n")))
    };
    let cache = root.join(SCAN);
    let kept: Option<BTreeMap<String, String>> = std::fs::read(&cache)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|v| v["key"] == key.as_str())
        .and_then(|v| serde_json::from_value(v["standard"].clone()).ok());
    let standard = match kept {
        Some(s) => s,
        None => {
            let mut s = BTreeMap::new();
            for e in &entries {
                let found = if e.xa_attributes.is_some_and(|a| a & (xa::MODE2_FORM2 | xa::INTERLEAVED) != 0) {
                    whole_stream(&disc, e.lba, e.sectors())
                } else {
                    fs.read_file(&disc, e).ok().and_then(|b| {
                        // An executable is code, whether or not the inventory has it.
                        if b.starts_with(b"PS-X EXE") { Some(EXE.to_string()) } else { whole_standard(&b) }
                    })
                };
                if let Some(k) = found {
                    s.insert(disc_path(&e.path), k);
                }
            }
            let v = json!({"key": key, "standard": s});
            if let Some(d) = cache.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = crate::write_atomic(&cache, &serde_json::to_vec(&v).unwrap_or_default());
            s
        }
    };
    Ok(entries
        .iter()
        .map(|e| {
            let p = disc_path(&e.path);
            let s = standard.get(&p).cloned();
            (p, s)
        })
        .filter(|(_, s)| s.as_deref() != Some(EXE))
        .collect())
}

/// The standard format a file is wholly made of (one or more TIM, TMD, VAB
/// or VAG back to back, zeros between and after), if it is.
fn whole_standard(bytes: &[u8]) -> Option<String> {
    let hits = psx_assets::scan::scan(bytes, Default::default());
    let mut at = 0;
    let mut kinds = BTreeSet::new();
    for h in &hits {
        if h.size == 0 || bytes[at..h.offset].iter().any(|b| *b != 0) {
            return None;
        }
        kinds.insert(format!("{:?}", h.kind).to_uppercase());
        at = h.offset + h.size;
    }
    let tail = bytes.get(at..).unwrap_or_default();
    (!hits.is_empty() && tail.iter().all(|b| *b == 0)).then(|| kinds.into_iter().collect::<Vec<_>>().join("+"))
}

/// The standard formats a streamed file is wholly made of: XA audio
/// sectors and STR (MDEC) video sectors, empty sectors between; None when
/// any sector holds anything else (the game's own streamed data).
fn whole_stream(disc: &psx_media::disc::Disc, lba: u32, sectors: u32) -> Option<String> {
    let (mut xa, mut str) = (false, false);
    for i in 0..sectors {
        let s = disc.read_data(lba + i).ok()?;
        let submode = s.subheader.map_or(0, |h| h[2]);
        if submode & 0x04 != 0 {
            xa = true;
        } else if psx_media::str_video::StrHeader::parse(&s.data).is_some_and(|h| h.is_video()) {
            str = true;
        } else if s.data.iter().any(|b| *b != 0) {
            return None;
        }
    }
    match (str, xa) {
        (true, true) => Some("STR+XA".into()),
        (true, false) => Some("STR".into()),
        (false, true) => Some("XA".into()),
        _ => None,
    }
}

/// The last `port check`'s report, and whether it judged `head`.
fn report(root: &Path, head: Option<&str>) -> (Option<Report>, Option<String>) {
    let Some(r) = std::fs::read(root.join(REPORT)).ok().and_then(|b| serde_json::from_slice::<Report>(&b).ok()) else {
        return (None, None);
    };
    if r.clean && Some(r.commit.as_str()) == head { (Some(r), None) } else { (None, Some(r.commit)) }
}

/// The disc's files by state, for the workspace `root` whose formats and
/// unused list `port` read.
pub fn inventory(
    root: &Path,
    port: &super::Port,
    inv: Option<&Value>,
    head: Option<&str>,
) -> Result<Inventory, String> {
    let media = crate::context::resolve(root, None, None, None, false)
        .ok()
        .and_then(|r| r.media)
        .ok_or("no media: the workspace's disc image (psx context --image ... --save)")?;
    let files = disc_files(root, &media, inv)?;
    let (report, stale) = report(root, head);
    let imported: BTreeSet<String> = report.iter().flat_map(|r| r.imported.iter().map(|p| disc_path(p))).collect();
    let checked: BTreeSet<String> = report.iter().flat_map(|r| r.checked.iter().map(|p| disc_path(p))).collect();
    let unused: BTreeMap<String, &String> = port.unused.iter().map(|(p, w)| (disc_path(p), w)).collect();
    let mut problems = Vec::new();
    for f in port.formats.values() {
        for p in f.files.iter().filter(|p| !files.iter().any(|(path, _)| matches(p, path))) {
            problems.push(problem(&f.file, format!("files: {p} matches no disc file (code files are not assets)")));
        }
    }
    let mut out = Vec::new();
    for (path, standard) in files {
        let doc = port.formats.values().find(|f| f.files.iter().any(|p| matches(p, &path)));
        if let (true, Some(d)) = (unused.contains_key(&path), doc) {
            problems.push(problem(UNUSED, format!("{path} is unused, but {} covers it", d.file)));
        }
        let format = doc.map(|f| f.id.clone()).or(standard);
        let state = if unused.contains_key(&path) {
            State::Unused
        } else if checked.contains(&path) {
            State::Checked
        } else if imported.contains(&path) {
            State::Imported
        } else if format.is_some() {
            State::Found
        } else {
            State::Unknown
        };
        out.push(Asset { path, state, format });
    }
    for p in unused.keys().filter(|p| !out.iter().any(|a| &a.path == *p)) {
        problems.push(problem(UNUSED, format!("{p} is not an asset file on the disc")));
    }
    Ok(Inventory { files: out, report, stale, problems })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_within_a_directory() {
        assert!(matches("/MAPS/*.WLD", "/MAPS/L1_1.WLD"));
        assert!(matches("/maps/l?_1.wld", "/MAPS/L1_1.WLD"));
        assert!(!matches("/*.WLD", "/MAPS/L1_1.WLD"));
        assert!(matches("/*/*.WLD", "/MAPS/L1_1.WLD"));
        assert!(!matches("/MAPS/*.WLD", "/MAPS/L1_1.WLDX"));
        assert_eq!(disc_path("maps\\l1_1.wld;1"), "/MAPS/L1_1.WLD");
    }

    #[test]
    fn a_file_is_standard_when_standard_formats_fill_it() {
        // A 16-bit TIM, 4x2 pixels.
        let mut tim = Vec::new();
        for v in [0x10u32, 2, 12 + 2 * 8] {
            tim.extend(v.to_le_bytes());
        }
        for v in [0u16, 0, 4, 2] {
            tim.extend(v.to_le_bytes());
        }
        for i in 0..8u16 {
            tim.extend((0x10 + i).to_le_bytes());
        }
        let mut two = tim.clone();
        two.extend(&tim);
        two.extend([0u8; 100]);
        assert_eq!(whole_standard(&two).as_deref(), Some("TIM"));
        let mut not = tim.clone();
        not.extend([1u8; 8]);
        assert_eq!(whole_standard(&not), None);
        assert_eq!(whole_standard(&[0u8; 64]), None);
    }
}

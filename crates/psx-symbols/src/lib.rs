//! Library signature matching and symbol tables for PlayStation executables.
//!
//! Signatures are read from a local directory in the `psx_psyq_signatures`
//! JSON layout (`<root>/<version>/<LIB>.json`, each an array of
//! `{name, sig: "AA ?? ..", labels: [{name, offset}]}` objects). They are
//! derived from Sony's PsyQ libraries, so they are *not* bundled; supply them
//! locally.
//!
//! A match means the object's concrete bytes appear verbatim at an
//! instruction-aligned address. Short or wildcard-heavy objects are skipped
//! (`min_concrete`) and objects that match in more than one place are reported
//! as ambiguous instead of guessed.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
struct RawLabel {
    name: String,
    offset: u32,
}

#[derive(Deserialize)]
struct RawObject {
    name: String,
    /// Absent for data-only objects.
    #[serde(default)]
    sig: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Label {
    pub name: String,
    pub offset: u32,
}

#[derive(Clone, Debug)]
pub struct ObjectSig {
    pub version: String,
    pub library: String,
    pub object: String,
    /// `None` = wildcard byte.
    pub pattern: Vec<Option<u8>>,
    pub labels: Vec<Label>,
    pub concrete: usize,
}

impl ObjectSig {
    pub fn source(&self) -> String {
        format!("psyq:{}/{}/{}", self.version, self.library, self.object)
    }
}

fn parse_sig(s: &str) -> Result<Vec<Option<u8>>> {
    s.split_whitespace()
        .map(|t| {
            if t == "??" {
                Ok(None)
            } else {
                u8::from_str_radix(t, 16).map(Some).map_err(|_| Error(format!("bad signature byte {t:?}")))
            }
        })
        .collect()
}

/// Load every `<root>/<version>/*.json`.
pub fn load_dir(root: &Path) -> Result<Vec<ObjectSig>> {
    let mut out = Vec::new();
    let mut versions: Vec<_> = std::fs::read_dir(root)
        .map_err(|e| Error(format!("{}: {e}", root.display())))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()))
        .collect();
    versions.sort_by_key(|e| e.file_name());
    for v in versions {
        let version = v.file_name().to_string_lossy().into_owned();
        let mut files: Vec<_> =
            std::fs::read_dir(v.path()).map_err(|e| Error(e.to_string()))?.filter_map(|e| e.ok()).collect();
        files.sort_by_key(|e| e.file_name());
        for f in files {
            let name = f.file_name().to_string_lossy().into_owned();
            let Some(library) = name.strip_suffix(".json") else { continue };
            let text = std::fs::read_to_string(f.path()).map_err(|e| Error(format!("{name}: {e}")))?;
            let objs: Vec<RawObject> =
                serde_json::from_str(&text).map_err(|e| Error(format!("{version}/{name}: {e}")))?;
            for o in objs.into_iter().filter(|o| !o.sig.trim().is_empty()) {
                let pattern = parse_sig(&o.sig)?;
                let concrete = pattern.iter().filter(|b| b.is_some()).count();
                out.push(ObjectSig {
                    version: version.clone(),
                    library: library.to_string(),
                    object: o.name,
                    pattern,
                    labels: o.labels.into_iter().map(|l| Label { name: l.name, offset: l.offset }).collect(),
                    concrete,
                });
            }
        }
    }
    Ok(out)
}

/// Aligned-word index of a code image for fast anchored search.
pub struct CodeIndex<'a> {
    code: &'a [u8],
    words: HashMap<u32, Vec<u32>>,
}

impl<'a> CodeIndex<'a> {
    pub fn new(code: &'a [u8]) -> Self {
        let mut words: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, w) in code.as_chunks::<4>().0.iter().enumerate() {
            words.entry(u32::from_le_bytes(*w)).or_default().push(i as u32 * 4);
        }
        CodeIndex { code, words }
    }

    /// Offsets (into `code`) where `sig` matches, instruction-aligned.
    pub fn find(&self, sig: &ObjectSig) -> Vec<u32> {
        let p = &sig.pattern;
        // Anchor on the rarest fully concrete aligned word.
        let mut best: Option<(usize, &Vec<u32>)> = None;
        for k in (0..p.len().saturating_sub(3)).step_by(4) {
            let (Some(a), Some(b), Some(c), Some(d)) = (p[k], p[k + 1], p[k + 2], p[k + 3]) else { continue };
            let w = u32::from_le_bytes([a, b, c, d]);
            let Some(hits) = self.words.get(&w) else { return Vec::new() };
            if best.is_none_or(|(_, h)| hits.len() < h.len()) {
                best = Some((k, hits));
            }
        }
        let Some((k, hits)) = best else { return Vec::new() };
        hits.iter()
            .filter_map(|&pos| {
                let start = pos.checked_sub(k as u32)? as usize;
                let window = self.code.get(start..start + p.len())?;
                window.iter().zip(p).all(|(b, m)| m.is_none_or(|m| m == *b)).then_some(start as u32)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An exported label (function entry).
    Function,
    /// A compiler-generated local label (`loc_`, `text_`).
    Local,
}

#[derive(Clone, Debug, Serialize)]
pub struct Symbol {
    pub address: u32,
    pub name: String,
    pub kind: Kind,
    /// Provenance, e.g. `psyq:410/LIBETC.LIB/PAD.OBJ`.
    pub source: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObjectMatch {
    pub source: String,
    pub address: u32,
    pub size: u32,
    pub concrete_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct VersionScore {
    pub version: String,
    pub objects: usize,
    pub unique_matches: usize,
    pub matched_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct MatchReport {
    pub base: u32,
    pub code_bytes: usize,
    pub min_concrete: usize,
    pub versions: Vec<VersionScore>,
    pub chosen_version: Option<String>,
    pub objects: Vec<ObjectMatch>,
    /// Objects of the chosen version that matched more than once (not used).
    pub ambiguous: Vec<String>,
    pub symbols: Vec<Symbol>,
}

/// Match `sigs` against `code` loaded at `base`.
///
/// The version with the most unique matches is chosen; its objects come first.
/// Objects from other versions are then added where their range does not
/// overlap an accepted one. Their `source` names the version, so mixed
/// toolchains stay visible.
pub fn match_code(code: &[u8], base: u32, sigs: &[ObjectSig], min_concrete: usize) -> MatchReport {
    let index = CodeIndex::new(code);
    let mut by_version: HashMap<&str, Vec<(usize, Vec<u32>)>> = HashMap::new();
    for (i, s) in sigs.iter().enumerate() {
        if s.concrete < min_concrete {
            continue;
        }
        let hits = index.find(s);
        if !hits.is_empty() {
            by_version.entry(&s.version).or_default().push((i, hits));
        }
    }
    let mut versions: Vec<VersionScore> = by_version
        .iter()
        .map(|(v, ms)| {
            let unique: Vec<_> = ms.iter().filter(|(_, h)| h.len() == 1).collect();
            VersionScore {
                version: v.to_string(),
                objects: sigs.iter().filter(|s| s.version == *v && s.concrete >= min_concrete).count(),
                unique_matches: unique.len(),
                matched_bytes: unique.iter().map(|(i, _)| sigs[*i].pattern.len()).sum(),
            }
        })
        .collect();
    versions.sort_by(|a, b| b.matched_bytes.cmp(&a.matched_bytes).then(b.version.cmp(&a.version)));
    let chosen = versions.first().map(|v| v.version.clone());

    let mut order: Vec<&str> = versions.iter().map(|v| v.version.as_str()).collect();
    order.dedup();
    let mut taken: Vec<(u32, u32)> = Vec::new();
    let mut objects = Vec::new();
    let mut symbols = Vec::new();
    let mut ambiguous = Vec::new();
    for v in order {
        let mut ms: Vec<&(usize, Vec<u32>)> = by_version.get(v).map(|m| m.iter().collect()).unwrap_or_default();
        // Longer, more specific objects first.
        ms.sort_by_key(|(i, _)| std::cmp::Reverse(sigs[*i].concrete));
        for (i, hits) in ms {
            let s = &sigs[*i];
            if hits.len() != 1 {
                if Some(v) == chosen.as_deref() {
                    ambiguous.push(format!("{} ({} places)", s.source(), hits.len()));
                }
                continue;
            }
            let (lo, hi) = (hits[0], hits[0] + s.pattern.len() as u32);
            if taken.iter().any(|&(a, b)| lo < b && hi > a) {
                continue;
            }
            taken.push((lo, hi));
            objects.push(ObjectMatch {
                source: s.source(),
                address: base.wrapping_add(lo),
                size: s.pattern.len() as u32,
                concrete_bytes: s.concrete,
            });
            for l in &s.labels {
                let local = l.name.starts_with("loc_") || l.name.starts_with("text_");
                symbols.push(Symbol {
                    address: base.wrapping_add(lo + l.offset),
                    name: l.name.clone(),
                    kind: if local { Kind::Local } else { Kind::Function },
                    source: s.source(),
                });
            }
        }
    }
    objects.sort_by_key(|o| o.address);
    symbols.sort_by_key(|s| s.address);
    MatchReport { base, code_bytes: code.len(), min_concrete, versions, chosen_version: chosen, objects, ambiguous, symbols }
}

/// The parts of a serialised [`MatchReport`] needed to rebuild a symbol table.
#[derive(Clone, Debug, Deserialize)]
pub struct MatchReportIn {
    pub symbols: Vec<SymbolIn>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SymbolIn {
    pub address: u32,
    pub name: String,
    pub kind: String,
}

/// Sorted function symbols with nearest-preceding lookup.
#[derive(Clone, Debug, Default)]
pub struct SymbolTable {
    entries: Vec<(u32, String)>,
    bodies: HashMap<u32, Vec<(u32, u32)>>,
}

impl SymbolTable {
    pub fn from_symbols<S: AsSymbol>(symbols: &[S]) -> Self {
        let mut entries: Vec<(u32, String)> =
            symbols.iter().filter(|s| s.is_function()).map(|s| (s.addr(), s.label().to_string())).collect();
        entries.sort();
        entries.dedup_by_key(|e| e.0);
        SymbolTable { entries, bodies: HashMap::new() }
    }

    /// Restrict a function to its actual half-open body ranges.
    pub fn set_body(&mut self, entry: u32, ranges: Vec<(u32, u32)>) {
        self.bodies.insert(entry, ranges);
    }

    /// `name+0xOFF` for the function containing `addr`, if any precedes it
    /// within `max_distance` bytes.
    pub fn describe(&self, addr: u32, max_distance: u32) -> Option<String> {
        let (a, name) = self.entries[..self.entries.partition_point(|e| e.0 <= addr)]
            .iter()
            .rev()
            .find(|(a, _)| self.bodies.get(a).is_none_or(|ranges| ranges.iter().any(|&(lo, hi)| lo <= addr && addr < hi)))?;
        let off = addr - a;
        (off <= max_distance).then(|| if off == 0 { name.clone() } else { format!("{name}+0x{off:X}") })
    }

    /// Parse `ADDRESS NAME` lines (the `.sym` format written by [`to_sym`]).
    /// Several tables can be merged with [`SymbolTable::extend`].
    pub fn from_sym_text(text: &str) -> Self {
        let mut t = SymbolTable::default();
        t.extend_sym_text(text);
        t
    }

    pub fn extend_sym_text(&mut self, text: &str) {
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if let (Some(a), Some(n)) = (it.next(), it.next()) {
                if let Ok(a) = u32::from_str_radix(a.trim_start_matches("0x"), 16) {
                    self.entries.push((a, n.to_string()));
                }
            }
        }
        self.entries.sort();
        self.entries.dedup_by_key(|e| e.0);
    }

    /// Address of the function `name` and where the next known function starts.
    pub fn find(&self, name: &str) -> Option<(u32, Option<u32>)> {
        let k = self.entries.iter().position(|(_, n)| n == name)?;
        let start = self.entries[k].0;
        let end = self
            .bodies
            .get(&start)
            .and_then(|r| r.iter().map(|r| r.1).max())
            .or_else(|| self.entries[k + 1..].iter().map(|e| e.0).find(|&a| a > start));
        Some((start, end))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub trait AsSymbol {
    fn addr(&self) -> u32;
    fn label(&self) -> &str;
    fn is_function(&self) -> bool;
}

impl AsSymbol for Symbol {
    fn addr(&self) -> u32 {
        self.address
    }
    fn label(&self) -> &str {
        &self.name
    }
    fn is_function(&self) -> bool {
        self.kind == Kind::Function
    }
}

impl AsSymbol for SymbolIn {
    fn addr(&self) -> u32 {
        self.address
    }
    fn label(&self) -> &str {
        &self.name
    }
    fn is_function(&self) -> bool {
        self.kind == "function"
    }
}

/// `NAME ADDRESS f` lines for Ghidra's ImportSymbolsScript.py (functions get `f`, labels `l`).
pub fn to_ghidra(symbols: &[Symbol]) -> String {
    symbols
        .iter()
        .map(|s| format!("{} {:08X} {}\n", s.name, s.address, if s.kind == Kind::Function { "f" } else { "l" }))
        .collect()
}

/// `ADDRESS NAME` lines (no$psx / generic .sym style), functions only.
pub fn to_sym(symbols: &[Symbol]) -> String {
    symbols.iter().filter(|s| s.kind == Kind::Function).map(|s| format!("{:08X} {}\n", s.address, s.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(version: &str, object: &str, sig: &str, labels: &[(&str, u32)]) -> ObjectSig {
        let pattern = parse_sig(sig).unwrap();
        ObjectSig {
            version: version.into(),
            library: "LIBTEST.LIB".into(),
            object: object.into(),
            concrete: pattern.iter().filter(|b| b.is_some()).count(),
            pattern,
            labels: labels.iter().map(|(n, o)| Label { name: n.to_string(), offset: *o }).collect(),
        }
    }

    #[test]
    fn finds_unique_aligned_matches_with_wildcards_and_skips_ambiguous() {
        let mut code = vec![0u8; 64];
        code[16..28].copy_from_slice(&[0xE8, 0xFF, 0xBD, 0x27, 0x34, 0x12, 0x01, 0x3C, 0x10, 0x00, 0xBF, 0xAF]);
        // Same bytes twice for the ambiguous object.
        code[40..44].copy_from_slice(&[1, 2, 3, 4]);
        code[48..52].copy_from_slice(&[1, 2, 3, 4]);
        let sigs = vec![
            sig("410", "PAD.OBJ", "E8 FF BD 27 ?? ?? 01 3C 10 00 BF AF", &[("PadInit", 0), ("loc_8", 8)]),
            sig("410", "DUP.OBJ", "01 02 03 04", &[("Dup", 0)]),
        ];
        let r = match_code(&code, 0x8001_0000, &sigs, 4);
        assert_eq!(r.chosen_version.as_deref(), Some("410"));
        assert_eq!(r.objects.len(), 1);
        assert_eq!(r.objects[0].address, 0x8001_0010);
        assert_eq!(r.ambiguous.len(), 1);
        let t = SymbolTable::from_symbols(&r.symbols);
        assert_eq!(t.len(), 1, "local labels are not functions");
        assert_eq!(t.describe(0x8001_0018, 0x100).as_deref(), Some("PadInit+0x8"));
        assert!(to_ghidra(&r.symbols).contains("PadInit 80010010 f"));
    }
}

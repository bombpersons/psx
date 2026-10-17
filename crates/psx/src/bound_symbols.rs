//! Portable, byte-bound Ghidra names. Ranges are canonical KSEG0, end exclusive.
use std::path::{Path, PathBuf};

use psx_media::{
    disc::sha256_bytes,
    memmap::{resolve, Region},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub format: u32,
    pub analysis: PathBuf,
    pub revision: u64,
    pub input: PathBuf,
    pub input_sha256: String,
    pub base: u32,
    pub file_offset: usize,
    pub size: usize,
    pub functions: Vec<Function>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub address: u32,
    pub name: String,
    pub body: Vec<Range>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: u32,
    pub end: u32,
    pub sha256: String,
}

pub fn canonical(addr: u32) -> Option<u32> {
    let r = resolve(addr);
    (r.region == Region::MainRam).then_some(0x8000_0000 | r.offset)
}

pub struct Loaded {
    pub document: Document,
    pub bytes: Vec<u8>,
    // Sorted body intervals and their prefix maximum end, for bounded PC lookup.
    intervals: Vec<(u32, u32, usize, u32)>,
}
impl Loaded {
    pub fn load(mut document: Document, parent: &Path) -> Result<Self, String> {
        if document.format != 2 {
            return Err("unsupported bound symbol format".into());
        }
        if !document.input.is_absolute() {
            document.input = parent.join(&document.input);
        }
        if std::fs::metadata(&document.input).map_err(|e| e.to_string())?.len() > 64*1024*1024 {
            return Err("symbol input exceeds 64 MiB".into());
        }
        let bytes = std::fs::read(&document.input).map_err(|e| e.to_string())?;
        if sha256_bytes(&bytes) != document.input_sha256 {
            return Err("symbol input hash mismatch".into());
        }
        let end = document
            .base
            .checked_add(u32::try_from(document.size).map_err(|_| "invalid symbol mapping")?)
            .filter(|&end| end <= 0x8020_0000)
            .ok_or("invalid symbol mapping")?;
        if canonical(document.base) != Some(document.base) || document.size == 0 {
            return Err("symbol mapping must be canonical main RAM".into());
        }
        let payload = document
            .file_offset
            .checked_add(document.size)
            .and_then(|e| bytes.get(document.file_offset..e))
            .ok_or("symbol mapping exceeds copied input")?;
        let mut entries = std::collections::HashSet::new();
        for f in &document.functions {
            if f.name.is_empty() || f.name.chars().any(char::is_whitespace) || f.body.is_empty() || !entries.insert(f.address) || f.address & 3 != 0 {
                return Err("invalid or duplicate bound function".into());
            }
            let mut previous = document.base;
            for r in &f.body {
                if r.start < previous || r.end <= r.start || r.end > end {
                    return Err("invalid function body range".into());
                }
                let bytes = &payload[(r.start - document.base) as usize..(r.end - document.base) as usize];
                if sha256_bytes(bytes) != r.sha256 {
                    return Err("function body hash mismatch".into());
                }
                previous = r.end;
            }
            if !f.body.iter().any(|r| r.start <= f.address && f.address < r.end) {
                return Err("function entry is outside its body".into());
            }
        }
        let mut intervals: Vec<_> =
            document.functions.iter().enumerate().flat_map(|(i, f)| f.body.iter().map(move |r| (r.start, r.end, i, 0))).collect();
        intervals.sort_unstable();
        let mut max_end = 0;
        for interval in &mut intervals {
            max_end = max_end.max(interval.1);
            interval.3 = max_end;
        }
        Ok(Self { document, bytes: payload.to_vec(), intervals })
    }

    pub fn table(&self) -> psx_symbols::SymbolTable {
        let mut table = psx_symbols::SymbolTable::from_sym_text(
            &self.document.functions.iter().map(|f| format!("{:08X} {}\n", f.address, f.name)).collect::<String>(),
        );
        for f in &self.document.functions {
            table.set_body(f.address, f.body.iter().map(|r| (r.start, r.end)).collect());
        }
        table
    }

    /// Check every byte in the containing function, including disjoint bodies.
    pub fn describe(&self, pc: u32, image_base: u32, image: &[u8]) -> Option<String> {
        let pc = canonical(pc)?;
        let i = self.intervals.partition_point(|r| r.0 <= pc);
        let interval = self.intervals[..i].iter().rev().take_while(|r| r.3 > pc).find(|r| pc < r.1)?;
        let f = &self.document.functions[interval.2];
        for r in &f.body {
            let lo = r.start.checked_sub(image_base)? as usize;
            let hi = r.end.checked_sub(image_base)? as usize;
            let original = &self.bytes[(r.start - self.document.base) as usize..(r.end - self.document.base) as usize];
            if image.get(lo..hi) != Some(original) {
                return None;
            }
        }
        let off = pc.checked_sub(f.address)?;
        Some(if off == 0 { f.name.clone() } else { format!("{}+0x{off:X}", f.name) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, Document) {
        let path = std::env::temp_dir().join(format!("psx-bound-{}-{}", std::process::id(), crate::instance_suffix()));
        let bytes = vec![7; 256];
        std::fs::write(&path, &bytes).unwrap();
        let body = vec![
            Range { start: 0x80010000, end: 0x80010040, sha256: sha256_bytes(&bytes[..64]) },
            Range { start: 0x80010080, end: 0x80010090, sha256: sha256_bytes(&bytes[128..144]) },
        ];
        let doc = Document {
            format: 2,
            analysis: PathBuf::from("analysis.json"),
            revision: 1,
            input: path.clone(),
            input_sha256: sha256_bytes(&bytes),
            base: 0x80010000,
            file_offset: 0,
            size: 256,
            functions: vec![Function { address: 0x80010000, name: "studied_writer".into(), body }],
        };
        (path, doc)
    }
    #[test]
    fn names_require_all_body_bytes_and_never_fill_gaps_or_other_regions() {
        let (path, doc) = fixture();
        let loaded = Loaded::load(doc, Path::new(".")).unwrap();
        let mut ram = vec![0; 0x200000];
        ram[0x10000..0x10100].copy_from_slice(&loaded.bytes);
        assert_eq!(loaded.describe(0xA0610004, 0x80000000, &ram).as_deref(), Some("studied_writer+0x4"));
        assert!(loaded.describe(0x1f800004, 0x80000000, &ram).is_none());
        assert!(loaded.describe(0xbfc10004, 0x80000000, &ram).is_none());
        assert!(loaded.describe(0x80010060, 0x80000000, &ram).is_none());
        assert!(loaded.table().describe(0x80010060, 0x4000).is_none());
        assert_eq!(loaded.table().find("studied_writer"), Some((0x80010000, Some(0x80010090))));
        ram[0x100f0] ^= 1; // unrelated data does not invalidate code
        assert!(loaded.describe(0x80010004, 0x80000000, &ram).is_some());
        ram[0x10088] ^= 1; // changed far outside the queried instruction window
        assert!(loaded.describe(0x80010004, 0x80000000, &ram).is_none());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn edited_inputs_and_malformed_hashes_or_ranges_are_rejected() {
        let (path, doc) = fixture();
        let mut bad = doc.clone();
        bad.functions[0].body[0].sha256 = "wrong".into();
        assert!(Loaded::load(bad, Path::new(".")).is_err());
        let mut bad = doc.clone();
        bad.functions[0].body[0].start -= 4;
        assert!(Loaded::load(bad, Path::new(".")).is_err());
        let mut bad = doc.clone();
        bad.functions[0].body[0].end += 0x200;
        assert!(Loaded::load(bad, Path::new(".")).is_err());
        std::fs::write(&path, vec![8; 256]).unwrap();
        assert!(Loaded::load(doc, Path::new(".")).is_err());
        std::fs::remove_file(path).unwrap();
    }
}

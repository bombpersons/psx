//! Symbol loading and residency checks shared by runtime and static tools.
use crate::bound_symbols;
use psx_media::{MediaError, PsxExe};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Symbols for one executable, with its code bytes when known so that a name
/// is only applied where that code is actually resident.
pub(crate) struct ScopedSymbols {
    pub name: String,
    pub table: psx_symbols::SymbolTable,
    /// `(text_addr, payload)` from a `.symbols.json` report; `None` for bare `.sym` files.
    pub code: Option<(u32, Vec<u8>)>,
    pub bound: Option<bound_symbols::Loaded>,
}

impl ScopedSymbols {
    /// Resolve names against the exact code image used by the caller.
    pub fn describe_image(&self, pc: u32, image_base: u32, image: &[u8], distance: u32) -> Option<String> {
        if let Some(bound) = &self.bound {
            if distance == 0 {
                self.table.describe(bound_symbols::canonical(pc)?, 0)?;
            }
            return bound.describe(pc, image_base, image);
        }
        let canonical = bound_symbols::canonical(pc).unwrap_or(pc);
        let name = self.table.describe(canonical, distance).or_else(|| self.table.describe(pc, distance))?;
        if let Some((base, code)) = &self.code {
            let pc = bound_symbols::canonical(pc)?;
            let base = bound_symbols::canonical(*base)?;
            let off = pc.checked_sub(base)? as usize & !3;
            if off >= code.len() {
                return None;
            }
            let lo = off.saturating_sub(16);
            let hi = off.saturating_add(16).min(code.len());
            let r = base.checked_add(lo as u32)?.checked_sub(image_base)? as usize;
            if lo >= hi || image.get(r..r + (hi - lo)) != code.get(lo..hi) {
                return None;
            }
        }
        Some(name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolCheck {
    /// The executable's bytes around the PC match the RAM given.
    RamMatch,
    /// All bytes in the analysed function body match the supplied image.
    BodyMatch,
    /// Bare `.sym` file: no code to compare against.
    Unverified,
}

impl SymbolCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolCheck::RamMatch => "ram_match",
            SymbolCheck::BodyMatch => "body_match",
            SymbolCheck::Unverified => "unverified",
        }
    }
}

/// Load `.sym` or `.symbols.json` files. Names describe one executable's code;
/// overlays and loaders can reuse the same addresses, so prefer `.symbols.json`.
pub(crate) fn load_symbols(paths: &[PathBuf]) -> psx_media::Result<Vec<ScopedSymbols>> {
    let mut out = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(p).map_err(|e| MediaError::io(p, e))?;
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.ends_with(".symbols.json") {
            let v: Value = serde_json::from_str(&text).map_err(|e| MediaError::Malformed(format!("{name}: {e}")))?;
            if v["format"] == 2 {
                let doc = serde_json::from_value(v).map_err(|e| MediaError::Malformed(format!("{name}: {e}")))?;
                let bound = bound_symbols::Loaded::load(doc, p.parent().unwrap_or(Path::new("."))).map_err(MediaError::Malformed)?;
                out.push(ScopedSymbols { name, table: bound.table(), code: None, bound: Some(bound) });
                continue;
            }
            let source = v["source"].as_str().unwrap_or_default().to_string();
            let report: psx_symbols::MatchReportIn =
                serde_json::from_value(v["report"].clone()).map_err(|e| MediaError::Malformed(format!("{name}: {e}")))?;
            let bytes = psx_media::source::read(&source)?;
            if v["exe_sha256"].as_str().is_some_and(|hash| hash != psx_media::disc::sha256_bytes(&bytes)) {
                return Err(MediaError::Malformed(format!("{name}: symbol source hash mismatch")));
            }
            let exe = PsxExe::parse(&bytes)?;
            out.push(ScopedSymbols {
                name: source.rsplit(['/', '\\', ':']).next().unwrap_or(&source).trim_end_matches(";1").to_string(),
                table: psx_symbols::SymbolTable::from_symbols(&report.symbols),
                code: Some((exe.text_addr, exe.payload(&bytes).to_vec())),
                bound: None,
            });
        } else {
            out.push(ScopedSymbols {
                name: name.trim_end_matches(".sym").to_string(),
                table: psx_symbols::SymbolTable::from_sym_text(&text),
                code: None,
                bound: None,
            });
        }
    }
    Ok(out)
}

/// Name `pc` from the first executable whose code is resident there in `ram`
/// (2 MiB main RAM image). Without RAM, or for bare `.sym` files, the name is unverified.
pub(crate) fn name_pc(scopes: &[ScopedSymbols], pc: u32, ram: Option<&[u8]>) -> Option<(String, SymbolCheck, String)> {
    for s in scopes {
        if let Some(ram) = ram {
            if let Some(n) = s.describe_image(pc, 0x8000_0000, ram, 0x4000) {
                let check = if s.bound.is_some() {
                    SymbolCheck::BodyMatch
                } else if s.code.is_some() {
                    SymbolCheck::RamMatch
                } else {
                    SymbolCheck::Unverified
                };
                return Some((n, check, s.name.clone()));
            }
        } else if s.bound.is_none() {
            let canonical = bound_symbols::canonical(pc).unwrap_or(pc);
            if let Some(n) = s.table.describe(canonical, 0x4000).or_else(|| s.table.describe(pc, 0x4000)) {
                return Some((n, SymbolCheck::Unverified, s.name.clone()));
            }
        }
    }
    None
}

/// Names computed after a run must not claim historical snapshot residency.
pub(crate) fn final_name(scopes: &[ScopedSymbols], pc: u32, ram: Option<&[u8]>) -> Value {
    match name_pc(scopes, pc, ram) {
        Some((name, SymbolCheck::BodyMatch, source)) => json!({"candidate_name":name,"symbols":source,"check":"final_ram_only",
            "diagnostic":"Function body matches final RAM; historical event residency is unknown."}),
        Some((name, check, source)) => json!({"name":name,"exe":source,"check":check.as_str()}),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_tables_check_code_context_and_preserve_plain_symbol_aliases() {
        let scoped = ScopedSymbols {
            name: "original.exe".into(),
            table: psx_symbols::SymbolTable::from_sym_text("80010000 library_function\n"),
            code: Some((0x80010000, vec![7; 128])),
            bound: None,
        };
        let mut image = vec![7; 128];
        assert_eq!(scoped.describe_image(0xa0010004, 0x80010000, &image, 0x4000).as_deref(), Some("library_function+0x4"));
        image[4] = 8;
        assert!(scoped.describe_image(0x80010004, 0x80010000, &image, 0x4000).is_none());
        assert!(scoped.describe_image(0x1f800004, 0x80010000, &image, 0x4000).is_none());
        assert!(scoped.describe_image(0x80010080, 0x80010000, &image, 0x4000).is_none());
        let plain = ScopedSymbols {
            name: "plain".into(),
            table: psx_symbols::SymbolTable::from_sym_text("A0010000 user_alias\n"),
            code: None,
            bound: None,
        };
        assert_eq!(name_pc(&[plain], 0xa0010004, None).unwrap().0, "user_alias+0x4");
    }
}

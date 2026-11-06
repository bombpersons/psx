//! Symbol loading and residency checks shared by runtime and static tools.
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use psx_media::PsxExe;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) fn d_sigs() -> PathBuf {
    sigs_default().0
}

/// The PsyQ signature directory and why it was chosen: `PSX_SIGS_DIR`, else this
/// project's `external/psx_psyq_signatures` when present, else the shared copy in
/// the user data directory when present, else the project path (reported missing).
pub(crate) fn sigs_default() -> (PathBuf, &'static str) {
    if let Some(p) = std::env::var_os("PSX_SIGS_DIR").filter(|p| !p.is_empty()) {
        return (p.into(), "PSX_SIGS_DIR");
    }
    let project = PathBuf::from("external/psx_psyq_signatures");
    match crate::bios::data_dir().map(|d| d.join("psyq_signatures")) {
        Some(shared) if !project.is_dir() && shared.is_dir() => (shared, "user_data"),
        _ => (project, "project_default"),
    }
}
fn d_min_concrete() -> usize {
    24
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct SymbolsArgs {
    /// PS-X EXE file, or `image.cue::/PATH`.
    pub source: String,
    /// Local signature directory (`<version>/<LIB>.json` layout). Defaults to
    /// PSX_SIGS_DIR when set, otherwise external/psx_psyq_signatures in this project, otherwise
    /// the copy the installer put in the user data directory (psx/psyq_signatures).
    #[arg(long, default_value_os_t = d_sigs())]
    #[serde(default = "d_sigs")]
    pub sigs: PathBuf,
    /// Minimum non-wildcard bytes for a signature to be used.
    #[arg(long, default_value_t = d_min_concrete())]
    #[serde(default = "d_min_concrete")]
    pub min_concrete: usize,
    /// With a disc: a directory of decompressed disc data with unpacked.json
    /// (as code inventory takes), searched for debug files too. Default: the
    /// files the project's inventory was taken with.
    #[arg(long)]
    #[serde(default)]
    pub unpacked: Option<PathBuf>,
    /// Output path prefix (default .psx/symbols/<exe name>).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

impl Op for SymbolsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::result(symbols_cmd(&self.source, &self.sigs, self.min_concrete, self.unpacked.as_deref(), self.out))
    }
}

/// Symbols for one executable, with its code bytes when known so that a name
/// is only applied where that code is actually resident.
pub(crate) struct ScopedSymbols {
    pub name: String,
    pub table: psx_symbols::SymbolTable,
    /// `(text_addr, payload)` from a `.symbols.json` report; `None` for bare `.sym` files.
    pub code: Option<(u32, Vec<u8>)>,
    /// The project's names files, checked by the inventory's hashes.
    pub names: Option<std::sync::Arc<crate::names_bind::Scope>>,
}

impl ScopedSymbols {
    /// Resolve names against the exact code image used by the caller.
    pub fn describe_image(&self, pc: u32, image_base: u32, image: &[u8], distance: u32) -> Option<String> {
        if let Some(names) = &self.names {
            return names.describe(pc, image_base, image, distance).map(|(n, _)| n);
        }
        let canonical = crate::names_bind::canonical(pc).unwrap_or(pc);
        let name = self.table.describe(canonical, distance).or_else(|| self.table.describe(pc, distance))?;
        if let Some((base, code)) = &self.code {
            let pc = crate::names_bind::canonical(pc)?;
            let base = crate::names_bind::canonical(*base)?;
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
pub(crate) fn load_symbols(paths: &[PathBuf]) -> crate::Result<Vec<ScopedSymbols>> {
    // The project's own names come first.
    let mut out: Vec<ScopedSymbols> = crate::names_bind::Scope::project()
        .map(|n| ScopedSymbols { name: "names".into(), table: n.table.clone(), code: None, names: Some(n) })
        .into_iter()
        .collect();
    for p in paths {
        let text = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.ends_with(".symbols.json") {
            let v: Value = serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{name}: {e}")))?;
            let source = v["source"].as_str().unwrap_or_default().to_string();
            let report: psx_symbols::MatchReportIn =
                serde_json::from_value(v["report"].clone()).map_err(|e| Error::malformed(format!("{name}: {e}")))?;
            let bytes = psx_media::source::read(&source)?;
            if v["exe_sha256"].as_str().is_some_and(|hash| hash != psx_media::disc::sha256_bytes(&bytes)) {
                return Err(Error::malformed(format!("{name}: symbol source hash mismatch")));
            }
            let exe = PsxExe::parse(&bytes)?;
            out.push(ScopedSymbols {
                name: source.rsplit(['/', '\\', ':']).next().unwrap_or(&source).trim_end_matches(";1").to_string(),
                table: psx_symbols::SymbolTable::from_symbols(&report.symbols),
                code: Some((exe.text_addr, exe.payload(&bytes).to_vec())),
                names: None,
            });
        } else {
            out.push(ScopedSymbols {
                name: name.trim_end_matches(".sym").to_string(),
                table: psx_symbols::SymbolTable::from_sym_text(&text),
                code: None,
                names: None,
            });
        }
    }
    Ok(out)
}

/// Name `pc` from the first executable whose code is resident there in `ram`
/// (2 MiB main RAM image). Without RAM, or for bare `.sym` files, the name is unverified.
pub(crate) fn name_pc(scopes: &[ScopedSymbols], pc: u32, ram: Option<&[u8]>) -> Option<(String, SymbolCheck, String)> {
    for s in scopes {
        if let (Some(names), Some(ram)) = (&s.names, ram) {
            if let Some((n, checked)) = names.describe(pc, 0x8000_0000, ram, 0x4000) {
                let check = if checked { SymbolCheck::BodyMatch } else { SymbolCheck::Unverified };
                return Some((n, check, s.name.clone()));
            }
        } else if let Some(ram) = ram {
            if let Some(n) = s.describe_image(pc, 0x8000_0000, ram, 0x4000) {
                let check = if s.code.is_some() { SymbolCheck::RamMatch } else { SymbolCheck::Unverified };
                return Some((n, check, s.name.clone()));
            }
        } else {
            let canonical = crate::names_bind::canonical(pc).unwrap_or(pc);
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
        Some((name, SymbolCheck::BodyMatch, source)) => {
            json!({"candidate_name":name,"symbols":source,"check":"final_ram_only",
            "diagnostic":"Function body matches final RAM; historical event residency is unknown."})
        }
        Some((name, check, source)) => json!({"name":name,"exe":source,"check":check.as_str()}),
        None => Value::Null,
    }
}

/// `psx code symbols`: name PsyQ library functions in an EXE by signature matching.
pub(crate) fn symbols_cmd(
    source: &str,
    sigs_dir: &Path,
    min_concrete: usize,
    unpacked: Option<&Path>,
    out: Option<PathBuf>,
) -> crate::Result<Value> {
    // A disc means its boot executable, and the disc is scanned for debug files.
    let (media, exe_path) = match source.split_once("::") {
        Some((m, p)) => (Some(m.to_string()), Some(p.to_string())),
        None if psx_media::disc::is_image_path(Path::new(source)) => {
            use psx_media::iso9660::FileSystem;
            let disc = psx_media::disc::Disc::open(source)?;
            let fs = FileSystem::read(&disc)?;
            let cnf = fs.find("/SYSTEM.CNF").and_then(|e| fs.read_file(&disc, e).ok());
            let boot = cnf
                .as_deref()
                .map(psx_media::system_cnf::SystemCnf::parse)
                .and_then(|c| c.boot)
                .unwrap_or_else(|| psx_media::system_cnf::DEFAULT_BOOT.into());
            (Some(source.to_string()), Some(crate::inventory::boot_path(&boot)))
        }
        None => (None, None),
    };
    let exe_source = match (&media, &exe_path) {
        (Some(m), Some(p)) => format!("{m}::{p}"),
        _ => source.to_string(),
    };
    let source = exe_source.as_str();
    let bytes = psx_media::source::read(source)?;
    let exe = PsxExe::parse(&bytes)?;
    let sigs = psx_symbols::load_dir(sigs_dir).map_err(|e| {
        Error::new("not_found", format!("signatures: {e} (run the installer with signatures, supply a local psx_psyq_signatures checkout with --sigs / MCP sigs, or set PSX_SIGS_DIR to its absolute path for all projects; context reports the default path)"))
    })?;
    let report = psx_symbols::match_code(exe.payload(&bytes), exe.text_addr, &sigs, min_concrete);
    let stem = source.rsplit(['/', '\\', ':']).next().unwrap_or("exe").trim_end_matches(";1").to_string();
    let prefix = out.unwrap_or_else(|| PathBuf::from(".psx/symbols").join(&stem));
    if let Some(parent) = prefix.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let with_ext = |ext: &str| PathBuf::from(format!("{}.{ext}", prefix.display()));
    let write = |p: PathBuf, text: String| std::fs::write(&p, text).map_err(|e| Error::io(&p, e)).map(|_| p);
    let doc = json!({
        "source": source,
        "text_addr": psx_media::memmap::hex(exe.text_addr),
        "exe_sha256": psx_media::disc::sha256_bytes(&bytes),
        "report": report,
    });
    let json_path = write(with_ext("symbols.json"), serde_json::to_string_pretty(&doc).unwrap())?;
    let ghidra = write(with_ext("ghidra.txt"), psx_symbols::to_ghidra(&report.symbols))?;
    let sym = write(with_ext("sym"), psx_symbols::to_sym(&report.symbols))?;
    // Names into .psx/names: the disc's debug files first, then the PsyQ
    // signatures' where no debug file named the address.
    let names_dir = PathBuf::from(psx_symbols::names::IMPORTED_DIR);
    let mut named = std::collections::BTreeSet::new();
    let disc = match &media {
        Some(m) => {
            let unpacked = crate::disc_names::unpacked(Path::new(m), unpacked)?;
            let scan = crate::disc_names::scan(Path::new("."), Path::new(m), &unpacked, &names_dir, &mut named)?;
            crate::disc_names::record(Path::new("."), &scan, true)?;
            Some(scan)
        }
        None => None,
    };
    let psyq_names =
        crate::disc_names::psyq_names(&report, source, &names_dir.join(format!("psyq_{}.names", psx_symbols::debug::ident(&stem))), &named)?;
    let functions = report.symbols.iter().filter(|s| s.kind == psx_symbols::Kind::Function).count();
    let mut libs: std::collections::BTreeMap<String, usize> = Default::default();
    for o in &report.objects {
        let lib = o.source.split('/').nth(1).unwrap_or("?").to_string();
        *libs.entry(lib).or_default() += 1;
    }
    let matched: u64 = report.objects.iter().map(|o| o.size as u64).sum();
    let build = psx_symbols::build::detect(exe.payload(&bytes), exe.text_addr, exe.pc);
    // Only inside matched library objects is a word known to be code; there,
    // name the function holding it.
    let mut hand_written = std::collections::BTreeSet::new();
    for &a in &build.hand_written {
        let Some(o) = report.objects.iter().find(|o| o.address <= a && a < o.address + o.size) else { continue };
        let function = report
            .symbols
            .iter()
            .filter(|s| s.kind == psx_symbols::Kind::Function && o.address <= s.address && s.address <= a)
            .max_by_key(|s| s.address);
        let lib = o.source.split('/').skip(1).collect::<Vec<_>>().join("/");
        hand_written.insert(match function {
            Some(f) => format!("{} ({lib})", f.name),
            None => lib,
        });
    }
    Ok(json!({
        "exe": source,
        "text": {"addr": format!("0x{:08X}", exe.text_addr), "bytes": exe.text_size},
        "signatures_loaded": sigs.len(),
        "versions": report.versions.iter().take(5).collect::<Vec<_>>(),
        "chosen_version": report.chosen_version,
        "objects_matched": report.objects.len(),
        "objects_by_library": libs,
        "functions_named": functions,
        "code_covered_bytes": matched,
        "code_covered_pct": (matched as f64 * 1000.0 / exe.text_size.max(1) as f64).round() / 10.0,
        "ambiguous": report.ambiguous.len(),
        "build": {
            "gp": build.gp.map(psx_media::memmap::hex),
            "gp_accesses": build.gp_accesses,
            "divisions": build.divisions,
            "zero_checks": build.zero_checks,
            "overflow_checks": build.overflow_checks,
            "frames": build.frames,
            "frame_pointers": build.frame_pointers,
            "load_nops_pct": build.load_nops_pct,
            "hand_written": hand_written,
        },
        "interpretation": {
            "build":"Read from the code: gp is set by the startup code when small data is addressed from it (GCC's -G); zero_checks and overflow_checks are GCC's division checks (break 7 and 6), which psx's decompile folds; load_nops_pct near 50 means the code was not scheduled; hand_written names the library functions that write gp, k0 or k1 outside the startup code, which compiled C never does (game code is not checked: its words cannot be told from data here). Counts cover every word, data included.",
            "scope":"PsyQ library signatures, not a catalogue of game-specific functions; low coverage alone is not a failure.",
            "chosen_version":"Best-scoring installed signature set, not proof of a game's exact SDK version.",
            "ambiguous":"Signatures with multiple matches are skipped, never arbitrarily selected.",
            "disc":"With a disc, every file (archives included, as bytes), each decompressed file the inventory was taken with (or unpacked gives), and every sector no file owns are searched for debug files (SN SYM, psylink MAP, ELF, CPE) by name and by content, and for source paths and GCC ident strings. A debug file most of whose functions are at the executable's function starts is imported to .psx/names/, in the names files' format; an ELF's DWARF is not read. Compressed data is searched only as decompressed files given.",
            "next_step":"Use runtime observations and Ghidra for game-specific functions; verify pseudocode against instructions."
        },
        "files": {"report": json_path, "ghidra": ghidra, "sym": sym, "names": psyq_names},
        "disc": disc,
    }))
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
            names: None,
        };
        let mut image = vec![7; 128];
        assert_eq!(
            scoped.describe_image(0xa0010004, 0x80010000, &image, 0x4000).as_deref(),
            Some("library_function+0x4")
        );
        image[4] = 8;
        assert!(scoped.describe_image(0x80010004, 0x80010000, &image, 0x4000).is_none());
        assert!(scoped.describe_image(0x1f800004, 0x80010000, &image, 0x4000).is_none());
        assert!(scoped.describe_image(0x80010080, 0x80010000, &image, 0x4000).is_none());
        let plain = ScopedSymbols {
            name: "plain".into(),
            table: psx_symbols::SymbolTable::from_sym_text("A0010000 user_alias\n"),
            code: None,
            names: None,
        };
        assert_eq!(name_pc(&[plain], 0xa0010004, None).unwrap().0, "user_alias+0x4");
    }
}

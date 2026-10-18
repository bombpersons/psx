//! Deterministic first-use discovery and project defaults; no process-wide cwd changes.
use crate::{
    bios,
    commands::{Ctx, Op, Outcome},
};
use clap::Args;
use psx_media::{disc::sha256_file, MediaError, PsxExe};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    format: u32,
    #[serde(default)]
    media: Option<PathBuf>,
    #[serde(default)]
    bios: Option<PathBuf>,
    /// Older configs saved a backend; native is now the only one.
    #[serde(default, rename = "backend", skip_serializing)]
    _backend: Option<serde::de::IgnoredAny>,
}
fn config(root: &Path) -> Result<Config, Value> {
    let p = root.join(".psx/project.json");
    match fs::read(&p) {
        Ok(b) => {
            if b.len() > 64 * 1024 {
                return Err(error("invalid_config", "Project config exceeds 64 KiB"));
            }
            let c: Config = serde_json::from_slice(&b).map_err(|e| error("invalid_config", format!("{}: {e}", p.display())))?;
            if c.format != 1 {
                return Err(error("invalid_config", "Project config requires format: 1"));
            }
            Ok(c)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(error("io_error", format!("{}: {e}", p.display()))),
    }
}
pub fn error(code: &str, message: impl ToString) -> Value {
    json!({"error":{"code":code,"message":message.to_string()}})
}
pub fn failed(value: Value) -> Outcome {
    Outcome { value, exit: 1, pretty: true, text: false }
}
fn media_error(e: MediaError) -> Value {
    error(e.code(), e)
}
fn absolute(root: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_owned()
    } else {
        root.join(p)
    }
}

pub fn build() -> Value {
    json!({"version":env!("CARGO_PKG_VERSION"),"id":env!("PSX_BUILD_ID"),
    "source_sha256":env!("PSX_SOURCE_SHA256"),"profile":env!("PSX_BUILD_PROFILE"),"target":env!("PSX_BUILD_TARGET")})
}
pub fn version() -> String {
    format!("{}+{}", env!("CARGO_PKG_VERSION"), env!("PSX_BUILD_ID"))
}
pub fn capabilities() -> Value {
    json!({
        "emulator":{"available":true,"run":true,"checkpoints":true,"write_watches":true,"read_watches":true,"instruction_trace":true,"gpu_capture":true,"samples":true,"pokes":true,"screenshots":true},
    "ghidra":{"optional":true,"runtime_snapshots":true,"rename_comments":true,"verified_symbol_export":true,"same_analysis_queue":true},
    "agent_interface":{"artifact_read":true,"inline_input":true,"explicit_release_boundary":true,"query_pagination":true,"coverage_metadata":true,
        "per_call_project_root":true,"extraction_manifests":true,"native_input_application_log":true,"gpu_draw_filters":true},
        "setup":{"bios_list":true,"bios_install_local":true,"bios_download_explicit":true,"automatic_downloads":false}
    })
}
fn native_header(path: &Path) -> Result<Option<psx_core::state::Header>, Value> {
    let mut f = fs::File::open(path).map_err(|e| error("io_error", format!("{}: {e}", path.display())))?;
    let mut prefix = [0; 12];
    match f.read_exact(&mut prefix) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(error("io_error", e)),
    }
    if &prefix[..8] != psx_core::state::MAGIC {
        return Ok(None);
    }
    let len = u32::from_le_bytes(prefix[8..].try_into().unwrap()) as usize;
    if len > 64 * 1024 {
        return Err(error("invalid_state", "State header exceeds 64 KiB"));
    }
    let mut b = prefix.to_vec();
    b.resize(12 + len, 0);
    f.read_exact(&mut b[12..]).map_err(|e| error("invalid_state", format!("Truncated state header: {e}")))?;
    psx_core::state::read_header(&b).map(|h| Some(h.0)).map_err(|e| error("invalid_state", e))
}
/// A report can supply paths only when it cryptographically identifies this state.
fn state_report(path: &Path) -> Option<Value> {
    let p = path.parent()?.join("report.json");
    if fs::metadata(&p).ok()?.len() > 64 * 1024 * 1024 {
        return None;
    }
    let v: Value = serde_json::from_slice(&fs::read(p).ok()?).ok()?;
    let hash = sha256_file(path).ok()?;
    let bound = v["files"].as_object().is_some_and(|files| files.values().any(|f| f["sha256"] == hash))
        || v["states"].as_array().is_some_and(|states| states.iter().any(|s| s["sha256"] == hash));
    bound.then_some(v)
}
fn report_path(root: &Path, report: &Value, key: &str) -> Option<PathBuf> {
    let p = absolute(root, Path::new(report["identity"][key].as_str()?));
    p.is_file().then_some(p)
}
fn region(media: Option<&Path>) -> Result<Option<bios::Region>, Value> {
    let Some(p) = media else { return Ok(None) };
    let exe = psx_media::exe::is_exe_path(p);
    let hint = if exe {
        let b = psx_media::source::read(&p.to_string_lossy()).map_err(media_error)?;
        PsxExe::parse(&b).map_err(media_error)?.region_hint().map(str::to_owned)
    } else {
        let disc = crate::media_cmd::disc_summary(p, false).map_err(media_error)?;
        disc["boot_exe"]["header"]["region_hint"].as_str().map(str::to_owned)
    };
    Ok(hint.and_then(|h| bios::Region::from_hint(&h)))
}

pub struct Resolved {
    pub media: Option<PathBuf>,
    pub bios: Option<PathBuf>,
    pub region: Option<bios::Region>,
    pub bios_reason: &'static str,
    pub candidates: Vec<bios::Candidate>,
    pub state: Option<psx_core::state::Header>,
    pub media_required: bool,
    pub warnings: Vec<String>,
}
impl Resolved {
    pub fn setup_error(&self) -> Option<Value> {
        if self.bios.is_none() {
            return Some(self.bios_error());
        }
        self.media_required.then(|| {
            error(
                "media_required",
                "This checkpoint requires media; pass its disc/EXE or configure matching project media with context --image PATH --save.",
            )
        })
    }
    pub fn bios_error(&self) -> Value {
        let mut v = error("bios_required", "No BIOS selected. Use bios_list, install one, or pass bios. See candidates and next_action.");
        v["error"]["candidates"] = json!(self.candidates);
        v["error"]["selection"] = json!(self.bios_reason);
        v["error"]["required_sha256"] = json!(self.state.as_ref().map(|s| &s.bios_sha256));
        v["error"]["next_action"] = if self.state.is_some() {
            json!({"tool":"bios_list","arguments":{}})
        } else if let Some(r) = self.region {
            json!({"tool":"bios_install","arguments":{"region":r,"download":true},"explicit_setup_action":true})
        } else {
            json!({"tool":"bios_list","arguments":{}})
        };
        v
    }
}
/// Explicit BIOS wins (including intentional identity-mismatch diagnostics).
/// Otherwise state identity wins, then project preference, then documented regional catalog order.
pub fn resolve(
    root: &Path,
    media: Option<PathBuf>,
    bios_path: Option<PathBuf>,
    from_state: Option<&Path>,
    discover: bool,
) -> Result<Resolved, Value> {
    let cfg = config(root)?;
    let state = from_state.map(native_header).transpose()?.flatten();
    let report = from_state.and_then(state_report);
    let media = media.or_else(|| {
        if state.as_ref().is_some_and(|s| s.media.is_none()) {
            None
        } else {
            report.as_ref().and_then(|r| report_path(root, r, "media")).or(cfg.media.map(|p| absolute(root, &p)))
        }
    });
    // An explicit BIOS requires no inventory or media inspection before validation.
    let region = if discover || bios_path.is_none() { region(media.as_deref())? } else { None };
    let mut candidates = if discover || bios_path.is_none() { bios::inventory(root).map_err(media_error)? } else { vec![] };
    if bios_path.is_none() {
        if let Some(p) = &cfg.bios {
            let p = absolute(root, p);
            if p.is_file() && !candidates.iter().any(|c| c.path == p) {
                candidates.push(bios::inspect(&p).map_err(media_error)?);
            }
        }
    }
    let media_required = state.as_ref().is_some_and(|s| s.media.is_some()) && media.is_none();
    let mut warnings = Vec::new();
    let (selected, reason) = if let Some(p) = bios_path {
        (Some(p), "explicit")
    } else if let Some(s) = &state {
        if let Some(p) = report.as_ref().and_then(|r| report_path(root, r, "bios")) {
            if let Ok(c) = bios::inspect(&p) {
                if c.native_sha256 == s.bios_sha256 && !candidates.iter().any(|old| old.path == p) {
                    candidates.push(c);
                }
            }
        }
        let matching = candidates.iter().find(|c| c.native_sha256 == s.bios_sha256);
        if matching.is_none() {
            warnings.push("A saved state requires its exact BIOS hash; regional substitutes are not selected.".into());
        }
        (matching.map(|c| c.path.clone()), "state_sha256")
    } else if let Some(p) = cfg.bios {
        (Some(absolute(root, &p)), "project_config")
    } else if let Some(r) = region {
        let preferred = bios::CATALOG.iter().filter(|c| c.region == r).find_map(|known| {
            candidates
                .iter()
                .filter(|c| c.sha256 == known.sha256)
                .min_by_key(|c| (!c.path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(known.file)), c.path.clone()))
        });
        if let Some(c) = preferred {
            (Some(c.path.clone()), "regional_catalog_preference")
        } else {
            let mut compatible: Vec<_> = candidates.iter().filter(|c| c.bytes == 524288 && c.region == Some(r)).collect();
            compatible.sort_by(|a, b| a.sha256.cmp(&b.sha256));
            compatible.dedup_by_key(|c| &c.sha256);
            if compatible.len() == 1 {
                (Some(compatible[0].path.clone()), "unique_regional_candidate")
            } else {
                (None, if compatible.is_empty() { "no_compatible_bios" } else { "ambiguous_regional_candidates" })
            }
        }
    } else {
        (None, "no_region_or_preference")
    };
    Ok(Resolved { media, bios: selected, region, bios_reason: reason, candidates, state, media_required, warnings })
}

#[derive(Args, Deserialize, JsonSchema)]
pub struct ArgsContext {
    /// Disc/EXE to inspect; defaults to saved project media. Discovery never downloads.
    #[arg(long)]
    pub image: Option<PathBuf>,
    /// Explicit BIOS preference; context --save stores it for later calls.
    #[arg(long)]
    pub bios: Option<PathBuf>,
    /// Inspect a checkpoint's exact BIOS requirements.
    #[arg(long)]
    pub from_state: Option<PathBuf>,
    /// Save the resolved media/BIOS as this project's defaults (.psx/project.json).
    #[arg(long)]
    #[serde(default)]
    pub save: bool,
}
impl Op for ArgsContext {
    fn exec(self, ctx: &Ctx) -> Outcome {
        let r = match resolve(&ctx.root, self.image, self.bios, self.from_state.as_deref(), true) {
            Ok(r) => r,
            Err(e) => return failed(e),
        };
        let selected_info = match r.bios.as_ref().map(|p| bios::inspect(&absolute(&ctx.root, p))).transpose() {
            Ok(v) => v,
            Err(e) => return Outcome::media(Err(e)),
        };
        let profile = ctx.root.join(".psx/project.json");
        let (signatures, sigs_selection) = crate::commands::sigs_default();
        let signatures = absolute(&ctx.root, &signatures);
        if self.save {
            if let Some(e) = r.setup_error() {
                return failed(e);
            }
            let bios = r.bios.as_ref().unwrap();
            if let Err(e) = bios::inspect(&absolute(&ctx.root, bios)) {
                return Outcome::media(Err(e));
            }
            let cfg = Config {
                format: 1,
                media: r.media.as_ref().map(|p| absolute(&ctx.root, p)),
                bios: Some(absolute(&ctx.root, bios)),
                _backend: None,
            };
            let temp = profile.with_extension(format!("tmp-{}-{}", std::process::id(), crate::instance_suffix()));
            let write = fs::create_dir_all(profile.parent().unwrap())
                .and_then(|_| fs::write(&temp, serde_json::to_vec_pretty(&cfg).unwrap()))
                .and_then(|_| fs::rename(&temp, &profile));
            if let Err(e) = write {
                let _ = fs::remove_file(&temp);
                return failed(error("io_error", e));
            }
        }
        let setup = r.setup_error().map(|v| v["error"].clone());
        let mut template = json!({"tool":"template_create","arguments":{"dest":"game"}});
        if let Some(region) = r.region {
            template["arguments"]["region"] = json!(match region {
                bios::Region::Pal => "pal",
                bios::Region::NtscU | bios::Region::NtscJ => "ntsc",
            });
        }
        let mut next_calls = vec![template];
        if setup.is_none() {
            next_calls.push(json!({"tool":"run","arguments":{"media":r.media,"bios":r.bios,"frames":60,"save_state":true,"brief":true,"return_screen":true}}));
        }
        Outcome::media(Ok(json!({"status":if setup.is_none(){"ready"}else{"needs_setup"},"build":build(),"project_root":ctx.root,
            "config":profile,"saved":self.save,"output_root":ctx.root.join(".psx"),"bios_dir":bios::store(),
            "signatures":{"path":signatures,"available":signatures.is_dir(),
                "selection":sigs_selection,
                "layout":"<version>/<LIB>.json","override":"symbols.sigs / --sigs; shared default: PSX_SIGS_DIR (use an absolute path)"},
            "resolved":{"media":r.media,"bios":r.bios,"bios_selection":r.bios_reason,"disc_region_hint":r.region.map(|r|r.hint())},
            "selected_bios":selected_info,"bios_candidates":r.candidates,"bios_hashes":bios::hash_notes(),"state":r.state,"setup":setup,"capabilities":capabilities(),"warnings":r.warnings,
            "next_calls":next_calls})))
    }
}

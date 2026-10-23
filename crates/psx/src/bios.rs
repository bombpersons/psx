//! Local BIOS inventory and explicit, hash-verified installation.
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use clap::{Args, Subcommand, ValueEnum};
use psx_media::disc::{sha256_bytes, sha256_file};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize, JsonSchema)]
pub enum Region {
    #[serde(rename = "pal")]
    Pal,
    #[serde(rename = "ntsc-u")]
    NtscU,
    #[serde(rename = "ntsc-j")]
    NtscJ,
}
impl Region {
    pub fn from_hint(s: &str) -> Option<Self> {
        match s {
            "PAL" => Some(Self::Pal),
            "NTSC-U" => Some(Self::NtscU),
            "NTSC-J" => Some(Self::NtscJ),
            _ => None,
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            Self::Pal => "PAL",
            Self::NtscU => "NTSC-U",
            Self::NtscJ => "NTSC-J",
        }
    }
}
pub struct CatalogEntry {
    pub file: &'static str,
    pub region: Region,
    pub sha256: &'static str,
}
/// SHA-256 identities of the locally validated 512 KiB images. The Archive
/// manifest's SHA-1/size also agree; installation verifies these pinned SHA-256s.
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        file: "scph5500.bin",
        region: Region::NtscJ,
        sha256: "9c0421858e217805f4abe18698afea8d5aa36ff0727eb8484944e00eb5e7eadb",
    },
    CatalogEntry {
        file: "scph5501.bin",
        region: Region::NtscU,
        sha256: "11052b6499e466bbf0a709b1f9cb6834a9418e66680387912451e971cf8a1fef",
    },
    CatalogEntry {
        file: "scph5502.bin",
        region: Region::Pal,
        sha256: "1faaa18fa820a0225e488d9f086296b8e6c46df739666093987ff7d8fd352c09",
    },
    CatalogEntry {
        file: "scph7502.bin",
        region: Region::Pal,
        sha256: "5e84a94818cf5282f4217591fefd88be36b9b174b3cc7cb0bcd75199beb450f1",
    },
];
pub const ARCHIVE: &str = "https://archive.org/download/the-ultimate-playstation-1-bios";

pub fn store() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PSX_BIOS_DIR").filter(|v| !v.is_empty()) {
        return Some(p.into());
    }
    data_dir().map(|d| d.join("bios"))
}

/// Per-user data shared by every project: `%LOCALAPPDATA%\psx` on Windows,
/// else `$XDG_DATA_HOME/psx` or `~/.local/share/psx` (where the installers put
/// the PsyQ signatures).
pub fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(p).join("psx"));
    }
    if let Some(p) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p).join("psx"));
    }
    std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share/psx"))
}
#[derive(Clone, Serialize)]
pub struct Candidate {
    pub path: PathBuf,
    pub sha256: String,
    /// Identity of the 512 KiB ROM bytes the native core actually uses (authored short ROMs are zero-padded).
    pub native_sha256: String,
    pub bytes: u64,
    pub region: Option<Region>,
    pub known_image: Option<String>,
    pub version: Option<String>,
}
pub fn hash_notes() -> Value {
    json!({"sha256":"SHA-256 of the entire file (bytes).",
        "native_sha256":"SHA-256 of the 512 KiB image used by the native core: longer files are truncated, shorter files zero-padded. Different lengths can legitimately produce different hashes."})
}
pub fn inspect(path: &Path) -> crate::Result<Candidate> {
    let size = fs::metadata(path).map_err(|e| Error::io(path, e))?.len();
    if size == 0 || size > 1024 * 1024 {
        return Err(Error::malformed("BIOS candidate must be 1 byte..1 MiB"));
    }
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    let hash = sha256_bytes(&bytes);
    let native_hash = if bytes.len() == 512 * 1024 {
        hash.clone()
    } else {
        let mut image = bytes.clone();
        image.resize(512 * 1024, 0);
        sha256_bytes(&image)
    };
    let known = CATALOG.iter().find(|c| c.sha256 == hash);
    let version = bytes
        .get(0x7ff32..)
        .and_then(|v| v.split(|b| *b == 0).next())
        .and_then(|v| std::str::from_utf8(v).ok())
        .filter(|v| v.starts_with("System ROM Version "))
        .map(str::to_owned);
    let region = known.map(|c| c.region).or_else(|| match version.as_deref()?.split_whitespace().last()? {
        "E" => Some(Region::Pal),
        "A" => Some(Region::NtscU),
        "I" => Some(Region::NtscJ),
        _ => None,
    });
    Ok(Candidate {
        path: path.to_owned(),
        sha256: hash,
        native_sha256: native_hash,
        bytes: size,
        region,
        known_image: known.map(|c| c.file.into()),
        version,
    })
}
pub fn inventory(root: &Path) -> crate::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut dirs = vec![root.join("bios")];
    if let Some(p) = store() {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    for dir in dirs {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(&dir, e)),
        };
        for e in entries {
            let p = e.map_err(|e| Error::io(&dir, e))?.path();
            if !p.is_file()
                || !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("bin") || e.eq_ignore_ascii_case("rom"))
            {
                continue;
            }
            let size = fs::metadata(&p).map_err(|e| Error::io(&p, e))?.len();
            if size == 0 || size > 1024 * 1024 {
                continue;
            }
            out.push(inspect(&p)?);
            if out.len() > 256 {
                return Err(Error::new("limit_exceeded", "more than 256 BIOS candidates; narrow PSX_BIOS_DIR"));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

#[derive(Subcommand)]
pub enum Action {
    List(ListArgs),
    Install(InstallArgs),
}
impl Action {
    pub fn exec(self, ctx: &Ctx) -> Outcome {
        match self {
            Self::List(a) => a.exec(ctx),
            Self::Install(a) => a.exec(ctx),
        }
    }
}
#[derive(Args, Deserialize, JsonSchema)]
pub struct ListArgs {}
impl Op for ListArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::ok(json!({"bios_dir":store(),"project_bios_dir":ctx.root.join("bios"),"hashes":hash_notes(),
        "installed": match inventory(&ctx.root) {Ok(v)=>v,Err(e)=>return Outcome::result(Err(e))},
        "download_catalog":CATALOG.iter().map(|c|json!({"name":c.file,"region":c.region,"sha256":c.sha256,"bytes":524288,"url":format!("{ARCHIVE}/{}",c.file)})).collect::<Vec<_>>() }))
    }
}
#[derive(Args, Deserialize, JsonSchema)]
pub struct InstallArgs {
    /// Install a local BIOS image. Exactly one of from or download is required.
    #[arg(long, conflicts_with = "download")]
    pub from: Option<PathBuf>,
    /// Explicitly download one pinned image from the documented Internet Archive source. Requires region.
    #[arg(long, conflicts_with = "from")]
    #[serde(default)]
    pub download: bool,
    /// Preferred region for a download; local imports infer it from known bytes/version when possible.
    #[arg(long, value_enum)]
    pub region: Option<Region>,
    /// Installation directory; default PSX_BIOS_DIR or the user's psx data directory.
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn download(url: &str, path: &Path, ctx: &Ctx) -> crate::Result<()> {
    let mut child = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "60",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-filesize",
            "524288",
            "--output",
        ])
        .arg(path)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::new("download_failed", format!("BIOS download requires curl: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(65);
    loop {
        if Instant::now() >= deadline || ctx.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new("download_failed", "BIOS download stopped before installation"));
        }
        if let Some(status) = child.try_wait().map_err(|e| Error::io(path, e))? {
            let output = child.wait_with_output().map_err(|e| Error::io(path, e))?;
            if status.success() {
                return Ok(());
            }
            return Err(Error::new(
                "download_failed",
                format!("BIOS download failed: {}; no image installed", String::from_utf8_lossy(&output.stderr).trim()),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
impl Op for InstallArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result((|| -> crate::Result<Value> {
            if self.from.is_some() == self.download {
                return Err(Error::invalid("give exactly one of --from or --download"));
            }
            let dir = self.dir.or_else(store).ok_or_else(|| Error::invalid("set PSX_BIOS_DIR or pass --dir"))?;
            let catalog = if self.download {
                Some(
                    CATALOG
                        .iter()
                        .find(|c| Some(c.region) == self.region)
                        .ok_or_else(|| Error::invalid("--download requires --region pal|ntsc-u|ntsc-j"))?,
                )
            } else {
                None
            };
            fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
            let temp = Temporary(dir.join(format!(".install-{}-{}", std::process::id(), crate::instance_suffix())));
            fs::OpenOptions::new().write(true).create_new(true).open(&temp.0).map_err(|e| Error::io(&temp.0, e))?;
            if let Some(c) = catalog {
                download(&format!("{ARCHIVE}/{}", c.file), &temp.0, ctx)?;
            } else {
                let source = self.from.as_ref().unwrap();
                inspect(source)?;
                fs::copy(source, &temp.0).map_err(|e| Error::io(source, e))?;
            }
            let mut candidate = inspect(&temp.0)?;
            if let Some(c) = catalog {
                if candidate.sha256 != c.sha256 || candidate.bytes != 524288 {
                    return Err(Error::new(
                        "verification_failed",
                        "Downloaded BIOS failed pinned SHA-256/size verification; nothing installed",
                    ));
                }
            }
            if self.region.is_some() && candidate.region.is_some() && self.region != candidate.region {
                return Err(Error::invalid("BIOS region differs from --region"));
            }
            // Unknown imports remain unknown; a region flag must not turn arbitrary bytes into a verified regional BIOS.
            let dest = dir.join(format!("{}.bin", candidate.sha256));
            if dest.exists() {
                if sha256_file(&dest)? != candidate.sha256 {
                    return Err(Error::new(
                        "verification_failed",
                        "Installed BIOS path contains different bytes; it was preserved",
                    ));
                }
            } else {
                // Hard-link publishing is atomic and never replaces another installation.
                match fs::hard_link(&temp.0, &dest) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        if sha256_file(&dest)? != candidate.sha256 {
                            return Err(Error::new(
                                "verification_failed",
                                "Concurrent BIOS installation has different bytes",
                            ));
                        }
                    }
                    Err(e) => return Err(Error::io(&dest, e)),
                }
            }
            candidate.path = dest.canonicalize().map_err(|e| Error::io(&dest, e))?;
            Ok(
                json!({"status":"installed","bios":candidate,"source":self.from.map(|p|json!({"file":p})).unwrap_or_else(||json!({"url":format!("{ARCHIVE}/{}",catalog.unwrap().file)})),"verification":if catalog.is_some(){"pinned_sha256"}else{"local_copy_sha256"}}),
            )
        })())
    }
}

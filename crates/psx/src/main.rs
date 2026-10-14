//! `psx`: PlayStation reverse-engineering toolkit.
//!
//! Every command prints one JSON document on stdout. Failures print
//! `{"error": {"code", "message"}}` and exit non-zero.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use psx_media::disc::{sha256_bytes, sha256_file};
use psx_media::iso9660::{Entry, FileSystem};
use psx_media::system_cnf::{SystemCnf, DEFAULT_BOOT};
use psx_media::{memmap, Disc, MediaError, PsxExe};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "psx", version, about = "PlayStation reverse-engineering toolkit")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Tracks, volume, SYSTEM.CNF and boot executable of a disc image.
    Disc {
        image: PathBuf,
        /// Also stream SHA-256 over every image file (slow for full discs).
        #[arg(long)]
        hash: bool,
    },
    /// List every ISO9660 entry with LBA, size and CD-XA attributes.
    Ls { image: PathBuf },
    /// Extract one file, or every file with --all, into a directory.
    /// Form 2 / interleaved files are written as raw 2352-byte sectors with a `.raw` suffix.
    Extract {
        image: PathBuf,
        /// ISO path, e.g. `/DATA/LEVEL1.BIN` or `cdrom:\SLUS_000.79;1`.
        path: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(short, long, default_value = "extracted")]
        out: PathBuf,
    },
    /// Parse a PS-X EXE from a file, or from a disc with `image.cue::/PATH`.
    Exe { source: String },
    /// Describe sectors: form, XA subheader, owning file and a hex preview.
    Sector {
        image: PathBuf,
        lba: u32,
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// Hex bytes shown per sector.
        #[arg(long, default_value_t = 64)]
        preview: usize,
    },
    /// Classify a CPU address (segment, region, folded offset).
    Addr {
        /// Hex address, e.g. 0x80010000.
        address: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("{}", json!({"error": {"code": e.code(), "message": e.to_string()}}));
            ExitCode::from(if matches!(e, MediaError::Unsupported(_)) { 2 } else { 1 })
        }
    }
}

type R = psx_media::Result<Value>;

fn run(cmd: Command) -> R {
    match cmd {
        Command::Disc { image, hash } => disc_summary(&image, hash),
        Command::Ls { image } => {
            let disc = Disc::open(&image)?;
            let fs = FileSystem::read(&disc)?;
            Ok(json!({"volume": fs.volume, "entries": fs.entries.iter().map(entry_json).collect::<Vec<_>>(), "warnings": fs.warnings}))
        }
        Command::Extract { image, path, all, out } => extract(&image, path.as_deref(), all, &out),
        Command::Exe { source } => exe_cmd(&source),
        Command::Sector { image, lba, count, preview } => sectors(&image, lba, count, preview),
        Command::Addr { address } => {
            let a = u32::from_str_radix(address.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .map_err(|_| MediaError::Malformed(format!("bad hex address {address}")))?;
            Ok(serde_json::to_value(memmap::resolve(a)).unwrap())
        }
    }
}

fn entry_json(e: &Entry) -> Value {
    json!({
        "path": e.path,
        "lba": e.lba,
        "size": e.size,
        "sectors": e.sectors(),
        "dir": e.is_dir,
        "streamed": e.is_streamed(),
        "xa_attributes": e.xa_attributes.map(|a| format!("0x{a:04X}")),
        "recorded": e.recorded,
    })
}

fn exe_json(exe: &PsxExe, bytes: &[u8]) -> Value {
    let h = |v: u32| format!("0x{v:08X}");
    json!({
        "pc": h(exe.pc),
        "gp": h(exe.gp),
        "text": {"addr": h(exe.text_addr), "size": h(exe.text_size), "end": h(exe.text_range().end), "file_offset": "0x800"},
        "data": {"addr": h(exe.data_addr), "size": h(exe.data_size)},
        "bss": {"addr": h(exe.bss_addr), "size": h(exe.bss_size)},
        "initial_sp": exe.initial_sp().map(h),
        "marker": exe.marker,
        "region_hint": exe.region_hint(),
        "file_size": exe.file_size,
        "sha256": sha256_bytes(bytes),
        "payload_sha256": sha256_bytes(exe.payload(bytes)),
        "warnings": exe.warnings,
    })
}

fn disc_summary(image: &Path, hash: bool) -> R {
    let disc = Disc::open(image)?;
    let files: Vec<Value> = disc
        .files
        .iter()
        .map(|f| {
            let mut v = json!({"name": f.name, "bytes": f.bytes});
            if hash {
                v["sha256"] = json!(sha256_file(&f.path).unwrap_or_else(|e| format!("error: {e}")));
            }
            v
        })
        .collect();
    let audio = disc.tracks.iter().filter(|t| !t.mode.is_data()).count();
    let mut out = json!({
        "source": disc.source,
        "files": files,
        "tracks": disc.tracks,
        "track_summary": {"total": disc.tracks.len(), "data": disc.tracks.len() - audio, "audio": audio},
        "end_lba": disc.end_lba(),
    });
    let fs = match FileSystem::read(&disc) {
        Ok(fs) => fs,
        Err(e) => {
            out["filesystem_error"] = json!(e.to_string());
            return Ok(out);
        }
    };
    let files_only = fs.entries.iter().filter(|e| !e.is_dir);
    out["volume"] = json!(fs.volume);
    out["entries"] = json!({
        "files": files_only.clone().count(),
        "dirs": fs.entries.iter().filter(|e| e.is_dir).count(),
        "streamed_files": files_only.clone().filter(|e| e.is_streamed()).count(),
        "bytes": files_only.map(|e| e.size as u64).sum::<u64>(),
    });
    out["fs_warnings"] = json!(fs.warnings);

    let cnf = fs.find("/SYSTEM.CNF").map(|e| fs.read_file(&disc, e)).transpose()?.map(|b| SystemCnf::parse(&b));
    let boot = cnf.as_ref().and_then(|c| c.boot.clone()).unwrap_or_else(|| DEFAULT_BOOT.to_string());
    out["system_cnf"] = json!(cnf);
    out["boot_path"] = json!(boot);
    match fs.find(&boot) {
        Some(e) => {
            let bytes = fs.read_file(&disc, e)?;
            out["boot_exe"] = match PsxExe::parse(&bytes) {
                Ok(exe) => json!({"entry": entry_json(e), "header": exe_json(&exe, &bytes)}),
                Err(err) => json!({"entry": entry_json(e), "error": err.to_string()}),
            };
        }
        None => out["boot_exe"] = json!({"error": format!("{boot} not found in file system")}),
    }
    Ok(out)
}

fn extract(image: &Path, path: Option<&str>, all: bool, out: &Path) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc)?;
    let selected: Vec<&Entry> = match (path, all) {
        (_, true) => fs.entries.iter().filter(|e| !e.is_dir).collect(),
        (Some(p), false) => vec![fs.find(p).ok_or_else(|| MediaError::NotFound(p.to_string()))?],
        (None, false) => return Err(MediaError::Malformed("give a path or --all".into())),
    };
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for e in selected {
        let (bytes, raw) = match fs.read_file(&disc, e) {
            Ok(b) => (b, false),
            Err(MediaError::Unsupported(_)) => match fs.read_file_raw(&disc, e) {
                Ok(b) => (b, true),
                Err(err) => {
                    failed.push(json!({"path": e.path, "error": err.to_string()}));
                    continue;
                }
            },
            Err(err) => {
                failed.push(json!({"path": e.path, "error": err.to_string()}));
                continue;
            }
        };
        let rel = e.path.trim_start_matches('/');
        let dest = out.join(if raw { format!("{rel}.raw") } else { rel.to_string() });
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|err| MediaError::io(parent, err))?;
        }
        std::fs::write(&dest, &bytes).map_err(|err| MediaError::io(&dest, err))?;
        written.push(json!({
            "path": e.path, "lba": e.lba, "dest": dest, "bytes": bytes.len(),
            "format": if raw { "raw_2352" } else { "form1_user_data" },
            "sha256": sha256_bytes(&bytes),
        }));
    }
    Ok(json!({"written": written, "failed": failed}))
}

fn exe_cmd(source: &str) -> R {
    let bytes = match source.split_once("::") {
        Some((image, path)) => {
            let disc = Disc::open(image)?;
            let fs = FileSystem::read(&disc)?;
            let e = fs.find(path).ok_or_else(|| MediaError::NotFound(path.to_string()))?;
            fs.read_file(&disc, e)?
        }
        None => std::fs::read(source).map_err(|e| MediaError::io(source, e))?,
    };
    let exe = PsxExe::parse(&bytes)?;
    Ok(exe_json(&exe, &bytes))
}

fn sectors(image: &Path, lba: u32, count: u32, preview: usize) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc).ok();
    let mut out = Vec::new();
    for l in lba..lba.saturating_add(count.min(256)) {
        let track = disc.track_for_lba(l).map(|t| t.number);
        let owner = fs.as_ref().and_then(|fs| fs.owner_of(l)).map(|e| e.path.clone());
        let v = match disc.read_data(l) {
            Ok(s) => json!({
                "lba": l, "track": track, "owner": owner, "form": s.form,
                "subheader": s.subheader.map(|h| json!({"file": h[0], "channel": h[1], "submode": format!("0x{:02X}", h[2]), "coding": format!("0x{:02X}", h[3])})),
                "preview": s.data.iter().take(preview).map(|b| format!("{b:02x}")).collect::<String>(),
            }),
            Err(e) => json!({"lba": l, "track": track, "owner": owner, "error": e.to_string()}),
        };
        out.push(v);
    }
    Ok(json!({"sectors": out}))
}

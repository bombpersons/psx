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

mod lockstep;
mod native;
mod provenance;
mod replay;
mod run;
mod run_native;
mod template;
mod textures;
mod vram_diff;
mod xa_cmd;

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
    /// List a file's XA-ADPCM audio channels (STR/XA streams), or decode one to a 44.1 kHz WAV.
    Xa {
        image: PathBuf,
        /// ISO path of the stream, e.g. `/MUSIC.XA`.
        path: String,
        /// Channel to decode (needs -o).
        #[arg(long)]
        channel: Option<u8>,
        /// Also require this subheader file number.
        #[arg(long)]
        file: Option<u8>,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// List the saves on a raw 128 KiB memory card image (.mcd/.mcr).
    Memcard { image: PathBuf },
    /// Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT
    /// pair a `--gpu-frame` capture used (with the UV bounds sampled), or one page.
    Textures {
        /// A native run directory (vram.bin, and gpu_frame.json unless --page).
        run_dir: PathBuf,
        /// One page as X,Y,DEPTH (VRAM halfword x, line y, 4/8/15 bpp).
        #[arg(long)]
        page: Option<String>,
        /// CLUT position X,Y for 4/8-bit pages.
        #[arg(long)]
        clut: Option<String>,
        /// Output directory (default <run_dir>/textures).
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
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
    /// Create a standalone native Bevy game project with the PS1 look.
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },
    /// Run a disc, EXE or state in a PCSX-Redux worker for N frames and capture
    /// RAM, VRAM, screen, registers and optional write watches.
    Run {
        /// Disc image (.cue/.bin/.iso) or PS-X EXE; omit to boot the BIOS or continue a state.
        media: Option<PathBuf>,
        #[arg(long)]
        bios: PathBuf,
        #[arg(long, default_value_t = 60)]
        frames: u32,
        /// JSON array of {"frame": n, "buttons": ["CROSS", ...]}.
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Write watch `ADDR[:WIDTH]` in hex, repeatable.
        #[arg(long = "watch")]
        watches: Vec<String>,
        #[arg(long)]
        from_state: Option<PathBuf>,
        #[arg(long)]
        save_state: bool,
        /// Output directory (default .psx/runs/<id>).
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Use the dynarec without the debugger: faster, but no --watch or traces.
        #[arg(long)]
        fast: bool,
        /// Log CD-ROM controller and CD DMA register writes (cd.jsonl).
        #[arg(long)]
        trace_cd: bool,
        /// Log BIOS A0/B0/C0 function calls (bios.jsonl).
        #[arg(long)]
        trace_bios: bool,
        /// `.symbols.json` (verified against RAM) or `.sym` files used to name PCs; repeatable.
        #[arg(long = "symbols")]
        symbols: Vec<PathBuf>,
        /// Cap on recorded events per trace log.
        #[arg(long, default_value_t = 200_000)]
        max_events: u32,
        /// Emulator: `redux` (PCSX-Redux worker, default) or `native` (psx-core).
        #[arg(long, default_value = "redux")]
        backend: String,
        #[command(flatten)]
        trace: run::TraceArgs,
        #[command(flatten)]
        cards: run::CardArgs,
    },
    /// Decode a `--trace-cd` run into disc → RAM load runs, mapped to ISO9660 files
    /// and verified against the run's final RAM.
    Loads {
        run_dir: PathBuf,
        /// The disc image the run used.
        #[arg(long)]
        disc: PathBuf,
        /// Include at most N individual runs (spans are always complete).
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// `.symbols.json` (verified against RAM) or `.sym` files from `psx symbols`; repeatable.
        #[arg(long = "symbols")]
        symbols: Vec<PathBuf>,
    },
    /// Name library functions in a PS-X EXE by PsyQ signature matching.
    /// Writes <out>.symbols.json, <out>.ghidra.txt (ImportSymbolsScript.py) and <out>.sym.
    Symbols {
        /// PS-X EXE file, or `image.cue::/PATH`.
        source: String,
        /// Local signature directory (`<version>/<LIB>.json` layout).
        #[arg(long, default_value = "external/psx_psyq_signatures")]
        sigs: PathBuf,
        /// Minimum non-wildcard bytes for a signature to be used.
        #[arg(long, default_value_t = 24)]
        min_concrete: usize,
        /// Output path prefix (default .psx/symbols/<exe name>).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run the native core and PCSX-Redux in lockstep from BIOS reset,
    /// comparing PC and registers after every instruction.
    Lockstep {
        #[arg(long)]
        bios: PathBuf,
        #[arg(long, default_value_t = 100_000)]
        steps: u64,
        /// Instructions of history shown at a divergence.
        #[arg(long, default_value_t = 12)]
        context: usize,
        /// GDB single-step with full register comparison (about 20 steps/s).
        #[arg(long)]
        exact: bool,
        /// Write the core's screen and VRAM as <PREFIX>.screen.png / .vram.png at the end.
        #[arg(long)]
        dump: Option<PathBuf>,
    },
    /// Run the native core alone (no oracle) for N frames; reports missing hardware.
    Native {
        #[arg(long)]
        bios: PathBuf,
        /// Disc image to insert (CUE/BIN/ISO); omit for an empty drive.
        #[arg(long)]
        disc: Option<PathBuf>,
        #[arg(long, default_value_t = 300)]
        frames: u64,
        #[arg(long, default_value_t = 20_000_000_000)]
        max_steps: u64,
        /// Write <PREFIX>.screen.png and <PREFIX>.vram.png at the end.
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Stop when the PC reaches this hex address and report the tail.
        #[arg(long = "break")]
        brk: Option<String>,
        /// Instructions of history kept for --break.
        #[arg(long, default_value_t = 48)]
        tail: usize,
        /// Report every change of this RAM word (hex address) with the responsible instruction.
        #[arg(long)]
        watch: Option<String>,
        /// Start recording --watch changes at this frame.
        #[arg(long, default_value_t = 0)]
        watch_after: u64,
        /// JSON `[{"frame": n, "buttons": ["CROSS"]}]` (frames from power-on).
        #[arg(long)]
        replay: Option<PathBuf>,
        /// With --dump: also save the screen every N frames.
        #[arg(long)]
        snapshot_every: Option<u64>,
        /// Capture the SPU output to a WAV file.
        #[arg(long)]
        wav: Option<PathBuf>,
        /// Controller in port 1: `digital` (default) or `dualshock`.
        #[arg(long, default_value = "digital")]
        pad: String,
    },
    /// Disassemble a PS-X EXE (file or `image.cue::/PATH`) at a RAM address range.
    Disasm {
        source: String,
        /// Start address (hex); defaults to the entry point.
        #[arg(long)]
        addr: Option<String>,
        #[arg(long, default_value_t = 32)]
        count: u32,
        /// `.sym` or `.symbols.json` files for labels; repeatable.
        #[arg(long = "symbols")]
        symbols: Vec<PathBuf>,
    },
    /// Replay a recorded GP0 stream (`[n, words..]` u32 records, e.g. tools/probes/gpu/gp0_stream.bin)
    /// into a fresh psx-gpu and write the resulting VRAM.
    GpuReplay {
        stream: PathBuf,
        /// Raw VRAM output (1024x512 little-endian halfwords).
        #[arg(long, short)]
        out: PathBuf,
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Compare two raw VRAM dumps (vram.bin) pixel by pixel.
    VramDiff {
        a: PathBuf,
        b: PathBuf,
        /// Cell size for per-block counts.
        #[arg(long, default_value_t = 128)]
        cell: usize,
        /// Region `X,Y,W,H` to compare (default: all of VRAM).
        #[arg(long)]
        region: Option<String>,
        /// Write a diff image (differing pixels red).
        #[arg(long)]
        png: Option<PathBuf>,
        /// Differing pixels listed individually.
        #[arg(long, default_value_t = 16)]
        samples: usize,
    },
    /// Classify a CPU address (segment, region, folded offset).
    Addr {
        /// Hex address, e.g. 0x80010000.
        address: String,
    },
}

#[derive(Subcommand)]
enum TemplateAction {
    /// Write the psx-bevy starter into an empty or new directory.
    Create {
        dest: PathBuf,
        /// Cargo package name (defaults to the directory name).
        #[arg(long)]
        name: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Run { media, bios, frames, replay, watches, from_state, save_state, out, timeout, fast, trace_cd, trace_bios, symbols, max_events, backend, trace, cards } = cli.command {
        let root = std::env::current_dir().unwrap_or_default();
        let (ok, v) = run::run(
            &root,
            run::RunArgs { media, bios, frames, replay, watches, from_state, save_state, out, timeout_s: timeout, fast, trace_cd, trace_bios, symbols, max_events, backend, trace, cards },
        );
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    if let Command::Native { bios, disc, frames, max_steps, dump, brk, tail, watch, watch_after, replay, snapshot_every, wav, pad } = &cli.command {
        let hex = |b: &Option<String>| b.as_deref().and_then(|b| u32::from_str_radix(b.trim_start_matches("0x").trim_start_matches("0X"), 16).ok());
        let (brk, watch) = (hex(brk), hex(watch));
        let (ok, v) = native::run(native::Options {
            bios,
            disc: disc.as_deref(),
            frames: *frames,
            max_steps: *max_steps,
            dump: dump.as_deref(),
            brk,
            tail: *tail,
            watch,
            watch_after: *watch_after,
            replay: replay.as_deref(),
            snapshot_every: *snapshot_every,
            wav: wav.as_deref(),
            pad,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    if let Command::Lockstep { bios, steps, context, exact, dump } = &cli.command {
        let root = std::env::current_dir().unwrap_or_default();
        let (ok, v) = if *exact {
            lockstep::run(&root, bios, *steps, *context)
        } else {
            lockstep::run_trace(&root, bios, *steps, *context, dump.as_deref())
        };
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return if ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
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
        Command::Textures { run_dir, page, clut, out } => textures::run(&run_dir, page.as_deref(), clut.as_deref(), out.as_deref()),
        Command::Memcard { image } => {
            let bytes = std::fs::read(&image).map_err(|e| MediaError::io(&image, e))?;
            if bytes.len() != psx_core::memcard::SIZE {
                return Err(MediaError::Malformed(format!("{} bytes; a raw memory card image is 131072", bytes.len())));
            }
            let card = psx_core::memcard::MemCard::from_image(bytes);
            let saves = card.saves();
            let used: usize = saves.iter().map(|s| s.blocks.len()).sum();
            let formatted = &card.sector(0)[..2] == b"MC";
            Ok(json!({"image": image, "formatted": formatted, "blocks_used": used, "blocks_free": 15 - used.min(15), "saves": saves}))
        }
        Command::Xa { image, path, file, channel, out } => xa_cmd::run(&image, &path, file, channel, out.as_deref()),
        Command::Sector { image, lba, count, preview } => sectors(&image, lba, count, preview),
        Command::Run { .. } | Command::Lockstep { .. } | Command::Native { .. } => unreachable!("handled in main"),
        Command::Symbols { source, sigs, min_concrete, out } => symbols_cmd(&source, &sigs, min_concrete, out),
        Command::Disasm { source, addr, count, symbols } => {
            let bytes = read_source(&source)?;
            let exe = PsxExe::parse(&bytes)?;
            let scopes = load_symbols(&symbols)?;
            let start = match addr {
                Some(a) => u32::from_str_radix(a.trim_start_matches("0x").trim_start_matches("0X"), 16)
                    .map_err(|_| MediaError::Malformed(format!("bad address {a}")))?,
                None => exe.pc,
            } & !3;
            let mut lines = Vec::new();
            for i in 0..count {
                let a = start.wrapping_add(4 * i);
                let Some(off) = exe.addr_to_file_offset(a) else { break };
                let Some(w) = bytes.get(off..off + 4) else { break };
                let word = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                let label = scopes.iter().find_map(|s| s.table.describe(a, 0).filter(|n| !n.contains('+')));
                lines.push(json!({"addr": format!("0x{a:08X}"), "word": format!("{word:08X}"), "asm": psx_core::disasm::disasm(word, a), "label": label}));
            }
            Ok(json!({"source": source, "lines": lines}))
        }
        Command::Loads { run_dir, disc, limit, symbols } => {
            let scopes = load_symbols(&symbols)?;
            let mut v = provenance::report(&run_dir, &disc, &scopes).map_err(MediaError::Malformed)?;
            if let Some(runs) = v["runs"].as_array_mut() {
                let total = runs.len();
                runs.truncate(limit);
                v["runs_total"] = json!(total);
            }
            Ok(v)
        }
        Command::Template { action: TemplateAction::Create { dest, name } } => template::create(&dest, name.as_deref()),
        Command::GpuReplay { stream, out, png } => vram_diff::replay(&stream, &out, png.as_deref()),
        Command::VramDiff { a, b, cell, region, png, samples } => {
            let region = match region {
                Some(r) => {
                    let v: Vec<usize> = r.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() != 4 {
                        return Err(MediaError::Malformed(format!("bad region {r}, expected X,Y,W,H")));
                    }
                    Some((v[0], v[1], v[2], v[3]))
                }
                None => None,
            };
            vram_diff::diff(vram_diff::DiffArgs { a: &a, b: &b, cell, region, png: png.as_deref(), samples })
        }
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
        // LibCrypt subchannel data: rips lack it; supply the redump .sbi/.lsd beside the image.
        "subchannel": {
            "source": disc.subq_source,
            "overridden_sectors": disc.subq.len(),
            "first_lbas": disc.subq.keys().take(4).collect::<Vec<_>>(),
        },
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

/// Symbols for one executable, with its code bytes when known so that a name
/// is only applied where that code is actually resident.
pub(crate) struct ScopedSymbols {
    pub name: String,
    pub table: psx_symbols::SymbolTable,
    /// `(text_addr, payload)` from a `.symbols.json` report; `None` for bare `.sym` files.
    pub code: Option<(u32, Vec<u8>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolCheck {
    /// The executable's bytes around the PC match the RAM given.
    RamMatch,
    /// Bare `.sym` file: no code to compare against.
    Unverified,
}

impl SymbolCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolCheck::RamMatch => "ram_match",
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
            let source = v["source"].as_str().unwrap_or_default().to_string();
            let report: psx_symbols::MatchReportIn = serde_json::from_value(v["report"].clone())
                .map_err(|e| MediaError::Malformed(format!("{name}: {e}")))?;
            let bytes = read_source(&source)?;
            let exe = PsxExe::parse(&bytes)?;
            out.push(ScopedSymbols {
                name: source.rsplit(['/', '\\', ':']).next().unwrap_or(&source).trim_end_matches(";1").to_string(),
                table: psx_symbols::SymbolTable::from_symbols(&report.symbols),
                code: Some((exe.text_addr, exe.payload(&bytes).to_vec())),
            });
        } else {
            out.push(ScopedSymbols {
                name: name.trim_end_matches(".sym").to_string(),
                table: psx_symbols::SymbolTable::from_sym_text(&text),
                code: None,
            });
        }
    }
    Ok(out)
}

/// Name `pc` from the first executable whose code is resident there in `ram`
/// (2 MiB main RAM image). Without RAM, or for bare `.sym` files, the name is unverified.
pub(crate) fn name_pc(scopes: &[ScopedSymbols], pc: u32, ram: Option<&[u8]>) -> Option<(String, SymbolCheck, String)> {
    for s in scopes {
        let Some(n) = s.table.describe(pc, 0x4000) else { continue };
        match (&s.code, ram) {
            (Some((base, code)), Some(ram)) => {
                let off = pc.wrapping_sub(*base) as usize & !3;
                let lo = off.saturating_sub(16);
                let hi = (off + 16).min(code.len());
                let r = (*base as usize & 0x1F_FFFF) + lo;
                if lo < hi && ram.get(r..r + (hi - lo)) == Some(&code[lo..hi]) {
                    return Some((n, SymbolCheck::RamMatch, s.name.clone()));
                }
            }
            (Some(_), None) | (None, _) => return Some((n, SymbolCheck::Unverified, s.name.clone())),
        }
    }
    None
}

fn read_source(source: &str) -> psx_media::Result<Vec<u8>> {
    Ok(match source.split_once("::") {
        Some((image, path)) => {
            let disc = Disc::open(image)?;
            let fs = FileSystem::read(&disc)?;
            let e = fs.find(path).ok_or_else(|| MediaError::NotFound(path.to_string()))?;
            fs.read_file(&disc, e)?
        }
        None => std::fs::read(source).map_err(|e| MediaError::io(source, e))?,
    })
}

fn symbols_cmd(source: &str, sigs_dir: &Path, min_concrete: usize, out: Option<PathBuf>) -> R {
    let bytes = read_source(source)?;
    let exe = PsxExe::parse(&bytes)?;
    let sigs = psx_symbols::load_dir(sigs_dir).map_err(|e| {
        MediaError::NotFound(format!("signatures: {e} (supply a local psx_psyq_signatures checkout with --sigs)"))
    })?;
    let report = psx_symbols::match_code(exe.payload(&bytes), exe.text_addr, &sigs, min_concrete);
    let stem = source.rsplit(['/', '\\', ':']).next().unwrap_or("exe").trim_end_matches(";1").to_string();
    let prefix = out.unwrap_or_else(|| PathBuf::from(".psx/symbols").join(&stem));
    if let Some(parent) = prefix.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MediaError::io(parent, e))?;
    }
    let with_ext = |ext: &str| PathBuf::from(format!("{}.{ext}", prefix.display()));
    let write = |p: PathBuf, text: String| std::fs::write(&p, text).map_err(|e| MediaError::io(&p, e)).map(|_| p);
    let doc = json!({
        "source": source,
        "text_addr": exe.text_addr,
        "exe_sha256": psx_media::disc::sha256_bytes(&bytes),
        "report": report,
    });
    let json_path = write(with_ext("symbols.json"), serde_json::to_string_pretty(&doc).unwrap())?;
    let ghidra = write(with_ext("ghidra.txt"), psx_symbols::to_ghidra(&report.symbols))?;
    let sym = write(with_ext("sym"), psx_symbols::to_sym(&report.symbols))?;
    let functions = report.symbols.iter().filter(|s| s.kind == psx_symbols::Kind::Function).count();
    let mut libs: std::collections::BTreeMap<String, usize> = Default::default();
    for o in &report.objects {
        let lib = o.source.split('/').nth(1).unwrap_or("?").to_string();
        *libs.entry(lib).or_default() += 1;
    }
    let matched: u64 = report.objects.iter().map(|o| o.size as u64).sum();
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
        "files": {"report": json_path, "ghidra": ghidra, "sym": sym},
    }))
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

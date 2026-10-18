//! `psx-oracle`: developer checks of the native core against PCSX-Redux.
//!
//! Not part of the `psx` toolkit agents use. `run` performs a bounded Redux run
//! and writes the same RAM/VRAM/screen artifacts as `psx run`, for diffing
//! (`psx vram-diff`, `psx ram-diff`); `lockstep` compares the CPU instruction by
//! instruction from BIOS reset. Needs external/pcsx-redux or `PSX_REDUX_BIN`.
//! Run from the project root: `cargo run -p psx-oracle -- run ...`.

mod lockstep;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use psx_redux::{Backend, Job, Media, ReplayEvent, Watch};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(about = "Check the native core against a PCSX-Redux oracle (psx development only)")]
struct Cli {
    /// Project root holding bios/, external/ and .psx/.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bounded run in a Redux worker; writes ram.bin, vram.bin, screen.png and a JSON report.
    Run {
        /// Disc image (.cue/.bin/.iso/.chd) or PS-X EXE; omit to boot the BIOS or continue a state.
        media: Option<PathBuf>,
        #[arg(long)]
        bios: PathBuf,
        #[arg(long, default_value_t = 60)]
        frames: u32,
        /// Output directory (default .psx/oracle/<time>-<pid>).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Write watch `ADDR[:1|2|4]` (hex); repeatable.
        #[arg(long = "watch")]
        watches: Vec<String>,
        /// Redux button schedule: JSON `[{"frame": N, "buttons": ["CROSS", ...]}]`.
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Continue from a Redux save state.
        #[arg(long)]
        from_state: Option<PathBuf>,
        #[arg(long)]
        save_state: bool,
        /// Dynarec without the debugger: faster, no watches.
        #[arg(long)]
        fast: bool,
        #[arg(long)]
        dualshock: bool,
        #[arg(long)]
        trace_cd: bool,
        #[arg(long)]
        trace_bios: bool,
        #[arg(long, default_value_t = 10_000)]
        max_events: u32,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Run the native core and Redux side by side from BIOS reset; stop at the first divergence.
    Lockstep {
        #[arg(long)]
        bios: PathBuf,
        /// Instructions to compare from reset.
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
        /// Wall-clock limit in seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Redux runs inside its job directory, so every path it is given must be absolute.
    let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
    let root = &abs(cli.root);
    let (ok, value) = match cli.command {
        Command::Lockstep { bios, steps, context, exact, dump, timeout } => {
            let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));
            if exact {
                lockstep::run(root, &bios, steps, context, deadline)
            } else {
                lockstep::run_trace(root, &bios, steps, context, dump.as_deref(), deadline)
            }
        }
        Command::Run { media, bios, frames, out, watches, replay, from_state, save_state, fast, dualshock, trace_cd, trace_bios, max_events, timeout } => {
            let (bios, media, replay, from_state) = (abs(bios), media.map(abs), replay.map(abs), from_state.map(abs));
            let job = (|| {
                let watches = watches.iter().map(|w| parse_watch(w)).collect::<Result<Vec<_>, _>>()?;
                let replay: Vec<ReplayEvent> = match replay {
                    Some(p) => serde_json::from_slice(&std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?)
                        .map_err(|e| format!("{}: {e}", p.display()))?,
                    None => Vec::new(),
                };
                let media = match media {
                    None => Media::None,
                    Some(p) if psx_media::exe::is_exe_path(&p) => Media::Exe(p),
                    Some(p) => Media::Disc(redux_disc(root, &p).map_err(|e| e.to_string())?),
                };
                Ok::<_, String>(Job {
                    bios,
                    media,
                    frames,
                    replay,
                    watches,
                    max_watch_hits: 10_000,
                    load_state: from_state,
                    save_state,
                    timeout: Duration::from_secs(timeout),
                    realtime: false,
                    trace_cd,
                    trace_bios,
                    max_events,
                    analog_pad: dualshock,
                })
            })();
            match job {
                Ok(job) => run(root, &job, !fast, out),
                Err(e) => (false, json!({"error": e})),
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn run(root: &Path, job: &Job, interpreter: bool, out: Option<PathBuf>) -> (bool, Value) {
    let mut backend = match Backend::from_project(root) {
        Ok(b) => b,
        Err(e) => return (false, json!({"error": e.to_string()})),
    };
    backend.interpreter = interpreter;
    let dir = out.unwrap_or_else(|| {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        root.join(".psx/oracle").join(format!("{stamp}-{}", std::process::id()))
    });
    let out = match psx_redux::run(&backend, job, &dir) {
        Ok(o) => o,
        Err(e) => return (false, json!({"error": e.to_string()})),
    };
    if let Ok((w, h, rgb)) = out.screen_rgb() {
        let _ = write_png(&out.file("screen.png"), w, h, &rgb, png::ColorType::Rgb);
    }
    if let Ok(bytes) = std::fs::read(out.file("vram.bin")) {
        let mut vram = psx_gpu::Vram::default();
        vram.load_bytes(&bytes);
        let _ = write_png(&out.file("vram.png"), 1024, 512, &vram.rgba15(0, 0, 1024, 512), png::ColorType::Rgba);
    }
    let complete = out.report.termination == "frames_reached" && out.report.reached_frames >= job.frames;
    (
        complete,
        json!({
            "status": if complete { "complete" } else { "partial" },
            "run_dir": out.dir,
            "termination": out.report.termination,
            "frames": {"requested": out.report.requested_frames, "reached": out.report.reached_frames},
            "cpu_cycles": out.report.cpu_cycles,
            "registers": out.report.registers,
            "watch": {"hits": out.report.watch_hits, "first": out.hits.iter().take(64).collect::<Vec<_>>()},
            "event_logs": out.report.event_logs,
            "bios_call_counts": out.report.bios_call_counts,
            "identity": {"backend": backend.binary, "backend_sha256": backend.identity().ok(), "interpreter": backend.interpreter},
            "wall_ms": out.wall_ms,
        }),
    )
}

fn parse_watch(s: &str) -> Result<Watch, String> {
    let (addr, width) = s.split_once(':').unwrap_or((s, "4"));
    let addr = psx_media::memmap::parse_hex(addr).ok_or_else(|| format!("bad watch address {addr:?}"))?;
    let width: u32 = width.parse().map_err(|_| format!("bad watch width {width:?}"))?;
    if ![1, 2, 4].contains(&width) {
        return Err("watch width must be 1, 2 or 4".into());
    }
    Ok(Watch { addr, width })
}

/// A disc path Redux can open: CUE/BIN/ISO as given; a CHD (which Redux cannot
/// read) is written once as CUE/BIN under `.psx/cache/discs/<content fingerprint>/`.
fn redux_disc(root: &Path, p: &Path) -> psx_media::Result<PathBuf> {
    use psx_media::MediaError;
    if !p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("chd")) {
        return Ok(p.to_path_buf());
    }
    let disc = psx_media::Disc::open(p)?;
    let dir = root.join(".psx/cache/discs").join(psx_core::state::disc_fingerprint(&disc));
    let cue = dir.join("disc.cue");
    if !cue.is_file() {
        let tmp = dir.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).map_err(|e| MediaError::io(&tmp, e))?;
        disc.write_bin_cue(&tmp, "disc")?;
        if let Err(e) = std::fs::rename(&tmp, &dir) {
            let _ = std::fs::remove_dir_all(&tmp);
            if !cue.is_file() {
                return Err(MediaError::io(&dir, e));
            }
        }
    }
    Ok(cue)
}

pub(crate) fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8], color: png::ColorType) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgb).map_err(std::io::Error::other)
}

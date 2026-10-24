//! `psx-oracle`: developer checks of the native core against Mednafen's
//! PlayStation emulation, which is taken as the ground truth.
//!
//! Not part of the `psx` toolkit agents use. `run` plays a disc, PS-X EXE or
//! the BIOS alone and writes the same RAM/VRAM/screen artifacts as `psx run`,
//! for diffing (`psx vram-diff`, `psx ram-diff`); `lockstep` compares the CPU
//! instruction by instruction from BIOS reset.
//!
//! GPL-2.0-or-later: built from Mednafen 1.32.1's sources (`mednafen/`, see
//! README.md). Run from the project root: `cargo run --release -p psx-oracle -- run ...`.

#[cfg(mednafen)]
mod ffi;
#[cfg(mednafen)]
mod lockstep;
#[cfg(mednafen)]
mod run;
mod suite;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "Check the native core against Mednafen (psx development only)")]
struct Cli {
    /// Project root holding bios/ and .psx/.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bounded run; writes ram.bin, scratch.bin, vram.bin, spu.bin, screen.png, vram.png and a JSON report.
    Run {
        /// Disc image (.cue/.ccd/.toc/.m3u/.chd) or PS-X EXE; omit to boot the BIOS with an empty drive.
        media: Option<PathBuf>,
        #[arg(long)]
        bios: PathBuf,
        /// Vblanks to run, as `psx run --frames`.
        #[arg(long, default_value_t = 60)]
        frames: u32,
        /// Output directory (default .psx/oracle/<time>-<pid>).
        #[arg(long)]
        out: Option<PathBuf>,
        /// CPU write watch `ADDR[:1|2|4]` (hex); repeatable. DMA writes aren't seen.
        #[arg(long = "watch")]
        watches: Vec<String>,
        /// Button schedule, as for `psx run --replay`: JSON `[{"frame": N, "buttons": ["CROSS", ...]}]`.
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Continue from a Mednafen save state (state.mcs from --save-state).
        #[arg(long)]
        from_state: Option<PathBuf>,
        /// Write state.mcs at the end.
        #[arg(long)]
        save_state: bool,
        /// Port 1 is a DualShock (replay `sticks` apply) instead of a digital pad.
        #[arg(long)]
        dualshock: bool,
        /// Write frames.csv: each frame's CPU cycles and display size.
        #[arg(long)]
        frame_log: bool,
        /// Print Mednafen's own messages.
        #[arg(long)]
        verbose: bool,
    },
    /// Run the native core and Mednafen side by side from BIOS reset; stop at the first divergence.
    Lockstep {
        #[arg(long)]
        bios: PathBuf,
        /// Instructions to compare from reset.
        #[arg(long, default_value_t = 1_000_000)]
        steps: u64,
        /// Instructions of history shown at a divergence.
        #[arg(long, default_value_t = 12)]
        context: usize,
        /// Stop at the first instruction whose cycle cost differs from Mednafen's.
        #[arg(long)]
        cycles: bool,
        /// A disc image (.cue, .bin) in the drive of both, from power-on; default empty.
        #[arg(long)]
        disc: Option<PathBuf>,
        /// Pad 1 input, as for `run --replay` (frames count from power-on).
        #[arg(long)]
        replay: Option<PathBuf>,
        /// At the end, write both sides' memory as <PREFIX>.{core,mednafen}.{ram,scratchpad,vram,spu_ram}.bin, and the core's screen and VRAM as PNGs.
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Print each value adopted from this I/O register (hex address) to stderr, one JSON line each.
        #[arg(long, value_parser = parse_hex)]
        trace_adopted: Option<u32>,
        /// Run each frame at full speed and compare at its end; a frame that differs is run again instruction by instruction from the last checkpoint.
        #[arg(long, conflicts_with = "trace_adopted")]
        fast: bool,
        /// With --fast, frames between checkpoints.
        #[arg(long, default_value_t = 60)]
        checkpoint_every: u64,
    },
    /// Run the lockstep suite (suite/suite.json) with --fast; each run must match with nothing adopted.
    Suite {
        /// The suite file; default the crate's suite/suite.json.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Only these runs, by name.
        #[arg(long)]
        only: Vec<String>,
        /// Runs at once.
        #[arg(long, default_value_t = 3)]
        jobs: usize,
    },
}

fn parse_hex(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).map_err(|e| format!("{s}: {e}"))
}

/// One pad state from `frame` on: buttons in the pad's bit order, sticks lx, ly, rx, ry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub frame: u32,
    pub buttons: u16,
    pub sticks: [u8; 4],
}

/// What `lockstep` compares, from its command line.
pub struct LockstepJob {
    pub bios: PathBuf,
    pub steps: u64,
    pub context: usize,
    pub strict_cycles: bool,
    pub disc: Option<PathBuf>,
    pub inputs: Vec<Input>,
    pub dump: Option<PathBuf>,
    pub trace_adopted: Option<u32>,
    pub fast: bool,
    pub checkpoint_every: u64,
}

/// A replay file as `psx run` reads it: `frame`, `buttons`, optional
/// `sticks` and `until` (a neutral state from that frame).
fn replay(path: &Path) -> Result<Vec<Input>, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let events: Vec<Value> = serde_json::from_slice(&std::fs::read(path).map_err(|e| err(&e))?).map_err(|e| err(&e))?;
    let mut out = Vec::new();
    for e in &events {
        let frame = e["frame"].as_u64().ok_or_else(|| err(&"each event needs a frame"))? as u32;
        let mut buttons = 0;
        for b in e["buttons"].as_array().into_iter().flatten() {
            let name = b.as_str().unwrap_or_default();
            buttons |= psx_core::sio0::button_from_name(name).ok_or_else(|| err(&format!("unknown button {b}")))?;
        }
        let sticks = match &e["sticks"] {
            Value::Null => [0x80; 4],
            s => serde_json::from_value(s.clone()).map_err(|e| err(&e))?,
        };
        out.push(Input { frame, buttons, sticks });
        if let Some(until) = e["until"].as_u64() {
            out.push(Input { frame: until as u32, buttons: 0, sticks: [0x80; 4] });
        }
    }
    out.sort_by_key(|i| i.frame);
    Ok(out)
}

fn parse_watch(s: &str) -> Result<(u32, u32), String> {
    let (addr, width) = s.split_once(':').unwrap_or((s, "4"));
    let addr = psx_media::memmap::parse_hex(addr).ok_or_else(|| format!("bad watch address {addr:?}"))?;
    let width: u32 = width.parse().map_err(|_| format!("bad watch width {width:?}"))?;
    if ![1, 2, 4].contains(&width) {
        return Err("watch width must be 1, 2 or 4".into());
    }
    Ok((addr & 0x1FFF_FFFF, width))
}

/// A disc path Mednafen can open: as given, except a CHD (which it can't
/// read), written once as CUE/BIN under `.psx/cache/discs/<content fingerprint>/`.
fn oracle_disc(root: &Path, p: &Path) -> psx_media::Result<PathBuf> {
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

pub(crate) fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = std::fs::File::create(path).map_err(|e| err(&e))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(rgba)).map_err(|e| err(&e))
}

pub struct RunJob {
    pub media: Option<PathBuf>,
    pub bios: PathBuf,
    pub frames: u32,
    pub inputs: Vec<Input>,
    pub watches: Vec<(u32, u32)>,
    pub from_state: Option<PathBuf>,
    pub save_state: bool,
    pub dualshock: bool,
    pub frame_log: bool,
    pub verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p);
    let root = abs(cli.root);
    let (ok, value) = match cli.command {
        Command::Run {
            media,
            bios,
            frames,
            out,
            watches,
            replay: replay_path,
            from_state,
            save_state,
            dualshock,
            frame_log,
            verbose,
        } => {
            let job = (|| {
                let media = match media.map(abs) {
                    Some(p) if psx_media::exe::is_exe_path(&p) => Some(p),
                    Some(p) => Some(oracle_disc(&root, &p).map_err(|e| e.to_string())?),
                    None => None,
                };
                Ok::<_, String>(RunJob {
                    media,
                    bios: abs(bios),
                    frames,
                    inputs: replay_path.as_deref().map(replay).transpose()?.unwrap_or_default(),
                    watches: watches.iter().map(|w| parse_watch(w)).collect::<Result<_, _>>()?,
                    from_state: from_state.map(abs),
                    save_state,
                    dualshock,
                    frame_log,
                    verbose,
                })
            })();
            let dir = abs(out.unwrap_or_else(|| {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                root.join(".psx/oracle").join(format!("{stamp}-{}", std::process::id()))
            }));
            match job.and_then(|job| run_job(&job, &dir)) {
                Ok(v) => (v["status"] == "complete", v),
                Err(e) => (false, json!({"error": e})),
            }
        }
        Command::Lockstep {
            bios,
            steps,
            context,
            cycles,
            disc,
            replay: replay_path,
            dump,
            trace_adopted,
            fast,
            checkpoint_every,
        } => match replay_path.as_deref().map(replay).transpose() {
            Ok(inputs) => lockstep_job(
                &root,
                &LockstepJob {
                    bios: abs(bios),
                    steps,
                    context,
                    strict_cycles: cycles,
                    disc: disc.map(abs),
                    inputs: inputs.unwrap_or_default(),
                    dump,
                    trace_adopted,
                    fast,
                    checkpoint_every,
                },
            ),
            Err(e) => (false, json!({"error": e})),
        },
        Command::Suite { file, only, jobs } => {
            suite::run(&root, &file.map(abs).unwrap_or_else(suite::default_file), &only, jobs)
        }
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(mednafen)]
fn run_job(job: &RunJob, dir: &Path) -> Result<Value, String> {
    run::run(job, dir)
}

#[cfg(mednafen)]
fn lockstep_job(root: &Path, job: &LockstepJob) -> (bool, Value) {
    lockstep::run(root, job)
}

#[cfg(not(mednafen))]
fn run_job(_: &RunJob, _: &Path) -> Result<Value, String> {
    Err("psx-oracle is only built for x86_64 Linux".into())
}

#[cfg(not(mednafen))]
fn lockstep_job(_: &Path, _: &LockstepJob) -> (bool, Value) {
    (false, json!({"error": "psx-oracle is only built for x86_64 Linux"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_read_like_psx_run_and_reject_unknown_buttons() {
        let dir = std::env::temp_dir().join(format!("psx-oracle-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.json");
        std::fs::write(&path, r#"[{"frame": 30, "buttons": []}, {"frame": 10, "buttons": ["cross", "START"], "sticks": [0, 255, 128, 128], "until": 20}]"#).unwrap();
        let inputs = replay(&path).unwrap();
        assert_eq!(
            inputs,
            vec![
                Input { frame: 10, buttons: 1 << 14 | 1 << 3, sticks: [0, 255, 128, 128] },
                Input { frame: 20, buttons: 0, sticks: [128; 4] },
                Input { frame: 30, buttons: 0, sticks: [128; 4] },
            ]
        );
        std::fs::write(&path, r#"[{"frame": 0, "buttons": ["X"]}]"#).unwrap();
        assert!(replay(&path).is_err());
        assert_eq!(parse_watch("80014808:2").unwrap(), (0x14808, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

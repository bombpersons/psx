//! A scenario recording as the port draws it, one PNG per tick, for `psx
//! web` to show beside the original's screens. psx web draws it through the
//! port protocol (`--psx`, [`crate::protocol`]); the game draws it itself
//! with
//!
//! ```text
//! cargo run -p game -- --replay scenarios/<id>/<name>.jsonl --frames DIR
//! ```
//!
//! The game's `main` hands its arguments to [`main`] first:
//!
//! ```ignore
//! fn main() -> std::process::ExitCode {
//!     if let Some(code) = psx_scenario::replay::main(std::env::args(), Game::headless) {
//!         return code;
//!     }
//!     // The game's own loop.
//! }
//! ```
//!
//! It launches the scenario as the scenario tests do, then runs each of the
//! recording's ticks with its pad alone. `DIR/<tick>.png` is
//! [`Port::screen`] after that tick, at the game's own resolution: `0.png`
//! is the initial moment, after the launcher. `DIR/frames.json` says how
//! many were drawn and what stopped them early; `no_screen` there says the
//! port draws nothing yet. Progress goes to stdout (`tick N/M`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::runner::{Pad, Port};
use crate::scenario::{Recording, Scenario};

/// The screen as the port drew it: `width * height` pixels, RGBA, rows top
/// to bottom. Alpha is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    /// Writes the frame as an RGB PNG.
    pub fn save_png(&self, path: &Path) -> Result<(), String> {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 || self.rgba.len() != w * h * 4 {
            return Err(format!(
                "a {}x{} frame needs {} bytes of RGBA, not {}",
                self.width,
                self.height,
                w * h * 4,
                self.rgba.len()
            ));
        }
        let rgb: Vec<u8> = self.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| format!("{}: {e}", path.display()))?;
        w.write_image_data(&rgb).map_err(|e| format!("{}: {e}", path.display()))?;
        w.finish().map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// What the game's `main` hands its arguments to first. `--psx` serves the
/// port protocol on stdin and stdout, with a fresh port from `port` for
/// each launch ([`crate::protocol`]): `psx port check` and psx web drive the
/// port that way. `--replay REC --frames DIR` draws the recording here.
/// None when neither is there. Other arguments (`--software`,
/// `--skip-import`) are left to the game.
pub fn main<P: Port>(args: impl IntoIterator<Item = String>, port: impl Fn() -> P) -> Option<ExitCode> {
    let args: Vec<String> = args.into_iter().collect();
    if args.iter().any(|a| a == "--psx") {
        let stdin = std::io::stdin().lock();
        return Some(match crate::protocol::serve(port, stdin, std::io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("psx: {e}");
                ExitCode::FAILURE
            }
        });
    }
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let recording = value("--replay")?;
    let Some(out) = value("--frames") else {
        eprintln!("replay: --replay needs --frames DIR");
        return Some(ExitCode::FAILURE);
    };
    Some(if run(port(), &recording, &out).is_ok() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Draws `recording` into `out` and writes `out/frames.json`; progress goes
/// to stdout.
pub fn run<P: Port>(port: P, recording: &Path, out: &Path) -> Result<(), String> {
    run_with(port, recording, out, |line| println!("{line}"))
}

/// [`run`], with progress (`tick N/M`) to `progress`.
pub fn run_with<P: Port>(
    mut port: P,
    recording: &Path,
    out: &Path,
    mut progress: impl FnMut(&str),
) -> Result<(), String> {
    let mut summary = json!({"recording": recording, "drawn": 0});
    let result = std::fs::create_dir_all(out)
        .map_err(|e| format!("{}: {e}", out.display()))
        .and_then(|_| draw(&mut port, recording, out, &mut summary, &mut progress));
    if let Err(e) = &result {
        summary["error"] = json!(e);
        eprintln!("replay: {e}");
    }
    let _ = std::fs::write(out.join("frames.json"), serde_json::to_vec_pretty(&summary).unwrap_or_default());
    result
}

fn draw<P: Port>(
    port: &mut P,
    recording: &Path,
    out: &Path,
    summary: &mut Value,
    progress: &mut impl FnMut(&str),
) -> Result<(), String> {
    let (scenario, rec) = load(recording)?;
    if !port.kinds().contains(&scenario.kind) {
        return Err(format!("no launcher registered for scenario kind {}", scenario.kind));
    }
    port.launch(&scenario.kind, &scenario.parameters).map_err(|e| format!("the launcher failed: {e}"))?;
    let ticks = rec.expand();
    summary["ticks"] = json!(ticks.len());
    let Some(first) = port.screen() else {
        // Nothing to draw: said once, not a tick at a time.
        summary["no_screen"] = json!(true);
        progress("the port draws no screen");
        return Ok(());
    };
    summary["width"] = json!(first.width);
    summary["height"] = json!(first.height);
    first.save_png(&out.join("0.png"))?;
    let mut drawn = 1;
    let mut failure = None;
    for t in &ticks {
        let pad = match Pad::of(t) {
            Ok(p) => p,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            port.tick(&pad);
            port.screen()
        }));
        let saved = match run {
            Err(p) => Err(format!("tick {} panicked: {}", t.tick, panic_message(&p))),
            Ok(None) => Err(format!("tick {}: the port drew no screen", t.tick)),
            Ok(Some(f)) => {
                f.save_png(&out.join(format!("{}.png", t.tick))).map_err(|e| format!("tick {}: {e}", t.tick))
            }
        };
        if let Err(e) = saved {
            failure = Some(e);
            break;
        }
        drawn += 1;
        summary["drawn"] = json!(drawn);
        if t.tick % 60 == 0 {
            progress(&format!("tick {}/{}", t.tick, ticks.len()));
        }
    }
    summary["drawn"] = json!(drawn);
    progress(&format!("tick {}/{} drawn {drawn}", ticks.len(), ticks.len()));
    failure.map_or(Ok(()), Err)
}

/// The scenario (`scenarios/<id>.toml`) and the recording.
fn load(recording: &Path) -> Result<(Scenario, Recording), String> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let id = recording
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .ok_or(format!("{}: not scenarios/<id>/<name>.jsonl", recording.display()))?;
    let toml = recording.parent().and_then(|d| d.parent()).unwrap_or(Path::new(".")).join(format!("{id}.toml"));
    let scenario = Scenario::parse(&read(&toml)?)?;
    let rec = Recording::parse(&read(recording)?)?;
    let problems = rec.check(id);
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok((scenario, rec))
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|m| m.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Button, Parameters};
    use crate::value::Value;

    /// A dot moving right while RIGHT is held, drawn on a 4x1 screen when
    /// `draws`.
    struct Dot {
        x: usize,
        draws: bool,
    }

    impl Port for Dot {
        type Observed = Vec<(&'static str, Option<Value>)>;

        fn kinds(&self) -> Vec<String> {
            vec!["level".into()]
        }

        fn launch(&mut self, _: &str, _: &Parameters) -> Result<(), String> {
            Ok(())
        }

        fn tick(&mut self, pad: &Pad) {
            if pad.held(Button::Right) {
                self.x += 1;
            }
        }

        fn observe(&mut self) -> Result<Self::Observed, String> {
            Ok(vec![("x", Some(Value::Int(self.x as i128)))])
        }

        fn screen(&mut self) -> Option<Frame> {
            let mut rgba = vec![0; 16];
            rgba[self.x * 4] = 255;
            self.draws.then_some(Frame { width: 4, height: 1, rgba })
        }
    }

    const RECORDING: &str = r#"{"format":"scenario-recording","format_version":1,"scenario":"walk","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0"},"quantities":{"x":{"type":"int","bits":32,"signed":true}},"initial":{"x":0}}
{"tick":1,"field":1,"pad":["RIGHT"],"values":{"x":1}}
{"tick":2,"field":2,"pad":[],"values":{"x":1}}
"#;

    fn workspace(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("psx-scenario-replay-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scenarios/walk")).unwrap();
        std::fs::write(
            root.join("scenarios/walk.toml"),
            "moment = \"m\"\nchain = \"c\"\nkind = \"level\"\n[compare]\nx = 0\n",
        )
        .unwrap();
        std::fs::write(root.join("scenarios/walk/a.jsonl"), RECORDING).unwrap();
        root
    }

    fn summary(out: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(out.join("frames.json")).unwrap()).unwrap()
    }

    #[test]
    fn draws_a_png_a_tick_from_the_initial_moment() {
        let root = workspace("draws");
        let (rec, out) = (root.join("scenarios/walk/a.jsonl"), root.join("frames"));
        let args = ["game", "--replay", rec.to_str().unwrap(), "--frames", out.to_str().unwrap(), "--software"];
        let code = main(args.map(String::from), || Dot { x: 0, draws: true });
        assert_eq!(code, Some(ExitCode::SUCCESS));
        let s = summary(&out);
        assert_eq!((s["drawn"].clone(), s["ticks"].clone(), s["width"].clone()), (json!(3), json!(2), json!(4)));
        for (tick, x) in [(0, 0), (1, 1), (2, 1)] {
            let img = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(out.join(format!("{tick}.png"))).unwrap(),
            ));
            let mut r = img.read_info().unwrap();
            let mut buf = vec![0; r.output_buffer_size().unwrap()];
            r.next_frame(&mut buf).unwrap();
            assert_eq!(buf[x * 3], 255, "tick {tick}");
        }
        assert_eq!(main(["game".to_string()], || Dot { x: 0, draws: true }), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_port_without_a_screen_says_so() {
        let root = workspace("blank");
        let out = root.join("frames");
        run(Dot { x: 0, draws: false }, &root.join("scenarios/walk/a.jsonl"), &out).unwrap();
        let s = summary(&out);
        assert_eq!((s["no_screen"].clone(), s["drawn"].clone(), s.get("error")), (json!(true), json!(0), None));
        // A recording that is not there fails into frames.json.
        assert!(run(Dot { x: 0, draws: false }, &root.join("scenarios/walk/b.jsonl"), &out).is_err());
        assert!(summary(&out)["error"].as_str().unwrap().contains("b.jsonl"));
        let _ = std::fs::remove_dir_all(root);
    }
}

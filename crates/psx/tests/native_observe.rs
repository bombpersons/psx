//! Per-frame samples and pokes against real game data (Mega Man 8 PAL, the
//! in-stage state of research/case-study-megaman8.md). Needs the local data:
//!
//! ```bash
//! PSX_ROOT=/path/to/main/checkout cargo test -p psx --release --test native_observe -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_psx");

fn root() -> Option<PathBuf> {
    let r = PathBuf::from(std::env::var_os("PSX_ROOT")?);
    r.join(".psx/mm8-study/s3900/state.bin").is_file().then_some(r)
}

fn run(root: &Path, out: &Path, extra: &[&str]) -> Value {
    let replay = out.with_extension("json");
    std::fs::write(&replay, r#"[{"frame": 0, "buttons": ["CROSS"]}]"#).unwrap();
    let mut args = vec![
        "run", "media/Mega Man 8.cue", "--bios", "bios/scph7502.bin", "--backend", "native", "--from-state", ".psx/mm8-study/s3900/state.bin",
        "--replay", replay.to_str().unwrap(), "--out", out.to_str().unwrap(), "--brief",
    ];
    args.extend_from_slice(extra);
    let o = Command::new(BIN).args(&args).env("PSX_ROOT", root).output().unwrap();
    serde_json::from_slice(&o.stdout).unwrap()
}

fn column(csv: &str, name: &str) -> Vec<String> {
    let mut lines = csv.lines();
    let header: Vec<&str> = lines.next().unwrap().split(',').collect();
    let k = header.iter().position(|h| *h == name).unwrap();
    lines.map(|l| l.split(',').nth(k).unwrap().to_string()).collect()
}

#[test]
#[ignore = "needs PSX_ROOT with bios/, media/ and the Mega Man 8 study state"]
fn samples_reproduce_the_recorded_jump_and_pokes_take_effect() {
    let Some(root) = root() else {
        eprintln!("PSX_ROOT with the Mega Man 8 state not found; skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("psx-observe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let samples = ["--sample", "8015E8AC:fx16.16:y", "--sample", "8015E8B4:fx16.16:vy", "--sample", "8015E89E:u8:state"];

    // Held jump: the recorded trajectory (tick t = sample row t + 1) from the study.
    let mut args = vec!["--frames", "62"];
    args.extend_from_slice(&samples);
    let r = run(&root, &dir.join("jump"), &args);
    assert_eq!(r["status"], "complete", "{r}");
    assert_eq!(r["frames"]["start"], 3900);
    let csv = std::fs::read_to_string(dir.join("jump/samples.csv")).unwrap();
    let recorded = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/megaman8-movement/tests/data/jump_hold.csv")).unwrap();
    let ys = column(&csv, "y");
    let mut ticks = 0;
    for line in recorded.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(',').collect();
        let t: usize = f[0].parse().unwrap();
        let want: f64 = f[2].parse().unwrap();
        let got: f64 = ys[t + 1].parse().unwrap();
        assert!((got - want).abs() < 1e-5, "tick {t}: y {got} vs recorded {want}");
        ticks += 1;
    }
    assert_eq!(ticks, 62);
    assert_eq!(ys.last().unwrap(), "441.15625", "the landing keeps the sub-pixel (a halfword store into the word)");

    // The high-jump flag (+0x8A), poked after the press tick clears it: take-off speed 0x669D8.
    let mut args = vec!["--frames", "3", "--poke", "8015E926=1:1@1"];
    args.extend_from_slice(&samples);
    let r = run(&root, &dir.join("hj"), &args);
    assert_eq!(r["pokes"][0]["applied"], 1, "{r}");
    let csv = std::fs::read_to_string(dir.join("hj/samples.csv")).unwrap();
    assert_eq!(column(&csv, "vy")[2], "6.1634521484375", "6.4134521484375 - 0.25");
    let _ = std::fs::remove_dir_all(&dir);
}

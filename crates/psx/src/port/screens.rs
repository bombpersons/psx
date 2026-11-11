//! How far the port's picture is from the original's: information for
//! `port_status`, never a test. Each work recording is drawn on both sides at
//! a few moments and compared on a coarse grid, so a shift of a pixel or two
//! costs little; the original's tick either side is allowed for, since what
//! it shows may be a tick behind what it simulated. What the port doesn't
//! draw yet always counts, so the number is for watching, not passing.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::commands::Ctx;

const DIR: &str = ".psx/screens";
const HISTORY: &str = ".psx/screens/history.jsonl";
const GRID: (usize, usize) = (64, 48);
/// The moments compared in each recording.
const SAMPLES: u64 = 32;
/// No new recording is started after this long.
const BUDGET: Duration = Duration::from_secs(600);

type Picture = Vec<[f32; 3]>;

/// An image averaged down to the grid.
fn picture(width: usize, height: usize, channels: usize, data: &[u8]) -> Picture {
    let (w, h) = GRID;
    let mut sum = vec![([0f32; 3], 0f32); w * h];
    for (i, px) in data.chunks_exact(channels).enumerate() {
        let (x, y) = (i % width, i / width);
        let cell = &mut sum[(y * h / height) * w + x * w / width];
        for (total, value) in cell.0.iter_mut().zip(px) {
            *total += f32::from(*value);
        }
        cell.1 += 1.0;
    }
    sum.iter().map(|(s, n)| s.map(|v| v / n.max(1.0))).collect()
}

fn load(path: &Path) -> Option<Picture> {
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut reader = png::Decoder::new(file).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let channels = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
        (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
        _ => return None,
    };
    Some(picture(info.width as usize, info.height as usize, channels, &buf[..info.buffer_size()]))
}

/// How far apart two pictures are: 0 the same, 100 black against white.
fn apart(a: &Picture, b: &Picture) -> f64 {
    let total: f32 = a.iter().zip(b).flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs())).sum();
    f64::from(total / (a.len() * 3) as f32 / 255.0 * 100.0)
}

fn rounded(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The moments to compare in a recording ending at tick `last`.
fn samples(last: u64) -> Vec<u64> {
    let mut out: Vec<u64> = (1..=SAMPLES).map(|i| i * last / SAMPLES).filter(|&t| t > 0).collect();
    out.dedup();
    out
}

/// The ticks the original is drawn at for `ticks`.
fn with_neighbours(ticks: &[u64]) -> BTreeSet<u64> {
    ticks.iter().flat_map(|&t| [t.saturating_sub(1), t, t + 1]).filter(|&t| t > 0).collect()
}

/// The port's picture at `tick` against the nearest of the original's.
fn at_tick(original: &Path, port: &Path, tick: u64) -> Option<f64> {
    let ours = load(&port.join(format!("{tick}.png")))?;
    (tick.saturating_sub(1)..=tick + 1)
        .filter_map(|t| load(&original.join(format!("{t}.png"))))
        .map(|theirs| apart(&ours, &theirs))
        .min_by(f64::total_cmp)
}

fn score_one(ctx: &Ctx, recording: &str, key: &str) -> Result<Value, String> {
    let (id, name) = recording.split_once('/').ok_or("not <scenario>/<name>")?;
    let ticks = samples(super::status::last_tick(&ctx.root, recording));
    if ticks.is_empty() {
        return Err("no ticks".into());
    }
    let dir = ctx.root.join(DIR);
    let original = dir.join("original").join(key);
    let port = dir.join("port").join(recording.replace('/', "-"));
    crate::web::sampled_original(ctx, id, name, &with_neighbours(&ticks), &original)?;
    let note = crate::web::sampled_port(&ctx.root, id, name, &ticks.iter().copied().collect(), &port)?;
    let scored: Vec<(u64, f64)> = ticks.iter().filter_map(|&t| at_tick(&original, &port, t).map(|d| (t, d))).collect();
    let Some(&(worst_tick, worst)) = scored.iter().max_by(|a, b| a.1.total_cmp(&b.1)) else {
        return Err(note.unwrap_or_else(|| "no screens to compare".into()));
    };
    let mean = scored.iter().map(|s| s.1).sum::<f64>() / scored.len() as f64;
    let mut v = json!({"recording": recording, "mean": rounded(mean), "worst_tick": worst_tick,
                       "worst": rounded(worst), "moments": scored.len()});
    if let Some(n) = note {
        v["note"] = json!(n);
    }
    Ok(v)
}

/// Score the work recordings, the shortest first, and remember the result.
pub fn score(ctx: &Ctx, work: &[String], commit: Option<&str>, clean: bool) -> crate::Result<Value> {
    let keyed: Vec<(String, Option<String>)> = work
        .iter()
        .map(|r| {
            let key = r.split_once('/').and_then(|(id, name)| crate::web::replay_key(&ctx.root, id, name));
            (r.clone(), key)
        })
        .collect();
    // The original's frames last until their recording or glossary changes.
    let originals = ctx.root.join(DIR).join("original");
    let current: BTreeSet<&str> = keyed.iter().filter_map(|(_, k)| k.as_deref()).collect();
    for e in std::fs::read_dir(&originals).into_iter().flatten().flatten() {
        if !e.file_name().to_str().is_some_and(|n| current.contains(n)) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let mut order = keyed;
    order.sort_by_key(|(r, _)| super::status::last_tick(&ctx.root, r));
    let started = Instant::now();
    let (mut scored, mut not_scored) = (Vec::new(), serde_json::Map::new());
    for (r, key) in &order {
        let result = match key {
            _ if started.elapsed() > BUDGET => Err("out of time: run it again".to_string()),
            None => Err("no data directory for the replays".to_string()),
            Some(k) => score_one(ctx, r, k),
        };
        match result {
            Ok(v) => scored.push(v),
            Err(why) => {
                not_scored.insert(r.clone(), json!(why));
            }
        }
    }
    let mean = (!scored.is_empty())
        .then(|| rounded(scored.iter().filter_map(|v| v["mean"].as_f64()).sum::<f64>() / scored.len() as f64));
    let worst =
        scored.iter().max_by(|a, b| a["worst"].as_f64().unwrap_or(0.0).total_cmp(&b["worst"].as_f64().unwrap_or(0.0)));
    let entry = json!({"commit": commit, "clean": clean, "at": crate::now_ms() as u64, "mean": mean,
                       "worst": worst.map(|v| json!({"recording": v["recording"], "tick": v["worst_tick"], "apart": v["worst"]})),
                       "recordings": scored, "not_scored": not_scored});
    remember(&ctx.root, &entry)?;
    Ok(last(&ctx.root).unwrap_or(entry))
}

fn remember(root: &Path, entry: &Value) -> crate::Result<()> {
    use std::io::Write;
    let path = root.join(HISTORY);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| crate::Error::io(d, e))?;
    }
    let mut f =
        std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| crate::Error::io(&path, e))?;
    writeln!(f, "{entry}").map_err(|e| crate::Error::io(&path, e))
}

/// The last scores, with the average before them as `was`.
pub fn last(root: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(root.join(HISTORY)).ok()?;
    let mut entries = text.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok());
    let mut v = entries.next()?;
    if let Some(was) = entries.find_map(|e| e["mean"].as_f64()) {
        v["was"] = json!(was);
    }
    Some(v)
}

/// `port status --plain`'s line.
pub fn line(v: &Value) -> String {
    let skipped = v["not_scored"].as_object().map_or(0, |o| o.len());
    let Some(mean) = v["mean"].as_f64() else {
        let why = v["not_scored"].as_object().and_then(|o| o.values().next()?.as_str()).unwrap_or("nothing to score");
        return format!("screens  not scored: {why}\n");
    };
    let was = v["was"].as_f64().map_or(String::new(), |w| format!(" (was {w}%)"));
    let commit = v["commit"].as_str().map_or("no commit", |c| &c[..c.len().min(7)]);
    let dirty = if v["clean"] == true { "" } else { " with uncommitted changes" };
    let worst = &v["worst"];
    let rest = if skipped > 0 { format!("; {skipped} not scored") } else { String::new() };
    format!(
        "screens  {mean}% apart on average{was} at {commit}{dirty}, worst {} tick {} at {}%{rest}\n",
        worst["recording"].as_str().unwrap_or(""),
        worst["tick"],
        worst["apart"]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(value: u8) -> Picture {
        picture(64, 48, 3, &vec![value; 64 * 48 * 3])
    }

    #[test]
    fn apart_runs_from_the_same_to_black_against_white() {
        assert_eq!(apart(&flat(90), &flat(90)), 0.0);
        assert_eq!(apart(&flat(0), &flat(255)), 100.0);
        assert!((apart(&flat(0), &flat(51)) - 20.0).abs() < 0.01);
    }

    #[test]
    fn pictures_of_different_sizes_land_on_the_same_grid() {
        let small = picture(320, 240, 4, &[200u8; 320 * 240 * 4]);
        let large = picture(640, 480, 3, &[200u8; 640 * 480 * 3]);
        assert_eq!(apart(&small, &large), 0.0);
    }

    #[test]
    fn samples_spread_to_the_last_tick_without_tick_zero() {
        assert_eq!(samples(0), Vec::<u64>::new());
        assert_eq!(samples(3), vec![1, 2, 3]);
        let long = samples(6740);
        assert_eq!((long.len(), *long.last().unwrap()), (32, 6740));
        assert_eq!(with_neighbours(&[1, 5]).into_iter().collect::<Vec<_>>(), vec![1, 2, 4, 5, 6]);
    }

    #[test]
    fn the_nearest_of_the_originals_ticks_counts_and_a_gap_is_skipped() {
        let d = super::super::tests::tempdir::Dir::new();
        let (o, p) = (d.0.join("o"), d.0.join("p"));
        std::fs::create_dir_all(&o).unwrap();
        std::fs::create_dir_all(&p).unwrap();
        let write = |path: std::path::PathBuf, v: u8| {
            crate::run::write_png(&path, 2, 2, &[v; 16], png::ColorType::Rgba).unwrap();
        };
        write(p.join("5.png"), 255);
        write(o.join("5.png"), 0);
        write(o.join("6.png"), 255);
        assert_eq!(at_tick(&o, &p, 5), Some(0.0));
        assert_eq!(at_tick(&o, &p, 9), None);
    }

    #[test]
    fn the_last_scores_carry_the_average_before_them() {
        let d = super::super::tests::tempdir::Dir::new();
        assert_eq!(last(&d.0), None);
        let entry = |mean: Value| {
            json!({"commit": "abcdef123", "clean": true, "mean": mean, "not_scored": {},
            "worst": {"recording": "a/b", "tick": 12, "apart": 40.5}})
        };
        remember(&d.0, &entry(json!(30.0))).unwrap();
        remember(&d.0, &entry(Value::Null)).unwrap();
        remember(&d.0, &entry(json!(21.5))).unwrap();
        let v = last(&d.0).unwrap();
        assert_eq!(v["was"], 30.0);
        assert_eq!(line(&v), "screens  21.5% apart on average (was 30%) at abcdef1, worst a/b tick 12 at 40.5%\n");
    }
}

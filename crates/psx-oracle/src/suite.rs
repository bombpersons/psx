//! `psx-oracle suite`: the lockstep runs in `suite/suite.json`, each as
//! `lockstep --fast` in a process of its own (Mednafen keeps its state in
//! globals). A run passes when it matches with no I/O value adopted, no
//! frame rerun, memory identical at every frame end compared, and the frame
//! count the file records. A run whose BIOS or disc is missing is skipped.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

pub fn default_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("suite/suite.json")
}

/// What failed in a lockstep report, against the run's expectations.
fn failures(report: &Value, frames: Option<u64>) -> Vec<String> {
    let mut out = Vec::new();
    if report["status"] != "matched" {
        out.push(format!("status {}", report["status"]));
    }
    if let Some(e) = report.get("error") {
        out.push(format!("error {e}"));
    }
    if report["io_values_adopted"].as_u64().unwrap_or(0) != 0 {
        out.push(format!("adopted {}", report["io_values_adopted_by_register"]));
    }
    let reruns = report["fast"]["reruns"].as_array().map_or(0, |r| r.len());
    if reruns != 0 {
        out.push(format!("{reruns} frames rerun"));
    }
    if let Some(memory) = report["memory"].as_object() {
        for (name, d) in memory {
            let n = d["frames_differing"].as_u64().unwrap_or(0);
            if n != 0 {
                out.push(format!("{name} differs at {n} frames, first {}", d["first"]));
            }
        }
    }
    if let (Some(want), Some(got)) = (frames, report["frames"].as_u64()) {
        if want != got {
            out.push(format!("{got} frames, not {want}"));
        }
    }
    out
}

fn one(root: &Path, dir: &Path, run: &Value) -> Value {
    let name = run["name"].as_str().unwrap_or("?").to_string();
    let path = |key: &str, base: &Path| run[key].as_str().map(|p| base.join(p));
    let (bios, disc, replay) = (path("bios", root), path("disc", root), path("replay", dir));
    let missing: Vec<_> = [&bios, &disc].into_iter().flatten().filter(|p| !p.exists()).collect();
    if bios.is_none() || !missing.is_empty() {
        return json!({"name": name, "result": "skipped", "missing": missing});
    }
    let steps = run["steps"].as_u64().unwrap_or(1_000_000);
    let exe = std::env::current_exe().unwrap_or_else(|_| "psx-oracle".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--root").arg(root).arg("lockstep").arg("--fast").arg("--bios").arg(bios.unwrap());
    cmd.arg("--steps").arg(steps.to_string());
    if let Some(d) = disc {
        cmd.arg("--disc").arg(d);
    }
    if let Some(r) = replay {
        cmd.arg("--replay").arg(r);
    }
    let started = Instant::now();
    let report = match cmd.output() {
        Ok(o) => serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| json!({"error": String::from_utf8_lossy(&o.stderr).trim().to_string()})),
        Err(e) => json!({"error": e.to_string()}),
    };
    let fails = failures(&report, run["frames"].as_u64());
    json!({
        "name": name,
        "result": if fails.is_empty() { "passed" } else { "failed" },
        "failures": fails,
        "steps": report["steps_compared"],
        "frames": report["frames"],
        "seconds": started.elapsed().as_secs(),
        "steps_per_second": report["steps_per_second"],
        "divergence": report["divergence"],
    })
}

pub fn run(root: &Path, file: &Path, only: &[String], jobs: usize) -> (bool, Value) {
    let suite: Value = match std::fs::read(file)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return (false, json!({"error": format!("{}: {e}", file.display())})),
    };
    let dir = file.parent().unwrap_or(Path::new("."));
    let runs: Vec<(usize, Value)> = suite["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| only.is_empty() || only.iter().any(|n| r["name"] == n.as_str()))
        .cloned()
        .enumerate()
        .collect();
    let started = Instant::now();
    let queue = Mutex::new(runs.into_iter().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                while let Some((i, run)) = queue.lock().unwrap().pop_front() {
                    let r = one(root, dir, &run);
                    eprintln!("{} {}", r["name"].as_str().unwrap_or("?"), r["result"].as_str().unwrap_or("?"));
                    results.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    let results: Vec<Value> = results.into_iter().map(|(_, r)| r).collect();
    let count = |what: &str| results.iter().filter(|r| r["result"] == what).count();
    let (passed, failed, skipped) = (count("passed"), count("failed"), count("skipped"));
    (
        failed == 0 && passed > 0,
        json!({
            "status": if failed > 0 { "failed" } else if passed == 0 { "nothing_ran" } else { "passed" },
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "seconds": started.elapsed().as_secs(),
            "runs": results,
        }),
    )
}

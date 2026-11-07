//! The conformance test of "any executable as a port": the C example
//! (examples/c-port) checked by `psx port check` through its psx-port.toml,
//! and its arithmetic run against `psx port vectors`. Needs `cc` and
//! `make`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_psx");

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/c-port")
}

/// The example's tracked files copied into a fresh git repository.
fn workspace() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("psx-c-port-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src = example();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        for e in std::fs::read_dir(src.join(&rel)).unwrap().flatten() {
            let rel = rel.join(e.file_name());
            if ["game/build", "target", ".psx"].iter().any(|skip| rel == Path::new(skip)) {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(dir.join(&rel)).unwrap();
                todo.push(rel);
            } else {
                std::fs::copy(e.path(), dir.join(&rel)).unwrap();
            }
        }
    }
    git(&dir, &["init", "-q"]);
    commit(&dir, "the C example");
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=test@example.com", "-c", "user.name=test"])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", message]);
}

fn psx(dir: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN).args(args).arg("--root").arg(dir).env_remove("PSX_ROOT").output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    });
    assert!(out.status.success(), "psx {args:?}: {v}");
    v
}

#[test]
fn a_c_port_is_checked_through_its_manifest() {
    let dir = workspace();

    // Its arithmetic matches the vectors.
    let vectors = dir.join(".psx/vectors");
    psx(&dir, &["port", "vectors", "--out", vectors.to_str().unwrap()]);
    let out = Command::new("make")
        .args(["-s", "-C", "game", "test"])
        .arg(format!("VECTORS={}", vectors.display()))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains(", 0 failed"));

    // It passes its recording.
    let v = psx(&dir, &["port", "check"]);
    assert_eq!(v["verdicts"][0]["passed"], true, "{v}");
    assert_eq!(v["verdicts"][0]["work"]["hill/push"], serde_json::json!([]), "{v}");

    // A port one unit out is caught, by the quantities that differ.
    let sled = dir.join("game/src/sled.c");
    let text = std::fs::read_to_string(&sled).unwrap();
    std::fs::write(&sled, text.replace("#define SNOW 0xF00", "#define SNOW 0xEFF")).unwrap();
    commit(&dir, "the snow one unit out");
    let v = psx(&dir, &["port", "check"]);
    assert_eq!(v["verdicts"][0]["passed"], false, "{v}");
    assert_eq!(v["verdicts"][0]["work"]["hill/push"][0], "differs from the recording in sled_speed, sled_x", "{v}");

    // A port that does not build says why.
    std::fs::write(&sled, "not C\n").unwrap();
    commit(&dir, "no game");
    let v = psx(&dir, &["port", "check"]);
    let why = v["verdicts"][0]["failures"][0].as_str().unwrap_or_default();
    assert!(why.starts_with("the port did not build"), "{v}");

    let _ = std::fs::remove_dir_all(&dir);
}

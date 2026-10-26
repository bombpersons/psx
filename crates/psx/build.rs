use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn collect(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            if !matches!(p.file_name().and_then(|s| s.to_str()), Some("target" | ".git")) {
                collect(&p, files);
            }
        }
    } else {
        files.push(path.to_owned());
    }
}
fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().unwrap().parent().unwrap();
    let mut files = Vec::new();
    for item in ["Cargo.toml", "Cargo.lock", "crates", "tools/ghidra", "templates/psx-bevy"] {
        let path = root.join(item);
        println!("cargo:rerun-if-changed={}", path.display());
        collect(&path, &mut files);
    }
    files.sort();
    let mut h = Sha256::new();
    for p in files {
        // Same name on every platform, so a source tree hashes alike on Windows.
        let name = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(&p).unwrap();
        h.update((name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    let source = format!("{:x}", h.finalize());
    // The emulator alone: what decides how a run behaves. Evidence recorded
    // on another emulator is recorded again (psx protocol next).
    let mut files = Vec::new();
    for item in
        ["Cargo.lock", "crates/psx-core", "crates/psx-gpu", "crates/psx-gte", "crates/psx-media", "crates/psx-assets"]
            .into_iter()
            .chain(["crates/psx/src/run_native.rs", "crates/psx/src/run_native"])
    {
        collect(&root.join(item), &mut files);
    }
    files.sort();
    let mut h = Sha256::new();
    for p in files {
        let name = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(&p).unwrap();
        h.update((name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    println!("cargo:rustc-env=PSX_EMULATOR_ID=emu-{}", &format!("{:x}", h.finalize())[..16]);
    let profile = std::env::var("PROFILE").unwrap();
    println!("cargo:rustc-env=PSX_SOURCE_SHA256={source}");
    println!("cargo:rustc-env=PSX_BUILD_ID=src-{}-{profile}", &source[..16]);
    println!("cargo:rustc-env=PSX_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=PSX_BUILD_TARGET={}", std::env::var("TARGET").unwrap());
}

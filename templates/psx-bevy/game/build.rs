fn main() {
    // `Observed`, the glossary's quantities as the port has them, for the
    // scenarios (psx_bevy::scenario): generated from glossary/src/lib.rs.
    psx_scenario::rust::write_observed(concat!(env!("CARGO_MANIFEST_DIR"), "/../glossary"));
    shared_library_paths();
}

/// With Bevy as a shared library (feature `dynamic`) the game's binaries
/// load it and Rust's standard library at start. `cargo run` puts both on
/// the loader path; these rpaths let a binary start on its own too (an MCP
/// client running `target/debug/<game> --mcp`): Bevy's from beside the
/// binary, the standard library from this toolchain. Release builds link
/// Bevy in and need neither.
fn shared_library_paths() {
    if std::env::var_os("CARGO_FEATURE_DYNAMIC").is_none() {
        return;
    }
    let origin = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly") => "$ORIGIN",
        Ok("macos" | "ios") => "@executable_path",
        // Windows looks beside the executable and on PATH only.
        _ => return,
    };
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let target = std::env::var("TARGET").unwrap_or_default();
    let std_dir = std::process::Command::new(rustc)
        .args(["--print", "target-libdir", "--target", &target])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    // A binary sits in target/<profile>/ (and deps/), an example in examples/,
    // a test in deps/; Bevy's library is in deps/.
    let mut dirs = vec![format!("{origin}/deps"), format!("{origin}/../deps"), origin.to_string()];
    dirs.extend(std_dir);
    for dir in dirs {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}

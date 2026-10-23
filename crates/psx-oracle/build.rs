//! Compiles the vendored Mednafen sources and the headless frontend
//! (`harness/driver.cpp`) with the flags Mednafen's own build uses.
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=mednafen");
    println!("cargo::rerun-if-changed=harness");
    println!("cargo::rustc-check-cfg=cfg(mednafen)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // include/config.h was made by configure on x86_64 Linux; elsewhere the
    // binary only says it isn't available.
    if os != "linux" || arch != "x86_64" {
        return;
    }
    println!("cargo::rustc-cfg=mednafen");

    let root = Path::new("mednafen");
    let src = root.join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    files.sort();
    let group = |f: &Path| {
        let rel = f.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        rel.split('/').next().filter(|_| rel.contains('/')).unwrap_or("").to_string()
    };
    let is = |f: &Path, ext: &str| f.extension().is_some_and(|e| e == ext);

    // The core and the frontend in one library: each calls the other.
    let mut core = build(root, true);
    core.file("harness/driver.cpp");
    for f in files.iter().filter(|f| is(f, "cpp")) {
        core.file(f);
    }
    core.compile("mednafen");

    // Mednafen's C sources, with the extra flags its makefiles give each library.
    let libraries: [(&str, &[&str]); 6] = [
        ("tremor", &["-fvisibility=hidden"]),
        ("zstd", &["-DZSTD_NO_TRACE=1", "-DZSTD_HAVE_WEAK_SYMBOLS=0", "-fvisibility=hidden"]),
        ("mpcdec", &["-DMPC_FIXED_POINT=1", "-fvisibility=hidden"]),
        ("trio", &["-fvisibility=hidden"]),
        ("quicklz", &["-fno-strict-aliasing"]),
        ("", &[]),
    ];
    for (name, flags) in libraries {
        let mut c = build(root, false);
        if name == "tremor" {
            c.include(src.join("tremor"));
        }
        for flag in flags {
            c.flag(flag);
        }
        let mine: Vec<&PathBuf> = files
            .iter()
            .filter(|f| is(f, "c"))
            .filter(|f| {
                let g = group(f);
                if name.is_empty() { !libraries.iter().any(|(n, _)| !n.is_empty() && g == *n) } else { g == name }
            })
            .collect();
        for f in mine {
            c.file(f);
        }
        c.compile(&format!("mednafen_{}", if name.is_empty() { "c" } else { name }));
    }
    println!("cargo::rustc-link-lib=z");
    println!("cargo::rustc-link-lib=pthread");
}

fn build(root: &Path, cpp: bool) -> cc::Build {
    let mut b = cc::Build::new();
    b.cpp(cpp)
        .include(root.join("include"))
        .define("HAVE_CONFIG_H", None)
        // Mednafen is built optimised even in debug builds: the oracle's speed
        // matters, its debug info doesn't.
        .opt_level(2)
        .debug(false)
        .warnings(false)
        .cargo_warnings(false)
        // Mednafen's warnings (and its note about position-independent code) aren't ours to fix.
        .flag("-w");
    if cpp {
        b.std("gnu++11").flag("-fexceptions");
    } else {
        b.std("gnu99");
    }
    // From Mednafen's configure: signed chars and wrapping signed overflow are
    // assumed, and floating point must be exact.
    for flag in [
        "-fsigned-char",
        "-fwrapv",
        "-fno-fast-math",
        "-fno-unsafe-math-optimizations",
        "-fno-aggressive-loop-optimizations",
        "-fno-ipa-icf",
        "-fno-printf-return-value",
        "-fomit-frame-pointer",
        "-fstrict-aliasing",
        "-fjump-tables",
    ] {
        b.flag(flag);
    }
    b
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "c" || e == "cpp") {
            out.push(path);
        }
    }
}

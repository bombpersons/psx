//! Small player settings files (controls.json, display.json) in the player's
//! config folder: read with a size limit, written atomically (temporary file,
//! then rename) so an interrupted save never leaves half a file.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Larger files are refused rather than parsed.
pub const MAX_BYTES: usize = 64 * 1024;

/// `<config dir>/<app>/<file>`: `%APPDATA%` on Windows, otherwise
/// `$XDG_CONFIG_HOME` or `~/.config`. `app` is usually the game's package name.
pub fn config_path(app: &str, file: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    if let Some(p) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(p).join(app).join(file));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
    Some(base.join(app).join(file))
}

/// The file's bytes, or `None` when it does not exist.
pub fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("{} exceeds 64 KiB", path.file_name().unwrap_or_default().to_string_lossy()));
    }
    Ok(Some(bytes))
}

/// Replaces the file with `bytes`, creating its folder if needed.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let temp = parent.join(format!(".{stem}-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| e.to_string())?;
    struct Staged(PathBuf);
    impl Drop for Staged {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _staged = Staged(temp.clone());
    let written = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    written.map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| e.to_string())
}

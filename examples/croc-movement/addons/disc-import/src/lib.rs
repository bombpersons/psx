//! The game's start: find the player's disc, run the importers on it
//! (`tools/import`) once, and show how far they are.
//!
//! The disc is found from `--disc PATH`, the disc used last time, a disc
//! image (`.cue`, `.chd`, `.iso`, `.bin`) next to the game or in the folder
//! it runs from, or one dropped on the window. An import is redone when
//! [`import::VERSION`] changes. While it runs the game draws [`DiscImport::draw`];
//! once [`DiscImport::ready`] it loads from [`Options::out`]. `--disc` with
//! another disc than last time imports again.
use import::{DiscReader, ImportProgress};
use renderer::{Font, Scene, TextureId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Where to look and where to write.
#[derive(Clone, Debug)]
pub struct Options {
    /// The game's folder name for player data.
    pub name: String,
    /// A disc given on the command line (`--disc PATH`).
    pub disc: Option<PathBuf>,
    /// Folders searched for a disc image.
    pub search: Vec<PathBuf>,
    /// Where the imported assets go.
    pub out: PathBuf,
}

impl Options {
    /// `--disc PATH` from the arguments, the game's folder and the working
    /// folder as places to look, and `out`.
    pub fn new(name: &str, args: &[String], out: PathBuf) -> Self {
        let disc = args.iter().position(|a| a == "--disc").and_then(|i| args.get(i + 1)).map(PathBuf::from);
        let mut search = Vec::new();
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            search.push(dir);
        }
        if let Ok(dir) = std::env::current_dir() {
            search.push(dir);
        }
        Self { name: name.to_string(), disc, search, out }
    }
}

/// Where a release build keeps imported assets: `<data dir>/<name>/imported`
/// (`%LOCALAPPDATA%` on Windows, otherwise `$XDG_DATA_HOME` or
/// `~/.local/share`). Development builds use `game/assets/imported`.
pub fn data_dir(name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return Some(PathBuf::from(p).join(name).join("imported"));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))?;
    Some(base.join(name).join("imported"))
}

/// `out/import.json`: which importer version wrote the assets, from which disc.
#[derive(Serialize, Deserialize)]
struct Stamp {
    version: u32,
    disc: PathBuf,
}

const STAMP: &str = "import.json";

/// Where the start is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// No disc yet. `rejected` says why the last one offered was not used.
    NeedDisc {
        rejected: Option<String>,
    },
    Importing {
        fraction: f32,
        step: String,
    },
    Failed(String),
    /// The assets are in [`Options::out`].
    Ready,
}

pub struct DiscImport {
    options: Options,
    state: State,
    progress: ImportProgress,
    job: Option<JoinHandle<Result<(), String>>>,
    last_scan: Option<Instant>,
    /// Images already tried, so the scan does not open them again.
    tried: BTreeSet<PathBuf>,
}

impl DiscImport {
    /// Ready when the assets are imported by this importer version;
    /// otherwise imports from the first disc found, or waits for one.
    pub fn new(options: Options) -> Self {
        let mut this = Self {
            options,
            state: State::NeedDisc { rejected: None },
            progress: ImportProgress::default(),
            job: None,
            last_scan: None,
            tried: BTreeSet::new(),
        };
        let stamp =
            std::fs::read(this.options.out.join(STAMP)).ok().and_then(|b| serde_json::from_slice::<Stamp>(&b).ok());
        let same_disc = |stamp: &Stamp| {
            this.options.disc.as_ref().is_none_or(|d| std::path::absolute(d).is_ok_and(|d| d == stamp.disc))
        };
        if let Some(stamp) = &stamp
            && stamp.version == import::VERSION
            && same_disc(stamp)
        {
            this.state = State::Ready;
            return this;
        }
        let candidates = this.options.disc.iter().cloned().chain(stamp.map(|s| s.disc));
        for disc in candidates.collect::<Vec<_>>() {
            if this.offer(&disc) {
                return this;
            }
        }
        this.scan();
        this
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn ready(&self) -> bool {
        self.state == State::Ready
    }

    pub fn out(&self) -> &Path {
        &self.options.out
    }

    /// Follows the import, and looks for a disc once a second while there
    /// is none. Call once a tick.
    pub fn update(&mut self) {
        if let Some(job) = &self.job {
            if !job.is_finished() {
                let (fraction, step) = self.progress.get();
                self.state = State::Importing { fraction, step };
                return;
            }
            let job = self.job.take().expect("job");
            self.state = match job.join() {
                Ok(Ok(())) => State::Ready,
                Ok(Err(e)) => State::Failed(e),
                Err(_) => State::Failed("the importer panicked".into()),
            };
            return;
        }
        if matches!(self.state, State::NeedDisc { .. })
            && self.last_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.scan();
        }
    }

    fn scan(&mut self) {
        self.last_scan = Some(Instant::now());
        for dir in self.options.search.clone() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let mut images: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
                    matches!(ext.as_deref(), Some("cue" | "chd" | "iso" | "bin"))
                })
                .filter(|p| !self.tried.contains(p))
                .collect();
            // A .cue before the .bin it names.
            images.sort_by_key(|p| (p.extension().is_none_or(|e| !e.eq_ignore_ascii_case("cue")), p.clone()));
            for image in images {
                if self.offer(&image) {
                    return;
                }
            }
        }
    }

    /// Tries `path` as the disc (a dropped file, say): imports from it if it
    /// is one the importers accept. False when it is not.
    pub fn offer(&mut self, path: &Path) -> bool {
        if self.job.is_some() {
            return false;
        }
        self.tried.insert(path.to_path_buf());
        let reader = match DiscReader::open(path) {
            Ok(r) => r,
            Err(e) => {
                self.state = State::NeedDisc { rejected: Some(e) };
                return false;
            }
        };
        if !reader.accepted() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            self.state =
                State::NeedDisc { rejected: Some(format!("{name} is {}, not {}", reader.serial, import::GAME)) };
            return false;
        }
        let out = self.options.out.clone();
        let progress = self.progress.clone();
        progress.set(0.0, "Starting");
        self.state = State::Importing { fraction: 0.0, step: "Starting".into() };
        self.job = Some(std::thread::spawn(move || run(&reader, &out, &progress)));
        true
    }

    /// The start screen for the current state, on the layout `size`.
    pub fn draw(&self, scene: &mut Scene, font: &Font, white: TextureId, size: [u32; 2]) {
        let [w, h] = [size[0] as f32, size[1] as f32];
        let scale = 1.0;
        let line = font.height(scale) + 3.0;
        let width = w - 24.0;
        let mut lines: Vec<(String, [f32; 4])> = Vec::new();
        let grey = [0.7, 0.7, 0.7, 1.0];
        let mut bar = None;
        match &self.state {
            State::NeedDisc { rejected } => {
                lines.extend(
                    wrap(
                        font,
                        &format!("Put your disc of {} next to the game, or drop it on this window.", import::GAME),
                        width,
                    )
                    .into_iter()
                    .map(|l| (l, [1.0; 4])),
                );
                if let Some(r) = rejected {
                    lines.push((String::new(), grey));
                    lines.extend(wrap(font, r, width).into_iter().map(|l| (l, [1.0, 0.6, 0.5, 1.0])));
                }
            }
            State::Importing { fraction, step } => {
                lines.extend(
                    wrap(font, &format!("Importing {} from your disc", import::GAME), width)
                        .into_iter()
                        .map(|l| (l, [1.0; 4])),
                );
                lines.push((String::new(), grey));
                lines.push((String::new(), grey));
                lines.extend(wrap(font, step, width).into_iter().map(|l| (l, grey)));
                bar = Some((*fraction, lines.len() - 2));
            }
            State::Failed(e) => {
                lines.push(("The import failed:".into(), [1.0, 0.6, 0.5, 1.0]));
                lines.extend(wrap(font, e, width).into_iter().map(|l| (l, grey)));
                lines.push((String::new(), grey));
                lines.extend(
                    wrap(font, "Drop a disc on this window to try again.", width).into_iter().map(|l| (l, [1.0; 4])),
                );
            }
            State::Ready => return,
        }
        let top = ((h - lines.len() as f32 * line) / 2.0).round();
        for (i, (text, color)) in lines.iter().enumerate() {
            let x = ((w - font.width(text, scale)) / 2.0).round();
            font.text(scene, text, [x, top + i as f32 * line], scale, *color);
        }
        if let Some((fraction, row)) = bar {
            let (x, y, bw) = (24.0, top + row as f32 * line, w - 48.0);
            scene.rect(white, [x, y, bw, line - 3.0], [0.25, 0.25, 0.3, 1.0]);
            scene.rect(white, [x, y, (bw * fraction).round(), line - 3.0], [0.9, 0.9, 0.9, 1.0]);
        }
    }
}

/// Imports into a fresh folder beside `out`, then puts it in `out`'s place,
/// so a failed or interrupted import leaves the last good one.
fn run(reader: &DiscReader, out: &Path, progress: &ImportProgress) -> Result<(), String> {
    let name = out.file_name().ok_or("the import folder has no name")?.to_string_lossy().to_string();
    let parent = out.parent().ok_or("the import folder has no parent")?;
    let fresh = parent.join(format!(".{name}.importing"));
    let old = parent.join(format!(".{name}.old"));
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    for dir in [&fresh, &old] {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| io(dir, e))?;
        }
    }
    std::fs::create_dir_all(&fresh).map_err(|e| io(&fresh, e))?;
    import::import(reader, &fresh, progress)?;
    let stamp =
        Stamp { version: import::VERSION, disc: std::path::absolute(&reader.path).unwrap_or(reader.path.clone()) };
    let json = serde_json::to_vec_pretty(&stamp).map_err(|e| e.to_string())?;
    std::fs::write(fresh.join(STAMP), json).map_err(|e| io(&fresh, e))?;
    if out.exists() {
        std::fs::rename(out, &old).map_err(|e| io(out, e))?;
    }
    std::fs::rename(&fresh, out).map_err(|e| io(out, e))?;
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(|e| io(&old, e))?;
    }
    progress.set(1.0, "Done");
    Ok(())
}

/// `text` in lines no wider than `width`, broken at spaces.
fn wrap(font: &Font, text: &str, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if font.width(&candidate, 1.0) > width && !line.is_empty() {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(d: &mut DiscImport) {
        let start = Instant::now();
        while matches!(d.state(), State::Importing { .. }) && start.elapsed() < Duration::from_secs(30) {
            d.update();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn imports_a_disc_found_beside_the_game_once() {
        let dir = std::env::temp_dir().join(format!("disc-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A disc of a serial the importers accept, if they name any.
        let serial = import::SERIALS.first().copied().unwrap_or("SLUS_123.45");
        let cnf = format!("BOOT = cdrom:\\{serial};1\r\n");
        let files: [(&str, &[u8]); 2] = [("SYSTEM.CNF", cnf.as_bytes()), (serial, &[0; 2048])];
        std::fs::write(dir.join("game.iso"), import::test_disc::iso("GAME", &files)).unwrap();
        let options = Options { name: "test".into(), disc: None, search: vec![dir.clone()], out: dir.join("imported") };
        let mut d = DiscImport::new(options.clone());
        wait(&mut d);
        assert_eq!(d.state(), &State::Ready);
        assert!(dir.join("imported").join(STAMP).exists());
        // Next start: ready at once, even with the disc gone.
        std::fs::remove_file(dir.join("game.iso")).unwrap();
        assert!(DiscImport::new(options).ready());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn waits_for_a_disc() {
        let dir = std::env::temp_dir().join(format!("disc-import-none-{}", std::process::id()));
        let options = Options { name: "test".into(), disc: None, search: vec![dir.clone()], out: dir.join("imported") };
        let mut d = DiscImport::new(options);
        assert_eq!(d.state(), &State::NeedDisc { rejected: None });
        assert!(!d.offer(Path::new("/nonexistent/disc.cue")));
        assert!(matches!(d.state(), State::NeedDisc { rejected: Some(_) }));
    }
}

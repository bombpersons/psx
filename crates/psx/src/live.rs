//! The screen of a game a logged call is running, for `psx web` to show on
//! the call's card while it runs: `.psx/calls/<id>.live.png`, replaced a few
//! times a second and removed when the call ends.
//!
//! The call that runs a game sets where its screen goes ([`publish`]) on the
//! thread that runs it; the loops that run the game call [`field`] at each
//! field. A project worker (MCP) is given the path in `PSX_LIVE`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use psx_core::Machine;

/// Variable naming a worker's live screen.
pub const ENV: &str = "PSX_LIVE";
/// How often the screen is replaced, at most.
const EVERY: Duration = Duration::from_millis(125);

struct Target {
    path: PathBuf,
    last: Option<Instant>,
}

thread_local! {
    static TARGET: RefCell<Option<Target>> = const { RefCell::new(None) };
}

/// Where this thread's call shows its screen, until the guard drops (which
/// removes the screen).
pub fn publish(path: PathBuf) -> Guard {
    TARGET.with(|t| *t.borrow_mut() = Some(Target { path, last: None }));
    Guard(())
}

/// The path this thread's call shows its screen at, for a worker it starts.
pub fn current() -> Option<PathBuf> {
    TARGET.with(|t| t.borrow().as_ref().map(|t| t.path.clone()))
}

pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(t) = TARGET.with(|t| t.borrow_mut().take()) {
            let _ = std::fs::remove_file(&t.path);
        }
    }
}

/// A field of the running game: its screen, when the last is old enough.
pub fn field(m: &Machine) {
    TARGET.with(|t| {
        let mut t = t.borrow_mut();
        let Some(t) = t.as_mut() else { return };
        if t.last.is_some_and(|l| l.elapsed() < EVERY) {
            return;
        }
        t.last = Some(Instant::now());
        let (w, h, rgba) = m.bus.gpu.display_rgba();
        if w == 0 || h == 0 {
            return;
        }
        write(&t.path, w, h, &rgba);
    });
}

/// Written beside and renamed, so a reader never sees half a file.
fn write(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let tmp = path.with_extension("tmp.png");
    // Fast compression: a screen is replaced eight times a second.
    if write_fast_png(&tmp, w, h, rgba).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn write_fast_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fastest);
    enc.write_header()?.write_image_data(rgba).map_err(std::io::Error::other)
}

//! Hand-editable data: RON files under `assets/`, built into the game so
//! the scenarios, `--replay` and a release build all see the same values.
//!
//! Design numbers inside a controller (speeds, damage, timers) live in a
//! per-type tuning file, so they can be changed without touching code; the
//! controller reads them from [`crate::Tunings`]. Structural details
//! (update order, rounding) stay in code.
use serde::de::DeserializeOwned;

/// The files under `assets/`, by their path there.
const FILES: &[(&str, &str)] = &[
    ("character/body.ron", include_str!("../assets/character/body.ron")),
    ("character/vertical.ron", include_str!("../assets/character/vertical.ron")),
    ("croc/running.ron", include_str!("../assets/croc/running.ron")),
    ("croc/jump.ron", include_str!("../assets/croc/jump.ron")),
    ("croc/stomp.ron", include_str!("../assets/croc/stomp.ron")),
    ("levels/start.ron", include_str!("../assets/levels/start.ron")),
];

/// Parses `assets/<relative>` as RON.
pub fn load_ron<T: DeserializeOwned>(relative: &str) -> Result<T, String> {
    let (_, text) = FILES
        .iter()
        .find(|(path, _)| *path == relative)
        .ok_or_else(|| format!("assets/{relative}: not one of config::FILES"))?;
    ron::from_str(text).map_err(|e| format!("assets/{relative}: {e}"))
}

/// A tuning file's own checks: values someone could write that the rules
/// cannot run with.
pub trait Tuning {
    /// Why the values cannot be used, if they cannot.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Reads tuning `T` from `assets/<path>`. Panics with the file and the
/// error if it is invalid or fails [`Tuning::validate`].
pub fn load_tuning<T: DeserializeOwned + Tuning>(path: &str) -> T {
    let value: T = load_ron(path).unwrap_or_else(|e| panic!("tuning {e}"));
    value.validate().unwrap_or_else(|e| panic!("tuning assets/{path}: {e}"));
    value
}

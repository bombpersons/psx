//! Hand-editable data: RON files under the running crate's `assets/` (or a
//! layered asset folder, see [`crate::assets`]).
//!
//! Design numbers inside a controller (speeds, damage, timers) live in a
//! per-type tuning file, so they can be modded without code changes; the
//! controller reads them from a resource. Structural details (update order,
//! rounding) stay in code.
//!
//! Files load synchronously when the app is built, so headless tests see the
//! same values as the game. Move to Bevy asset loaders when content needs
//! streaming or hot reload.
use bevy::prelude::*;
use serde::de::DeserializeOwned;
use std::marker::PhantomData;
use std::path::PathBuf;

/// `assets/<relative>` from the same folders Bevy's asset server reads: the
/// running crate's `assets/` (under `BEVY_ASSET_ROOT`, else the crate being
/// run, else the executable's folder), then any folders added with
/// [`crate::assets::add_root`] (a mod's base game).
pub fn asset_path(relative: &str) -> PathBuf {
    crate::assets::find(relative)
}

/// Reads and parses `assets/<relative>` as RON.
pub fn load_ron<T: DeserializeOwned>(relative: &str) -> Result<T, String> {
    let path = asset_path(relative);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    ron::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Inserts resource `T` from `assets/<path>` (RON) when added. Panics with
/// the file and parse error if it is missing or invalid: a game without its
/// tuning is not worth running.
pub struct TuningPlugin<T> {
    pub path: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T> TuningPlugin<T> {
    pub const fn new(path: &'static str) -> Self {
        Self { path, marker: PhantomData }
    }
}

impl<T: Resource + DeserializeOwned> Plugin for TuningPlugin<T> {
    fn build(&self, app: &mut App) {
        let value: T = load_ron(self.path).unwrap_or_else(|e| panic!("tuning {}: {e}", self.path));
        app.insert_resource(value);
    }
}

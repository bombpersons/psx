//! Asset folders, layered. The running crate's `assets/` comes first, then
//! any folders added with [`add_root`], so a crate built on another one (a
//! mod built on `game`) finds that crate's assets and can replace any of
//! them by shipping a file at the same path.
//!
//! ```no_run
//! // A mod's main.rs, before calling the launcher:
//! psx_bevy::assets::add_root("../game/assets");
//! ```
//!
//! Relative roots resolve against the same base Bevy uses for `assets/`
//! (`BEVY_ASSET_ROOT`, else the running crate, else the executable's
//! folder). The launcher installs the layers as Bevy's default asset source
//! ([`register`]); synchronous loads such as `engine::config::load_ron` use
//! [`find`]. With no added roots nothing changes.
use bevy::asset::io::file::FileAssetReader;
use bevy::asset::io::{AssetReader, AssetReaderError, AssetSourceBuilder, AssetSourceId, PathStream, Reader};
use bevy::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static EXTRA_ROOTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Adds an asset folder below the running crate's own `assets/`. Call it
/// before building the app (before the launcher, or before
/// `engine::testing` apps in tests). Later calls rank lower.
pub fn add_root(path: impl Into<PathBuf>) {
    let path = FileAssetReader::get_base_path().join(path.into());
    let mut roots = EXTRA_ROOTS.lock().unwrap();
    if !roots.contains(&path) {
        roots.push(path);
    }
}

/// Every asset folder, highest priority first: the running crate's
/// `assets/`, then the added roots.
pub fn roots() -> Vec<PathBuf> {
    let mut roots = vec![FileAssetReader::get_base_path().join("assets")];
    roots.extend(EXTRA_ROOTS.lock().unwrap().iter().cloned());
    roots
}

/// The file `relative` resolves to: the first root that has it, else the
/// path under the running crate's `assets/` (so errors name a real place).
pub fn find(relative: impl AsRef<Path>) -> PathBuf {
    let roots = roots();
    roots
        .iter()
        .map(|root| root.join(relative.as_ref()))
        .find(|path| path.exists())
        .unwrap_or_else(|| roots[0].join(relative.as_ref()))
}

/// Makes the layered folders Bevy's default asset source. Must run before
/// `AssetPlugin` (the launcher calls it before `DefaultPlugins`). Does
/// nothing when no roots were added, keeping Bevy's own reader.
pub fn register(app: &mut App) {
    if EXTRA_ROOTS.lock().unwrap().is_empty() {
        return;
    }
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(|| Box::new(LayeredReader(roots().into_iter().map(FileAssetReader::new).collect()))),
    );
}

/// Reads from the first folder that has the path.
struct LayeredReader(Vec<FileAssetReader>);

impl AssetReader for LayeredReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let mut last = AssetReaderError::NotFound(path.to_owned());
        for layer in &self.0 {
            match layer.read(path).await {
                Err(AssetReaderError::NotFound(missing)) => last = AssetReaderError::NotFound(missing),
                result => return result,
            }
        }
        Err(last)
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let mut last = AssetReaderError::NotFound(path.to_owned());
        for layer in &self.0 {
            match layer.read_meta(path).await {
                Err(AssetReaderError::NotFound(missing)) => last = AssetReaderError::NotFound(missing),
                result => return result,
            }
        }
        Err(last)
    }

    async fn read_directory<'a>(&'a self, path: &'a Path) -> Result<Box<PathStream>, AssetReaderError> {
        for layer in &self.0 {
            if layer.is_directory(path).await.unwrap_or(false) {
                return layer.read_directory(path).await;
            }
        }
        Err(AssetReaderError::NotFound(path.to_owned()))
    }

    async fn is_directory<'a>(&'a self, path: &'a Path) -> Result<bool, AssetReaderError> {
        for layer in &self.0 {
            if layer.is_directory(path).await.unwrap_or(false) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_roots_win_and_later_ones_fill_in() {
        let dir = std::env::temp_dir().join(format!("psx-assets-{}", std::process::id()));
        let (top, base) = (dir.join("top"), dir.join("base"));
        for (root, files) in [(&top, &["shared.txt"][..]), (&base, &["shared.txt", "only_base.txt"][..])] {
            std::fs::create_dir_all(root).unwrap();
            for file in files {
                std::fs::write(root.join(file), root.to_string_lossy().as_bytes()).unwrap();
            }
        }
        let reader = LayeredReader(vec![FileAssetReader::new(&top), FileAssetReader::new(&base)]);
        let read = |path: &str| {
            bevy::tasks::block_on(async {
                let mut bytes = Vec::new();
                let mut file = reader.read(Path::new(path)).await?;
                file.read_to_end(&mut bytes).await?;
                Ok::<_, AssetReaderError>(String::from_utf8(bytes).unwrap())
            })
        };
        assert_eq!(read("shared.txt").unwrap(), top.to_string_lossy());
        assert_eq!(read("only_base.txt").unwrap(), base.to_string_lossy());
        assert!(matches!(read("missing.txt"), Err(AssetReaderError::NotFound(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

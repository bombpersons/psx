//! `psx template create`: write the standalone Bevy game starter.

use std::path::Path;

use psx_media::MediaError;
use serde_json::{json, Value};

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../templates/psx-bevy/", $path)))),*]
    };
}

const FILES: &[(&str, &str)] = files![
    ".gitignore",
    "AGENTS.md",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "rustfmt.toml",
    "examples/depth_cue.rs",
    "src/controller.rs",
    "src/controller/bindings.rs",
    "src/controls/mod.rs",
    "src/controls/view.rs",
    "src/controls/tests.rs",
    "src/demo/mod.rs",
    "src/demo/player.rs",
    "src/dev_mcp.rs",
    "src/lib.rs",
    "src/main.rs",
    "src/model_viewer.rs",
    "src/psx/assets.rs",
    "src/psx/formats/mod.rs",
    "src/psx/formats/tim.rs",
    "src/psx/formats/tmd.rs",
    "src/psx/material.rs",
    "src/psx/material2d.rs",
    "src/psx/mod.rs",
    "src/psx/scene.rs",
    "src/psx/shaders.rs",
    "src/psx/shaders/psx_material.wesl",
    "src/psx/shaders/psx_material2d.wesl",
    "src/psx/shaders/psx_screen.wesl",
    "src/psx/shaders/psx_sprite.wesl",
    "src/shell.rs",
    "src/testing.rs",
    "src/testing/trajectory.rs",
    "src/viewer.rs",
    "tests/player.rs",
    "tests/dev_mcp.rs",
];

const TEMPLATE_PACKAGE: &str = "psx-game";
/// The lines of the template's `src/lib.rs` that `create` rewrites.
const REGION_LINE: &str = "pub const REGION: Region = Region::Ntsc;";
const FIELDS_PER_TICK_LINE: &str = "pub const FIELDS_PER_TICK: u32 = 1;";

/// The original game's video region, written as `REGION` in the generated `src/lib.rs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TemplateRegion {
    /// 60 Hz display.
    #[default]
    Ntsc,
    /// 50 Hz display.
    Pal,
}

impl TemplateRegion {
    fn name(self) -> &'static str {
        match self {
            TemplateRegion::Ntsc => "ntsc",
            TemplateRegion::Pal => "pal",
        }
    }

    fn rust(self) -> &'static str {
        match self {
            TemplateRegion::Ntsc => "Region::Ntsc",
            TemplateRegion::Pal => "Region::Pal",
        }
    }

    fn hz(self) -> f64 {
        match self {
            TemplateRegion::Ntsc => 60.0,
            TemplateRegion::Pal => 50.0,
        }
    }
}

/// Settings written into the generated project.
#[derive(Clone, Copy, Debug)]
pub struct TemplateOptions {
    pub region: TemplateRegion,
    /// Video fields (vblanks) per logic tick, 1 to 60.
    pub fields_per_tick: u32,
}

impl Default for TemplateOptions {
    fn default() -> Self {
        Self { region: TemplateRegion::Ntsc, fields_per_tick: 1 }
    }
}

/// The template's `src/lib.rs` with `REGION` and `FIELDS_PER_TICK` set.
fn configure_lib(contents: &str, options: TemplateOptions) -> psx_media::Result<String> {
    if !contents.contains(REGION_LINE) || !contents.contains(FIELDS_PER_TICK_LINE) {
        return Err(MediaError::Malformed(
            "template src/lib.rs no longer has the REGION / FIELDS_PER_TICK lines template create rewrites".into(),
        ));
    }
    Ok(contents
        .replacen(REGION_LINE, &format!("pub const REGION: Region = {};", options.region.rust()), 1)
        .replacen(
            FIELDS_PER_TICK_LINE,
            &format!("pub const FIELDS_PER_TICK: u32 = {};", options.fields_per_tick),
            1,
        ))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn create(dest: &Path, name: Option<&str>, options: TemplateOptions) -> psx_media::Result<Value> {
    let name = match name {
        Some(n) => n.to_string(),
        None => dest
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase().replace(' ', "-"))
            .unwrap_or_else(|| TEMPLATE_PACKAGE.to_string()),
    };
    if !valid_name(&name) {
        return Err(MediaError::Malformed(format!(
            "package name {name:?} must start with a-z and contain only a-z, 0-9, '-' or '_'; pass --name"
        )));
    }
    if !(1..=60).contains(&options.fields_per_tick) {
        return Err(MediaError::Malformed(format!(
            "fields_per_tick {} must be between 1 and 60 (vblanks the original waits per game frame)",
            options.fields_per_tick
        )));
    }
    if dest.exists() && std::fs::read_dir(dest).map_err(|e| MediaError::io(dest, e))?.next().is_some() {
        return Err(MediaError::Malformed(format!("{} exists and is not empty", dest.display())));
    }
    let mut written = Vec::new();
    for (rel, contents) in FILES {
        let contents = match *rel {
            "Cargo.toml" => contents.replacen(
                &format!("name = \"{TEMPLATE_PACKAGE}\""),
                &format!("name = \"{name}\""),
                1,
            ),
            "Cargo.lock" => contents.replace(
                &format!("name = \"{TEMPLATE_PACKAGE}\""),
                &format!("name = \"{name}\""),
            ),
            "src/lib.rs" => configure_lib(contents, options)?,
            _ => contents.to_string(),
        };
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MediaError::io(parent, e))?;
        }
        std::fs::write(&path, contents).map_err(|e| MediaError::io(&path, e))?;
        written.push(*rel);
    }
    std::fs::create_dir_all(dest.join("assets")).map_err(|e| MediaError::io(dest, e))?;
    Ok(json!({
        "template": "psx-bevy",
        "package": name,
        "dest": dest,
        "region": options.region.name(),
        "fields_per_tick": options.fields_per_tick,
        "tick_hz": options.region.hz() / f64::from(options.fields_per_tick),
        "files": written,
        "next": ["cargo run", "cargo run --features dev-mcp -- --mcp", "cargo run --features dev-mcp -- --mcp --headless --software"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_renamed_project_and_refuses_non_empty_dest() {
        let dir = std::env::temp_dir().join(format!("psx-template-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = create(&dir.join("my-game"), None, TemplateOptions::default()).unwrap();
        assert_eq!(out["package"], "my-game");
        assert_eq!(out["region"], "ntsc");
        let toml = std::fs::read_to_string(dir.join("my-game/Cargo.toml")).unwrap();
        assert!(toml.contains("name = \"my-game\""));
        assert!(dir.join("my-game/src/psx/shaders/psx_material.wesl").exists());
        assert!(dir.join("my-game/src/controller/bindings.rs").exists());
        assert!(dir.join("my-game/src/controls/view.rs").exists());
        let lib = std::fs::read_to_string(dir.join("my-game/src/lib.rs")).unwrap();
        assert!(lib.contains(REGION_LINE) && lib.contains(FIELDS_PER_TICK_LINE));
        assert!(create(&dir.join("my-game"), None, TemplateOptions::default()).is_err());
        assert!(create(&dir.join("Bad Name!"), None, TemplateOptions::default()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_region_and_fields_per_tick() {
        let dir = std::env::temp_dir().join(format!("psx-template-region-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let options = TemplateOptions { region: TemplateRegion::Pal, fields_per_tick: 2 };
        let out = create(&dir.join("croc"), None, options).unwrap();
        assert_eq!(out["region"], "pal");
        assert_eq!(out["tick_hz"], 25.0);
        let lib = std::fs::read_to_string(dir.join("croc/src/lib.rs")).unwrap();
        assert!(lib.contains("pub const REGION: Region = Region::Pal;"));
        assert!(lib.contains("pub const FIELDS_PER_TICK: u32 = 2;"));
        assert!(!lib.contains(REGION_LINE) && !lib.contains(FIELDS_PER_TICK_LINE));
        let bad = TemplateOptions { fields_per_tick: 0, ..options };
        assert!(create(&dir.join("zero"), None, bad).is_err());
        assert!(!dir.join("zero").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every file under templates/psx-bevy (except build output and local
    /// assets) must be embedded, or `template create` silently drops it.
    #[test]
    fn embeds_every_template_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/psx-bevy");
        let mut found = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                if rel == "target" || rel == "assets" {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(rel);
                }
            }
        }
        let embedded: Vec<&str> = FILES.iter().map(|(p, _)| *p).collect();
        let missing: Vec<&String> = found.iter().filter(|f| !embedded.contains(&f.as_str())).collect();
        assert!(missing.is_empty(), "template files not in FILES: {missing:?}");
    }
}

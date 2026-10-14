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
    "src/controller.rs",
    "src/demo.rs",
    "src/dev_mcp.rs",
    "src/main.rs",
    "src/psx/material.rs",
    "src/psx/mod.rs",
    "src/psx/shaders/psx_material.wesl",
];

const TEMPLATE_PACKAGE: &str = "psx-game";

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn create(dest: &Path, name: Option<&str>) -> psx_media::Result<Value> {
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
        "files": written,
        "next": ["cargo run", "cargo run --features dev-mcp -- --mcp"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_renamed_project_and_refuses_non_empty_dest() {
        let dir = std::env::temp_dir().join(format!("psx-template-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = create(&dir.join("my-game"), None).unwrap();
        assert_eq!(out["package"], "my-game");
        let toml = std::fs::read_to_string(dir.join("my-game/Cargo.toml")).unwrap();
        assert!(toml.contains("name = \"my-game\""));
        assert!(dir.join("my-game/src/psx/shaders/psx_material.wesl").exists());
        assert!(create(&dir.join("my-game"), None).is_err());
        assert!(create(&dir.join("Bad Name!"), None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

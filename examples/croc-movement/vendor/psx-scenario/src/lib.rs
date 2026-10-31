//! The formats for porting by recordings: the glossary written as Rust
//! (read by [`rust`]), scenario files and their recordings, with their checks
//! and the JSON Schemas of the files that are not Rust; and the glue the
//! glossary's binding and the port generate from the glossary.
//!
//! This is the platform-free side: what a binding reads on the PlayStation
//! is psx's. The formats are defined in psx's `docs/scenarios.md`.

pub mod release;
pub mod rust;
pub mod scenario;
pub mod value;

pub use release::Release;
pub use scenario::{Glossary, Recording, Scenario};
pub use value::{Field, Type, Value, Values};

/// The JSON Schemas of the platform-free files, by file name.
pub fn schemas() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("scenario.schema.json", schema::<Scenario>("scenarios/<id>.toml")),
        ("recording-header.schema.json", schema::<scenario::Header>("the first line of a scenarios/<id>/<name>.jsonl")),
        ("recording-row.schema.json", schema::<scenario::Row>("each later line of a scenarios/<id>/<name>.jsonl")),
    ]
}

/// The schema of `T`, titled with the file it describes.
pub fn schema<T: schemars::JsonSchema>(file: &str) -> serde_json::Value {
    let mut s = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    s["title"] = file.into();
    s
}

/// Compares `schemas` with the files in `dir`; with `PSX_UPDATE_SCHEMAS`
/// set, writes them instead. Returns the names that differ.
pub fn sync_schemas(dir: &std::path::Path, schemas: &[(&str, serde_json::Value)]) -> Vec<String> {
    let update = std::env::var_os("PSX_UPDATE_SCHEMAS").is_some();
    let mut stale = Vec::new();
    for (name, s) in schemas {
        let text = serde_json::to_string_pretty(s).unwrap() + "\n";
        let path = dir.join(name);
        if update {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(&path, text).unwrap();
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(&text) {
            stale.push(name.to_string());
        }
    }
    stale
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_schemas_are_current() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
        let stale = super::sync_schemas(&dir, &super::schemas());
        assert!(stale.is_empty(), "stale schemas {stale:?}: run with PSX_UPDATE_SCHEMAS=1");
    }
}

//! Working stdlib-only Python external parser starters, with authored inputs.
use psx_media::{MediaError, Result};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Copy, Debug, clap::ValueEnum, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Model,
    Archive,
    Asset,
    Video,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Archive => "archive",
            Self::Asset => "asset",
            Self::Video => "video",
        }
    }
    fn script(self) -> &'static str {
        match self {
            Self::Model => include_str!("../../../templates/parsers/model.py"),
            Self::Archive => include_str!("../../../templates/parsers/archive.py"),
            Self::Asset => include_str!("../../../templates/parsers/asset.py"),
            Self::Video => include_str!("../../../templates/parsers/video.py"),
        }
    }
    fn sample(self) -> Vec<u8> {
        match self {
            Self::Model => {
                let mut data = b"TRI1\x03\x00\x01\x00".to_vec();
                for n in [0i16, 0, 0, 100, 0, 0, 0, 100, 0, 0, 1, 2] {
                    data.extend_from_slice(&n.to_le_bytes());
                }
                data
            }
            Self::Archive => {
                let payload = Self::Model.sample();
                let mut data = b"ARC1\x02\x00\x00\x00".to_vec();
                for n in [24u32, 0, 24, payload.len() as u32] {
                    data.extend_from_slice(&n.to_le_bytes());
                }
                data.extend(payload);
                data
            }
            Self::Asset => {
                // A 2x2 16-bit direct-colour TIM, first pixel red.
                let mut tim = Vec::new();
                for n in [16u32, 2, 20] {
                    tim.extend_from_slice(&n.to_le_bytes());
                }
                tim.extend_from_slice(&[0, 0, 0, 0, 2, 0, 2, 0, 31, 0, 0, 0, 0, 0, 0, 0]);
                let mut data = b"WRAP".to_vec();
                data.extend_from_slice(&(tim.len() as u32).to_le_bytes());
                data.extend(tim);
                data
            }
            Self::Video => {
                // One neutral-grey 16x16 macroblock, BS v2: six DC=0/EOB
                // blocks then EOF, MSB first in little-endian halfwords.
                let bits = "000000000010".repeat(6) + "0111111111";
                let mut data = b"VBS1\x06\x00\x00\x38\x01\x00\x02\x00".to_vec();
                for chunk in bits.as_bytes().chunks(16) {
                    let word = chunk.iter().enumerate().fold(0u16, |w, (i, b)| w | (u16::from(*b == b'1') << (15 - i)));
                    data.extend_from_slice(&word.to_le_bytes());
                }
                data.extend_from_slice(&[0; 8]);
                data
            }
        }
    }
}

pub fn create(dest: &Path, kind: Kind) -> Result<Value> {
    if dest.exists()
        && (!dest.is_dir() || std::fs::read_dir(dest).map_err(|e| MediaError::io(dest, e))?.next().is_some())
    {
        return Err(MediaError::Malformed(format!("{} must be a new or empty directory", dest.display())));
    }
    std::fs::create_dir_all(dest).map_err(|e| MediaError::io(dest, e))?;
    let demo = match kind {
        Kind::Model => "psx assets extract sample.bin --model-parser ./parser.py -o out",
        Kind::Archive => {
            "psx assets extract sample.bin --archive-parser ./parser.py --entry 1 -o out\n# Entry 1 is TRI1; chain it with a generated model parser using --model-parser."
        }
        Kind::Asset => "psx assets extract sample.bin --asset-parser ./parser.py -o out",
        Kind::Video => {
            "# sample.bin is a wrapped 16x16 frame, not an STR disc image.\n# For a disc using this wrapper:\npsx str disc.cue::/MOVIE.STR --frame-parser ./parser.py -o out"
        }
    };
    let readme = format!(
        "# {} parser starter\n\n`parser.py` uses only Python 3's standard library. Replace `decode()` with your format logic. `sample.bin` is authored test data, with no game assets.\n\nTry the contract directly (from this directory):\n\n```sh\n./parser.py < sample.bin > result.json\n{}\n```\n\nSee [PROTOCOL.md](PROTOCOL.md) for the versioned contracts, context variables, limits and model conventions. Diagnostics go to stderr; stdout must be one JSON document. Executables in any language can implement the same contract.\n",
        kind.name(),
        demo
    );
    for (name, bytes) in [
        ("parser.py", kind.script().as_bytes().to_vec()),
        ("sample.bin", kind.sample()),
        ("README.md", readme.into_bytes()),
        ("PROTOCOL.md", include_bytes!("../../../docs/external-parsers.md").to_vec()),
    ] {
        let path = dest.join(name);
        std::fs::write(&path, bytes).map_err(|e| MediaError::io(&path, e))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dest.join("parser.py");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| MediaError::io(&path, e))?;
    }
    Ok(
        json!({"dest":dest,"kind":kind.name(),"parser":dest.join("parser.py"),"sample":dest.join("sample.bin"),"files":["parser.py","sample.bin","README.md","PROTOCOL.md"]}),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::parser::{self, Spec};

    #[test]
    fn every_generated_sample_obeys_its_contract_and_preserves_existing_files() {
        let root = std::env::temp_dir().join(format!("psx-parser-starters-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for kind in [Kind::Model, Kind::Archive, Kind::Asset, Kind::Video] {
            let dir = root.join(kind.name());
            create(&dir, kind).unwrap();
            assert!(create(&dir, kind).is_err());
            let spec = Spec { program: dir.join("parser.py"), args: vec![], timeout: 3 };
            let output = parser::run(&spec, &kind.sample(), kind.name(), &[], None).unwrap();
            match kind {
                Kind::Model => {
                    let model = parser::document::<psx_assets::model_document::ModelDocument>(&output.bytes)
                        .unwrap()
                        .into_model()
                        .unwrap();
                    assert_eq!(model.polygon_count(), 1);
                }
                Kind::Archive => {
                    let index = parser::document::<parser::ArchiveDocument>(&output.bytes)
                        .unwrap()
                        .validate(kind.sample().len())
                        .unwrap();
                    assert_eq!(index.entries.len(), 2);
                    assert_eq!(index.entries[0].size, 0);
                    assert_eq!(index.entries[1].bytes(&kind.sample()).unwrap(), Kind::Model.sample());
                }
                Kind::Asset => {
                    let payload = parser::document::<parser::PayloadDocument>(&output.bytes).unwrap().decode().unwrap();
                    let hits = psx_assets::scan::scan(&payload, Default::default());
                    assert_eq!(hits.len(), 1);
                    assert_eq!(hits[0].kind, psx_assets::scan::Kind::Tim);
                }
                Kind::Video => {
                    let payload = parser::document::<parser::PayloadDocument>(&output.bytes).unwrap().decode().unwrap();
                    let (_, frame) = psx_media::str_video::decode_bs(&payload, 16, 16).unwrap();
                    assert!(frame.end_code);
                    assert_eq!(frame.macroblocks.len(), 1);
                }
            }
            assert!(parser::run(&spec, b"bad", kind.name(), &[], None).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

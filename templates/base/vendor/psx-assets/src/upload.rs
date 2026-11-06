//! Explicit raw VRAM uploads. Pixel depth and palette roles belong to the
//! consuming texture page, so these rectangles contain uninterpreted u16 words.
use crate::tim::{VRAM_HEIGHT, VRAM_WIDTH, Vram, VramRect};
use base64::Engine;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UploadDocument {
    pub version: u32,
    pub uploads: Vec<UploadRecord>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRecord {
    /// Source bytes responsible for this upload, relative to parser stdin.
    pub source_offset: usize,
    pub source_size: usize,
    /// x, y, width in words, height (VRAM word coordinates).
    pub rect: [u16; 4],
    /// Row-major little-endian u16 words, exactly width*height*2 bytes.
    pub data_base64: String,
}

pub struct Upload {
    pub source_offset: usize,
    pub source_size: usize,
    pub rect: VramRect,
    pub words: Vec<u16>,
}

impl UploadDocument {
    pub fn decode(self, source_len: usize) -> Result<Vec<Upload>, String> {
        if self.version != 1 {
            return Err(format!("unsupported upload document version {}", self.version));
        }
        if self.uploads.is_empty() || self.uploads.len() > 65536 {
            return Err("upload document must contain 1..=65536 uploads".into());
        }
        self.uploads
            .into_iter()
            .enumerate()
            .map(|(index, record)| {
                let [x, y, w, h] = record.rect;
                if w == 0
                    || h == 0
                    || usize::from(x) + usize::from(w) > VRAM_WIDTH
                    || usize::from(y) + usize::from(h) > VRAM_HEIGHT
                {
                    return Err(format!("upload {index}: rectangle outside VRAM or empty"));
                }
                if record.source_size == 0
                    || record.source_offset.checked_add(record.source_size).is_none_or(|end| end > source_len)
                {
                    return Err(format!("upload {index}: source span out of bounds or empty"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&record.data_base64)
                    .map_err(|e| format!("upload {index}: invalid base64: {e}"))?;
                if bytes.len() != usize::from(w) * usize::from(h) * 2 {
                    return Err(format!("upload {index}: data size does not match rectangle"));
                }
                Ok(Upload {
                    source_offset: record.source_offset,
                    source_size: record.source_size,
                    rect: VramRect { x, y, w, h },
                    words: bytes.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect(),
                })
            })
            .collect()
    }
}

impl Upload {
    pub fn apply(&self, vram: &mut Vram) {
        vram.upload(self.rect, &self.words);
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tim::{TexPage, TimAlpha};
    use serde_json::json;

    #[test]
    fn validates_spans_rectangles_bytes_and_version_before_uploading() {
        let good = json!({"version":1,"uploads":[{"source_offset":0,"source_size":2,"rect":[1023,511,1,1],"data_base64":"H4A="}]});
        let decoded = serde_json::from_value::<UploadDocument>(good.clone()).unwrap().decode(2).unwrap();
        let mut vram = Vram::default();
        decoded[0].apply(&mut vram);
        assert_eq!(vram.data[512 * 1024 - 1], 0x801f);
        assert!(vram.written[512 * 1024 - 1]);
        for (field, value) in [
            ("source_offset", json!(2)),
            ("source_size", json!(0)),
            ("rect", json!([1023, 511, 2, 1])),
            ("rect", json!([0, 512, 1, 1])),
            ("rect", json!([0, 0, 0, 1])),
            ("data_base64", json!("!!!!")),
            ("data_base64", json!("Hw==")),
        ] {
            let mut invalid = good.clone();
            invalid["uploads"][0][field] = value;
            assert!(serde_json::from_value::<UploadDocument>(invalid).unwrap().decode(2).is_err());
        }
        let mut invalid = good.clone();
        invalid["version"] = json!(2);
        assert!(serde_json::from_value::<UploadDocument>(invalid).unwrap().decode(2).is_err());
        let mut invalid = good;
        invalid["uploads"][0]["bpp"] = json!(4);
        assert!(serde_json::from_value::<UploadDocument>(invalid).is_err());
    }

    #[test]
    fn order_overwrites_and_indexed_palette_lookup_preserve_raw_words() {
        let mut vram = Vram::default();
        let mut palette = vec![0; 16];
        palette[1] = 0x1f;
        for upload in [
            Upload { source_offset: 0, source_size: 1, rect: VramRect { x: 0, y: 500, w: 16, h: 1 }, words: palette },
            Upload {
                source_offset: 0,
                source_size: 1,
                rect: VramRect { x: 640, y: 256, w: 1, h: 1 },
                words: vec![0x1111],
            },
            Upload {
                source_offset: 0,
                source_size: 1,
                rect: VramRect { x: 1, y: 500, w: 1, h: 1 },
                words: vec![0x83e0],
            },
        ] {
            upload.apply(&mut vram);
        }
        let (rgba, covered) =
            vram.decode_page(TexPage { x: 640, y: 256, bits: 4, abr: 0 }, (0, 500), TimAlpha::SemiStp);
        assert_eq!(&rgba[..16], &[0, 255, 0, 128].repeat(4));
        assert_eq!(covered, 4.0 / (256.0 * 256.0));
        assert_eq!(vram.data[256 * 1024 + 640], 0x1111);
        assert!(!vram.written[256 * 1024 + 641]);
    }
}

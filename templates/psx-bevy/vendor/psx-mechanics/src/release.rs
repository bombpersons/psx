//! The release a binding reads: the game's image, as a binding describes
//! it to its producer (a mechanic's or the glossary's).

use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    /// The producer's platform: `psx`.
    pub platform: String,
    /// The release, in words: `Croc (Europe) SLES-00593`.
    pub title: String,
    /// The image's identity as the producer reports it (for psx, the media
    /// identity of `psx context`).
    pub image_sha256: String,
}

impl Release {
    pub fn check(&self, platform: &str, p: &mut Vec<String>) {
        if self.platform != platform {
            p.push(format!("source.platform {:?}: this producer is {platform:?}", self.platform));
        }
        if self.image_sha256.len() != 64 || !self.image_sha256.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
            p.push("source.image_sha256: 64 lowercase hex digits".into());
        }
    }
}

//! `IMAGE::/PATH` sources: a file inside a disc image, or a host file.

use std::path::PathBuf;

use crate::iso9660::FileSystem;
use crate::{Disc, MediaError, Result};

/// Split `image.cue::/PATH`, or take the ISO path as a separate argument.
pub fn split(source: &str, path: Option<&str>) -> Result<(PathBuf, String)> {
    match (source.split_once("::"), path) {
        (Some((img, p)), None) => Ok((PathBuf::from(img), p.to_string())),
        (None, Some(p)) => Ok((PathBuf::from(source), p.to_string())),
        (Some(_), Some(_)) => Err(MediaError::Malformed("give the path either as IMAGE::/PATH or as a second argument".into())),
        (None, None) => Err(MediaError::Malformed("missing ISO path: use IMAGE::/PATH or IMAGE /PATH".into())),
    }
}

/// The bytes of `image.cue::/PATH` (as Form 1 data), or of a host file.
pub fn read(source: &str) -> Result<Vec<u8>> {
    match source.split_once("::") {
        Some((image, path)) => {
            let disc = Disc::open(image)?;
            let fs = FileSystem::read(&disc)?;
            fs.read_file(&disc, fs.entry(path)?)
        }
        None => std::fs::read(source).map_err(|e| MediaError::io(source, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        assert_eq!(split("a.cue::/X.STR", None).unwrap(), (PathBuf::from("a.cue"), "/X.STR".into()));
        assert_eq!(split("a.cue", Some("/X.STR")).unwrap(), (PathBuf::from("a.cue"), "/X.STR".into()));
        assert!(split("a.cue", None).is_err());
        assert!(split("a.cue::/X", Some("/Y")).is_err());
    }
}

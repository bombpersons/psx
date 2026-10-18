//! CUE sheet parsing. Only the commands that affect disc geometry are kept.

use crate::error::{malformed, MediaError, Result};

pub const FRAMES_PER_SECOND: u32 = 75;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackMode {
    /// `MODE1/2352`: raw sectors, 2048 bytes user data at offset 16.
    Mode1Raw,
    /// `MODE1/2048`: cooked user data only.
    Mode1Cooked,
    /// `MODE2/2352`: raw CD-XA sectors (the normal PS1 data track).
    Mode2Raw,
    /// `AUDIO`: 2352 bytes of 44.1 kHz 16-bit stereo PCM per sector.
    Audio,
}

impl TrackMode {
    fn parse(text: &str) -> Result<Self> {
        match text.to_ascii_uppercase().as_str() {
            "MODE1/2352" => Ok(TrackMode::Mode1Raw),
            "MODE1/2048" => Ok(TrackMode::Mode1Cooked),
            "MODE2/2352" => Ok(TrackMode::Mode2Raw),
            "AUDIO" => Ok(TrackMode::Audio),
            other => Err(MediaError::Unsupported(format!("CUE track mode {other}"))),
        }
    }

    /// The CUE sheet spelling of this mode.
    pub fn cue_name(self) -> &'static str {
        match self {
            TrackMode::Mode1Raw => "MODE1/2352",
            TrackMode::Mode1Cooked => "MODE1/2048",
            TrackMode::Mode2Raw => "MODE2/2352",
            TrackMode::Audio => "AUDIO",
        }
    }

    pub fn sector_size(self) -> u64 {
        match self {
            TrackMode::Mode1Cooked => 2048,
            _ => 2352,
        }
    }

    pub fn is_data(self) -> bool {
        self != TrackMode::Audio
    }
}

#[derive(Clone, Debug)]
pub struct CueTrack {
    pub number: u8,
    pub mode: TrackMode,
    /// `(index number, sector offset from the start of the FILE)`.
    pub indices: Vec<(u8, u32)>,
    /// `PREGAP` sectors that are *not* stored in the file.
    pub pregap: u32,
}

impl CueTrack {
    pub fn index(&self, n: u8) -> Option<u32> {
        self.indices.iter().find(|(i, _)| *i == n).map(|(_, s)| *s)
    }

    /// First sector of this track that is stored in the file.
    pub fn first_stored(&self) -> u32 {
        self.indices.iter().map(|(_, s)| *s).min().unwrap_or(0)
    }
}

#[derive(Clone, Debug)]
pub struct CueFile {
    pub name: String,
    pub tracks: Vec<CueTrack>,
}

#[derive(Clone, Debug)]
pub struct CueSheet {
    pub files: Vec<CueFile>,
}

/// Parse `mm:ss:ff` into a sector count.
pub fn parse_msf(text: &str) -> Option<u32> {
    let mut parts = text.split(':');
    let m: u32 = parts.next()?.parse().ok()?;
    let s: u32 = parts.next()?.parse().ok()?;
    let f: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || s >= 60 || f >= FRAMES_PER_SECOND {
        return None;
    }
    Some((m * 60 + s) * FRAMES_PER_SECOND + f)
}

/// Split a CUE line into words, keeping double-quoted strings together.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            out.push(chars.by_ref().take_while(|&c| c != '"').collect());
        } else {
            let mut w = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                w.push(c);
                chars.next();
            }
            out.push(w);
        }
    }
    out
}

impl CueSheet {
    pub fn parse(text: &str) -> Result<Self> {
        let mut files: Vec<CueFile> = Vec::new();
        for (lineno, line) in text.lines().enumerate() {
            let w = words(line);
            let Some(cmd) = w.first() else { continue };
            let at = |m: &str| format!("CUE line {}: {m}", lineno + 1);
            match cmd.to_ascii_uppercase().as_str() {
                "FILE" => {
                    let (Some(name), Some(kind)) = (w.get(1), w.get(2)) else {
                        return malformed(at("FILE needs a name and type"));
                    };
                    if !kind.eq_ignore_ascii_case("BINARY") {
                        return Err(MediaError::Unsupported(at(&format!("FILE type {kind}"))));
                    }
                    files.push(CueFile { name: name.clone(), tracks: Vec::new() });
                }
                "TRACK" => {
                    let Some(file) = files.last_mut() else {
                        return malformed(at("TRACK before FILE"));
                    };
                    let number = w.get(1).and_then(|n| n.parse().ok());
                    let (Some(number), Some(mode)) = (number, w.get(2)) else {
                        return malformed(at("TRACK needs a number and mode"));
                    };
                    file.tracks.push(CueTrack {
                        number,
                        mode: TrackMode::parse(mode)?,
                        indices: Vec::new(),
                        pregap: 0,
                    });
                }
                "INDEX" | "PREGAP" => {
                    let Some(track) = files.last_mut().and_then(|f| f.tracks.last_mut()) else {
                        return malformed(at(&format!("{cmd} before TRACK")));
                    };
                    let time_word = if cmd.eq_ignore_ascii_case("INDEX") { 2 } else { 1 };
                    let Some(sectors) = w.get(time_word).and_then(|t| parse_msf(t)) else {
                        return malformed(at("bad mm:ss:ff time"));
                    };
                    if cmd.eq_ignore_ascii_case("INDEX") {
                        let Some(n) = w.get(1).and_then(|n| n.parse().ok()) else {
                            return malformed(at("bad INDEX number"));
                        };
                        track.indices.push((n, sectors));
                    } else {
                        track.pregap = sectors;
                    }
                }
                "POSTGAP" => {
                    return Err(MediaError::Unsupported(at("POSTGAP")));
                }
                // REM, CATALOG, TITLE, PERFORMER, FLAGS, ISRC, SONGWRITER: no geometry.
                _ => {}
            }
        }
        if files.iter().all(|f| f.tracks.is_empty()) {
            return malformed("CUE sheet has no tracks");
        }
        for f in &files {
            for t in &f.tracks {
                if t.index(1).is_none() {
                    return malformed(format!("track {} has no INDEX 01", t.number));
                }
            }
        }
        Ok(CueSheet { files })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multitrack_single_file() {
        let cue = CueSheet::parse(
            "FILE \"Game Name.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 27:47:11\n    INDEX 01 27:49:11\n",
        )
        .unwrap();
        assert_eq!(cue.files.len(), 1);
        assert_eq!(cue.files[0].name, "Game Name.bin");
        let t2 = &cue.files[0].tracks[1];
        assert_eq!(t2.mode, TrackMode::Audio);
        assert_eq!(t2.first_stored(), parse_msf("27:47:11").unwrap());
        assert_eq!(t2.index(1), Some(parse_msf("27:49:11").unwrap()));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(CueSheet::parse("TRACK 01 AUDIO"), Err(MediaError::Malformed(_))));
        assert!(matches!(
            CueSheet::parse("FILE \"a\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00"),
            Err(MediaError::Unsupported(_))
        ));
        assert!(parse_msf("00:60:00").is_none());
    }
}

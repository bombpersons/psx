//! ISO9660 (with CD-XA extensions) file system reading.
//!
//! PS1 games frequently bypass the file system: they read raw LBAs found in
//! their own tables, or pack many assets into one large file. The listing here
//! is therefore a map of *named* extents, and [`FileSystem::owner_of`] lets a
//! runtime sector read be attributed to a named file when one covers it.

use std::collections::HashSet;

use crate::disc::{Disc, SectorForm};
use crate::error::{malformed, MediaError, Result};

pub const SECTOR: u32 = 2048;

/// CD-XA attribute bits (big-endian u16 in the directory record system-use area).
pub mod xa {
    pub const MODE2_FORM1: u16 = 0x0800;
    pub const MODE2_FORM2: u16 = 0x1000;
    pub const INTERLEAVED: u16 = 0x2000;
    pub const CDDA: u16 = 0x4000;
    pub const DIRECTORY: u16 = 0x8000;
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Volume {
    pub system_id: String,
    pub volume_id: String,
    pub volume_sectors: u32,
    pub publisher: String,
    pub application: String,
    pub created: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Entry {
    /// Absolute path with `/` separators and no `;version` suffix.
    pub path: String,
    pub lba: u32,
    /// Size recorded in the directory. For Form 2 / interleaved files this is
    /// conventionally `sectors * 2048`, not the real payload length.
    pub size: u32,
    pub is_dir: bool,
    pub version: Option<u16>,
    pub xa_attributes: Option<u16>,
    pub xa_file_number: Option<u8>,
    /// Recording date `YYYY-MM-DD hh:mm:ss`.
    pub recorded: String,
}

impl Entry {
    pub fn sectors(&self) -> u32 {
        self.size.div_ceil(SECTOR)
    }

    pub fn contains_lba(&self, lba: u32) -> bool {
        lba >= self.lba && lba < self.lba + self.sectors().max(1)
    }

    /// True when the directory declares Form 2 or interleaved (XA/STR) content.
    pub fn is_streamed(&self) -> bool {
        self.xa_attributes.is_some_and(|a| a & (xa::MODE2_FORM2 | xa::INTERLEAVED) != 0)
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FileSystem {
    pub volume: Volume,
    pub entries: Vec<Entry>,
    /// Problems that did not stop the walk (loops, out-of-range extents).
    pub warnings: Vec<String>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim_end_matches([' ', '\0']).to_string()
}

struct RawRecord {
    name: Vec<u8>,
    lba: u32,
    size: u32,
    flags: u8,
    recorded: String,
    xa: Option<(u16, u8)>,
}

fn parse_record(r: &[u8]) -> Result<RawRecord> {
    if r.len() < 34 {
        return malformed("directory record shorter than 34 bytes");
    }
    let name_len = r[32] as usize;
    if 33 + name_len > r.len() {
        return malformed("directory record name overruns record");
    }
    let name = r[33..33 + name_len].to_vec();
    let su_start = 33 + name_len + (name_len + 1) % 2;
    let su = r.get(su_start..).unwrap_or(&[]);
    let xa = (su.len() >= 14 && &su[6..8] == b"XA")
        .then(|| (u16::from_be_bytes([su[4], su[5]]), su[8]));
    let d = &r[18..25];
    let recorded = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        1900 + d[0] as u32,
        d[1],
        d[2],
        d[3],
        d[4],
        d[5]
    );
    Ok(RawRecord { name, lba: u32le(r, 2), size: u32le(r, 10), flags: r[25], recorded, xa })
}

impl FileSystem {
    pub fn read(disc: &Disc) -> Result<Self> {
        let pvd = disc.read_form1(16, 1)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return Err(MediaError::Unsupported("no ISO9660 primary volume descriptor at LBA 16".into()));
        }
        if u16le(&pvd, 128) != SECTOR as u16 {
            return Err(MediaError::Unsupported(format!("logical block size {}", u16le(&pvd, 128))));
        }
        let volume = Volume {
            system_id: text(&pvd[8..40]),
            volume_id: text(&pvd[40..72]),
            volume_sectors: u32le(&pvd, 80),
            publisher: text(&pvd[318..446]),
            application: text(&pvd[574..702]),
            created: text(&pvd[813..829]),
        };
        let root = parse_record(&pvd[156..190])?;
        let mut fs = FileSystem { volume, entries: Vec::new(), warnings: Vec::new() };
        let mut seen = HashSet::new();
        fs.walk(disc, "", root.lba, root.size, 0, &mut seen)?;
        fs.entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(fs)
    }

    fn walk(
        &mut self,
        disc: &Disc,
        prefix: &str,
        lba: u32,
        size: u32,
        depth: u32,
        seen: &mut HashSet<u32>,
    ) -> Result<()> {
        if depth > 32 || !seen.insert(lba) {
            self.warnings.push(format!("skipped directory at LBA {lba} (loop or too deep)"));
            return Ok(());
        }
        let sectors = size.div_ceil(SECTOR);
        let bytes = match disc.read_form1(lba, sectors) {
            Ok(b) => b,
            Err(e) => {
                self.warnings.push(format!("directory {prefix}/ at LBA {lba}: {e}"));
                return Ok(());
            }
        };
        let mut subdirs = Vec::new();
        for sector in bytes.chunks(SECTOR as usize) {
            let mut o = 0;
            while o < sector.len() {
                let len = sector[o] as usize;
                if len == 0 {
                    break; // records never span sectors; rest is padding
                }
                if o + len > sector.len() {
                    return malformed(format!("directory record overruns sector at LBA {lba}"));
                }
                let rec = parse_record(&sector[o..o + len])?;
                o += len;
                if rec.name == [0] || rec.name == [1] {
                    continue; // "." and ".."
                }
                let full = String::from_utf8_lossy(&rec.name).into_owned();
                let (base, version) = match full.rsplit_once(';') {
                    Some((b, v)) => (b.to_string(), v.parse().ok()),
                    None => (full, None),
                };
                let path = format!("{prefix}/{base}");
                let is_dir = rec.flags & 0x02 != 0;
                if is_dir {
                    subdirs.push((path.clone(), rec.lba, rec.size));
                }
                self.entries.push(Entry {
                    path,
                    lba: rec.lba,
                    size: rec.size,
                    is_dir,
                    version,
                    xa_attributes: rec.xa.map(|x| x.0),
                    xa_file_number: rec.xa.map(|x| x.1),
                    recorded: rec.recorded,
                });
            }
        }
        for (path, lba, size) in subdirs {
            self.walk(disc, &path, lba, size, depth + 1, seen)?;
        }
        Ok(())
    }

    /// Case-insensitive lookup; accepts `cdrom:\X;1`, `\X`, `/X` or `X`.
    pub fn find(&self, path: &str) -> Option<&Entry> {
        let want = normalize(path);
        self.entries.iter().find(|e| e.path.eq_ignore_ascii_case(&want))
    }

    /// [`find`](Self::find), failing with `NotFound` when the path is absent.
    pub fn entry(&self, path: &str) -> Result<&Entry> {
        self.find(path).ok_or_else(|| MediaError::NotFound(path.to_string()))
    }

    /// The named file whose extent covers `lba`, if any.
    pub fn owner_of(&self, lba: u32) -> Option<&Entry> {
        self.entries.iter().filter(|e| !e.is_dir).find(|e| e.contains_lba(lba))
    }

    /// File contents as 2048-byte Form 1 payloads, trimmed to the recorded size.
    /// Fails on Form 2 sectors rather than silently dropping their extra bytes.
    pub fn read_file(&self, disc: &Disc, entry: &Entry) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(entry.size as usize);
        for i in 0..entry.sectors() {
            let s = disc.read_data(entry.lba + i)?;
            if s.form == SectorForm::Form2 {
                return Err(MediaError::Unsupported(format!(
                    "{} contains Form 2 sectors (LBA {}); extract raw sectors instead",
                    entry.path, s.lba
                )));
            }
            out.extend_from_slice(&s.data[..SECTOR as usize]);
        }
        out.truncate(entry.size as usize);
        Ok(out)
    }

    /// File extent as concatenated raw 2352-byte sectors (for XA/STR tools).
    pub fn read_file_raw(&self, disc: &Disc, entry: &Entry) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for i in 0..entry.sectors() {
            out.extend_from_slice(&disc.read_raw(entry.lba + i)?.0);
        }
        Ok(out)
    }
}

pub fn normalize(path: &str) -> String {
    let p = path.trim();
    let p = p.strip_prefix("cdrom:").or_else(|| p.strip_prefix("CDROM:")).unwrap_or(p);
    let p = p.split(';').next().unwrap_or("");
    let p = p.replace('\\', "/");
    let p = p.trim_start_matches('/');
    format!("/{p}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_boot_paths() {
        assert_eq!(normalize("cdrom:\\SLUS_000.79;1"), "/SLUS_000.79");
        assert_eq!(normalize("cdrom:SLUS_000.79;1"), "/SLUS_000.79");
        assert_eq!(normalize("/DATA/X.BIN"), "/DATA/X.BIN");
    }
}

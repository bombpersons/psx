//! Validated archive indexes. Offsets and sizes are bytes in the parent source;
//! entries retain their original zero-based index, including empty entries.
//! This module describes containers, without guessing or decoding their payloads.

use serde::Serialize;

const SECTOR: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    DotT,
    CrocDir,
    External,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub offset: usize,
    pub size: usize,
}

impl Entry {
    pub fn bytes<'a>(&self, parent: &'a [u8]) -> Option<&'a [u8]> {
        parent.get(self.offset..self.offset.checked_add(self.size)?)
    }

    pub fn contains(&self, offset: usize) -> bool {
        offset.checked_sub(self.offset).is_some_and(|relative| relative < self.size)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Archive {
    pub format: Format,
    pub entries: Vec<Entry>,
}

impl Archive {
    /// External indexes retain input order and empty entries. Nonempty spans
    /// must be disjoint so a hit has exactly one unambiguous parent entry.
    pub fn external(entries: Vec<Entry>, source_len: usize) -> Result<Self, String> {
        if entries.len() > 65536 {
            return Err("external archive exceeds 65536 entries".into());
        }
        for (index, entry) in entries.iter().enumerate() {
            if entry.index != index || entry.offset.checked_add(entry.size).is_none_or(|end| end > source_len) {
                return Err(format!("external archive entry {index} has an invalid index or byte span"));
            }
        }
        let mut spans: Vec<_> = entries.iter().filter(|e| e.size != 0).map(|e| (e.offset, e.offset + e.size)).collect();
        spans.sort_unstable();
        if spans.windows(2).any(|w| w[0].1 > w[1].0) {
            return Err("external archive entries overlap".into());
        }
        Ok(Self { format: Format::External, entries })
    }
    /// DOT-T: LE u16 count, followed by count+1 LE u16 sector offsets.
    /// Equal boundaries are empty entries. The final boundary must equal EOF.
    /// Padding contents are not part of explicit format validation.
    pub fn dot_t(bytes: &[u8]) -> Result<Self, String> {
        let count = u16::from_le_bytes(bytes.get(..2).ok_or("DOT-T count truncated")?.try_into().unwrap()) as usize;
        let table_end = 2 + (count + 1) * 2;
        let table = bytes.get(2..table_end).ok_or("DOT-T offset table truncated")?;
        let offsets: Vec<usize> = table.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b) as usize * SECTOR).collect();
        if offsets[0] < table_end {
            return Err("DOT-T first entry overlaps the offset table".into());
        }
        if offsets.windows(2).any(|w| w[0] > w[1]) {
            return Err("DOT-T offsets are not ordered".into());
        }
        if offsets.last().copied() != Some(bytes.len()) {
            return Err("DOT-T final offset does not match the file boundary".into());
        }
        Ok(Self {
            format: Format::DotT,
            entries: offsets
                .windows(2)
                .enumerate()
                .map(|(index, w)| Entry { index, name: None, offset: w[0], size: w[1] - w[0] })
                .collect(),
        })
    }

    /// Conservative signatureless detection: require at least one entry and
    /// zero padding to the next sector after the table. Call dot_t explicitly
    /// for layouts with extra/nonzero padding or zero entries.
    pub fn detect_dot_t(bytes: &[u8]) -> Option<Self> {
        // Most assets and arbitrary files fail before allocating an index.
        if bytes.len() < SECTOR || !bytes.len().is_multiple_of(SECTOR) {
            return None;
        }
        let count = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        if count == 0 {
            return None;
        }
        let table_end = 2 + (count + 1) * 2;
        let first = u16::from_le_bytes([bytes[2], bytes[3]]) as usize * SECTOR;
        if first != table_end.next_multiple_of(SECTOR) || bytes.get(table_end..first)?.iter().any(|&b| b != 0) {
            return None;
        }
        Self::dot_t(bytes).ok()
    }

    /// Croc's separate .DIR index: LE u32 count, then 24-byte records containing
    /// a 12-byte name, u32 size, u32 byte offset and reserved word. Optional
    /// trailing data (including Croc's checksum) is not interpreted here.
    pub fn croc_dir(directory: &[u8], archive_len: usize) -> Result<Self, String> {
        let count = u32::from_le_bytes(directory.get(..4).ok_or("Croc DIR count truncated")?.try_into().unwrap()) as usize;
        if count == 0 || count > (directory.len() - 4) / 24 {
            return Err("Croc DIR entry table truncated or empty".into());
        }
        let mut entries = Vec::with_capacity(count);
        for (index, record) in directory[4..4 + count * 24].as_chunks::<24>().0.iter().enumerate() {
            let name: String = record[..12].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_graphic()) {
                return Err(format!("Croc DIR entry {index} has an invalid name"));
            }
            let size = u32::from_le_bytes(record[12..16].try_into().unwrap()) as usize;
            let offset = u32::from_le_bytes(record[16..20].try_into().unwrap()) as usize;
            if offset.checked_add(size).is_none_or(|end| end > archive_len) {
                return Err(format!("Croc DIR entry {index} exceeds the archive boundary"));
            }
            entries.push(Entry { index, name: Some(name), offset, size });
        }
        Ok(Self { format: Format::CrocDir, entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(offsets: &[u16], sectors: usize) -> Vec<u8> {
        let mut bytes = vec![0; sectors * SECTOR];
        bytes[..2].copy_from_slice(&((offsets.len() - 1) as u16).to_le_bytes());
        for (i, offset) in offsets.iter().enumerate() {
            bytes[2 + i * 2..4 + i * 2].copy_from_slice(&offset.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn external_spans_check_bounds_and_overlap_without_reordering_entries() {
        let entry = |index, offset, size| Entry { index, offset, size, name: None };
        let entries = vec![entry(0, 8, 2), entry(1, 10, 0), entry(2, 0, 8)];
        assert_eq!(Archive::external(entries.clone(), 10).unwrap().entries, entries);
        for entries in [
            vec![entry(1, 0, 1)],
            vec![entry(0, 0, 11)],
            vec![entry(0, 11, 0)],
            vec![entry(0, usize::MAX, 1)],
            vec![entry(0, 0, 3), entry(1, 2, 1)],
        ] {
            assert!(Archive::external(entries, 10).is_err());
        }
    }

    #[test]
    fn sector_spans_and_empty_entries_keep_their_indexes() {
        let bytes = fixture(&[1, 2, 2, 4], 4);
        let archive = Archive::detect_dot_t(&bytes).unwrap();
        assert_eq!(archive.format, Format::DotT);
        assert_eq!(
            archive.entries.iter().map(|e| (e.index, e.offset, e.size)).collect::<Vec<_>>(),
            [(0, 2048, 2048), (1, 4096, 0), (2, 4096, 4096)]
        );
        assert_eq!(archive.entries[1].bytes(&bytes), Some(&[][..]));
        assert!(!archive.entries[1].contains(4096));
        assert!(!archive.entries[0].contains(4096));
        assert!(archive.entries[2].contains(4096));
    }

    #[test]
    fn rejects_truncated_overlapping_unordered_and_inexact_tables() {
        for bytes in
            [vec![], vec![1], vec![2, 0, 1, 0], fixture(&[0, 2], 2), fixture(&[1, 3, 2, 4], 4), fixture(&[1, 3], 2), fixture(&[1, 1], 2)]
        {
            assert!(Archive::dot_t(&bytes).is_err());
            assert!(Archive::detect_dot_t(&bytes).is_none());
        }
        let mut unaligned = fixture(&[1, 2], 2);
        unaligned.push(0);
        assert!(Archive::dot_t(&unaligned).is_err());
    }

    #[test]
    fn large_tables_are_checked_before_the_first_payload() {
        let mut offsets = vec![2; 1024];
        offsets.push(3);
        let bytes = fixture(&offsets, 3);
        assert_eq!(Archive::detect_dot_t(&bytes).unwrap().entries.len(), 1024);
        let mut overlap = bytes;
        overlap[2..4].copy_from_slice(&1u16.to_le_bytes());
        assert!(Archive::dot_t(&overlap).is_err());
    }

    #[test]
    fn explicit_parser_and_conservative_detection_have_distinct_padding_rules() {
        let mut bytes = fixture(&[2, 3], 3);
        assert!(Archive::dot_t(&bytes).is_ok());
        assert!(Archive::detect_dot_t(&bytes).is_none());
        bytes = fixture(&[1, 2], 2);
        bytes[100] = 1;
        assert!(Archive::dot_t(&bytes).is_ok());
        assert!(Archive::detect_dot_t(&bytes).is_none());
        let empty = fixture(&[1], 1);
        assert!(Archive::dot_t(&empty).unwrap().entries.is_empty());
        assert!(Archive::detect_dot_t(&empty).is_none());
    }

    #[test]
    fn croc_directory_preserves_names_and_checks_overflow_and_bounds() {
        let mut directory = vec![0; 4 + 2 * 24];
        directory[..4].copy_from_slice(&2u32.to_le_bytes());
        for (i, (name, size, offset)) in [("A.BIN", 10u32, 0u32), ("B.WAD", 6, 10)].into_iter().enumerate() {
            let at = 4 + i * 24;
            directory[at..at + name.len()].copy_from_slice(name.as_bytes());
            directory[at + 12..at + 16].copy_from_slice(&size.to_le_bytes());
            directory[at + 16..at + 20].copy_from_slice(&offset.to_le_bytes());
        }
        let archive = Archive::croc_dir(&directory, 16).unwrap();
        assert_eq!(archive.entries[1], Entry { index: 1, name: Some("B.WAD".into()), offset: 10, size: 6 });
        assert!(Archive::croc_dir(&directory, 15).is_err());
        directory[4 + 24 + 16..4 + 24 + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Archive::croc_dir(&directory, 16).is_err());
        assert!(Archive::croc_dir(&u32::MAX.to_le_bytes(), 16).is_err());
    }
}

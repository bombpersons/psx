//! Explicit adapters for preprocessed TMD variants. Standard signature scanning
//! remains unchanged. The current profile is validated against King's Field
//! (PAL SCES-00510) RTMD banks, not claimed compatible with other games.

use psx_assets::tmd::{polygon_reference_range, Tmd, TMD_ID};

/// Both representations are useful: normal game code consumes the typed model,
/// and the existing template can load the normalized standard TMD bytes.
pub struct NormalizedTmd {
    pub model: Tmd,
    /// Same length as the input; sector padding and non-reference bytes retained.
    pub bytes: Vec<u8>,
}

fn word(bytes: &[u8], at: usize) -> Result<u32, String> {
    let end = at.checked_add(4).ok_or("RTMD word offset overflow")?;
    Ok(u32::from_le_bytes(bytes.get(at..end).ok_or("RTMD header/table truncated")?.try_into().unwrap()))
}

/// Normalize King's Field's {0, 0x10/0x12, object_count} header and byte-offset
/// references into ordinary TMD. Vector records are eight bytes; only actual
/// vertex/normal references are divided by eight, not UVs, colours or padding.
/// Unknown polygon layouts and non-divisible references are errors. Full TMD
/// validation runs after normalization, including vertex/normal bounds.
pub fn normalize_kings_field(bytes: &[u8]) -> Result<NormalizedTmd, String> {
    if word(bytes, 0)? != 0 || !matches!(word(bytes, 4)?, 0x10 | 0x12) {
        return Err("not a King's Field RTMD header (expected 0, 0x10 or 0x12)".into());
    }
    let objects = word(bytes, 8)? as usize;
    if objects == 0 || objects > 4096 {
        return Err(format!("RTMD implausible object count {objects}"));
    }
    let table_end = 12 + objects * 28;
    bytes.get(..table_end).ok_or("RTMD object table truncated")?;
    let mut normalized = bytes.to_vec();
    normalized[..4].copy_from_slice(&TMD_ID.to_le_bytes());
    normalized[4..8].fill(0);
    for object in 0..objects {
        let at = 12 + object * 28;
        let count = word(bytes, at + 20)? as usize;
        if count > 0x10000 {
            return Err(format!("RTMD object {object}: implausible primitive count {count}"));
        }
        let pointer = word(bytes, at + 16)? as usize;
        let mut packet = 12usize.checked_add(pointer).ok_or("RTMD primitive pointer overflow")?;
        if count != 0 && (packet < table_end || !pointer.is_multiple_of(4)) {
            return Err(format!("RTMD object {object}: primitive table overlaps header or is unaligned"));
        }
        for primitive in 0..count {
            let header = word(bytes, packet)?;
            let ilen = ((header >> 8) & 0xff) as usize;
            let flag = (header >> 16) as u8;
            let mode = (header >> 24) as u8;
            if flag & !7 != 0 {
                return Err(format!("RTMD object {object}, primitive {primitive}: reserved polygon flags"));
            }
            let refs = polygon_reference_range(mode, flag, ilen)
                .ok_or_else(|| format!("RTMD object {object}, primitive {primitive}: unsupported polygon layout"))?;
            let payload = packet.checked_add(4).ok_or("RTMD packet offset overflow")?;
            let end = payload.checked_add(ilen * 4).ok_or("RTMD packet size overflow")?;
            bytes.get(payload..end).ok_or("RTMD primitive truncated")?;
            for field in (refs.start..refs.end).step_by(2) {
                let location = payload + field;
                let reference = u16::from_le_bytes(bytes[location..location + 2].try_into().unwrap());
                if !reference.is_multiple_of(8) {
                    return Err(format!("RTMD object {object}, primitive {primitive}: reference {reference} is not an eight-byte offset"));
                }
                normalized[location..location + 2].copy_from_slice(&(reference / 8).to_le_bytes());
            }
            packet = end;
        }
    }
    let model = Tmd::parse(&normalized)?;
    if model.polygon_count() == 0 || model.objects.iter().any(|o| o.skipped != 0) {
        return Err("RTMD contains no polygons or unsupported primitives".into());
    }
    Ok(NormalizedTmd { model, bytes: normalized })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independently authored standard packets plus their actual reference byte
    /// locations. UVs, colours and padding deliberately contain nonmultiples of 8.
    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<usize>) {
        let mut standard = Vec::new();
        for word in [TMD_ID, 0, 1, 28, 4, 60, 4, 92, 3, 0] {
            standard.extend_from_slice(&word.to_le_bytes());
        }
        for vector in 0..8i16 {
            for value in [vector * 10, -vector * 3, vector, 0] {
                standard.extend_from_slice(&value.to_le_bytes());
            }
        }
        let mut locations = Vec::new();
        // FT4: 4 UV words, (normal, v0, v1, v2, v3), padding.
        standard.extend_from_slice(&[9, 7, 0, 0x2c]);
        for uv in [0x7fc0_0301u32, 0x0085_0705, 0x0000_0b09, 0x0000_0f0d] {
            standard.extend_from_slice(&uv.to_le_bytes());
        }
        for reference in [1u16, 0, 1, 3, 2] {
            locations.push(standard.len());
            standard.extend_from_slice(&reference.to_le_bytes());
        }
        standard.extend_from_slice(&0xf00du16.to_le_bytes());
        // Unlit G3: 3 colours, 3 vertex references, padding.
        standard.extend_from_slice(&[6, 5, 1, 0x31]);
        standard.extend_from_slice(&[1, 3, 5, 0x31, 7, 9, 11, 0, 13, 15, 17, 0]);
        for reference in [0u16, 2, 3] {
            locations.push(standard.len());
            standard.extend_from_slice(&reference.to_le_bytes());
        }
        standard.extend_from_slice(&0xe10du16.to_le_bytes());
        // Lit GT3: 3 UV words, (normal, vertex) repeated per corner.
        standard.extend_from_slice(&[9, 6, 0, 0x34]);
        for uv in [0x7fc0_0301u32, 0x0085_0705, 0x0000_0b09] {
            standard.extend_from_slice(&uv.to_le_bytes());
        }
        for reference in [0u16, 0, 2, 1, 3, 2] {
            locations.push(standard.len());
            standard.extend_from_slice(&reference.to_le_bytes());
        }
        standard.extend_from_slice(&[0x55; 20]); // Preserve trailing source bytes.
        let mut source = standard.clone();
        source[..4].fill(0);
        source[4..8].copy_from_slice(&0x12u32.to_le_bytes());
        for &at in &locations {
            let reference = u16::from_le_bytes(standard[at..at + 2].try_into().unwrap());
            source[at..at + 2].copy_from_slice(&(reference * 8).to_le_bytes());
        }
        (source, standard, locations)
    }

    #[test]
    fn normalizes_only_header_and_references_and_reuses_standard_decoding() {
        let (source, standard, _) = fixture();
        let original = source.clone();
        let normalized = normalize_kings_field(&source).unwrap();
        assert_eq!(source, original);
        assert_eq!(normalized.bytes, standard);
        assert!(Tmd::parse(&source).is_err());
        assert!(psx_assets::scan::scan(&source, Default::default()).is_empty());
        let object = &normalized.model.objects[0];
        assert_eq!(object.vertices[3], [30, -9, 3]);
        assert_eq!(object.normals[1], [50, -15, 5]);
        assert_eq!(object.polygons[0].vertices, [0, 1, 3, 2]);
        assert_eq!(object.polygons[0].normals, [1]);
        assert_eq!(object.polygons[0].cba, 0x7fc0);
        assert_eq!(object.polygons[0].tsb, 0x85);
        assert_eq!(object.polygons[1].vertices, [0, 2, 3]);
        assert_eq!(object.polygons[1].colors, [[1, 3, 5], [7, 9, 11], [13, 15, 17]]);
        assert_eq!(object.polygons[2].normals, [0, 2, 3]);
        assert_eq!(normalized.model.polygon_count(), 3);
        let mut alternate = source;
        alternate[4..8].copy_from_slice(&0x10u32.to_le_bytes());
        assert_eq!(normalize_kings_field(&alternate).unwrap().bytes, standard);
    }

    #[test]
    fn rejects_bad_headers_counts_packet_layouts_and_nonintegral_references() {
        let (source, _, locations) = fixture();
        for (at, value) in [(0, 0x41u32), (4, 0x13), (8, 0), (8, 4097), (32, 0x10001), (28, u32::MAX), (28, 93)] {
            let mut bad = source.clone();
            bad[at..at + 4].copy_from_slice(&value.to_le_bytes());
            assert!(normalize_kings_field(&bad).is_err(), "field {at}, value {value}");
        }
        for (at, value) in [(104 + 1, 6u8), (104 + 2, 0x80), (104 + 3, 0x40)] {
            let mut bad = source.clone();
            bad[at] = value;
            assert!(normalize_kings_field(&bad).is_err());
        }
        let mut bad = source.clone();
        bad[locations[1]..locations[1] + 2].copy_from_slice(&1u16.to_le_bytes());
        assert!(normalize_kings_field(&bad).err().unwrap().contains("eight-byte offset"));
        for end in [0, 8, 39, 106, source.len() - 24] {
            assert!(normalize_kings_field(&source[..end]).is_err());
        }
    }

    #[test]
    fn normalized_vertex_and_normal_references_still_need_to_be_in_range() {
        let (source, _, locations) = fixture();
        for at in [locations[0], locations[1]] {
            let mut bad = source.clone();
            bad[at..at + 2].copy_from_slice(&32u16.to_le_bytes()); // index 4, only 4 vectors
            assert!(normalize_kings_field(&bad).err().unwrap().contains("index out of range"));
        }
    }
}

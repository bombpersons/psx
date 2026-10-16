//! SPU-ADPCM (psx-spx "SPU ADPCM Samples"): the sample format of sound RAM,
//! `.VAG` files and `.VB` banks.
//!
//! A block is 16 bytes: shift/filter, flags (bit 0 loop end, bit 1 repeat,
//! bit 2 loop start), then 28 4-bit samples, low nibble first. This is the
//! decoder `psx-core`'s SPU voices use, so offline decodes match the emulator.

/// Positive and negative filter coefficients (/64), filters 0-4.
pub const FILTER_POS: [i32; 5] = [0, 60, 115, 98, 122];
pub const FILTER_NEG: [i32; 5] = [0, 0, -52, -55, -60];

/// Block flag: end of the sample (jump to the repeat address).
pub const FLAG_END: u8 = 1;
/// Block flag: with [`FLAG_END`], loop to the repeat address instead of releasing.
pub const FLAG_REPEAT: u8 = 2;
/// Block flag: this block is the loop start (sets the repeat address).
pub const FLAG_START: u8 = 4;

/// The bytes a voice keyed on at `start` plays up to its first END block:
/// whole blocks from `start` through that block, clipped to `data`.
pub fn sample_extent(data: &[u8], start: usize) -> std::ops::Range<usize> {
    let mut end = start;
    while end + 16 <= data.len() {
        end += 16;
        if data[end - 15] & FLAG_END != 0 {
            break;
        }
    }
    start..end
}

/// A sample's identity: SHA-256 (hex) of its [`sample_extent`] bytes. The SPU
/// voice trace and VAB/VAG extraction report the same value, so a keyed-on
/// voice can be matched to the bank waveform it plays.
pub fn sample_sha256(data: &[u8], start: usize) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(&data[sample_extent(data, start)]);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode one 16-byte block (only `block[..16]` is read) into 28 samples.
/// `hist` is the filter history `[newest, older]`, carried between blocks.
pub fn decode_block(block: &[u8], hist: &mut [i32; 2], out: &mut [i16; 28]) {
    let header = block[0];
    // Shifts 13-15 are clamped to 12 (the SPU core's rule).
    let shift = (header & 0xF).min(12) as i32;
    let filter = ((header >> 4) & 7).min(4) as usize;
    for (n, o) in out.iter_mut().enumerate() {
        let byte = block[2 + n / 2];
        let nib = if n & 1 == 0 { byte & 0xF } else { byte >> 4 };
        let raw = (((nib as i16) << 12) >> shift) as i32;
        let s = raw + (hist[0] * FILTER_POS[filter] + hist[1] * FILTER_NEG[filter] + 32) / 64;
        let s = s.clamp(-0x8000, 0x7FFF);
        hist[1] = hist[0];
        hist[0] = s;
        *o = s as i16;
    }
}

/// Where a decoded stream's loop is, in samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Loop {
    pub start: usize,
    pub end: usize,
}

/// A decoded sample: PCM and loop points (from the block flags).
#[derive(Clone, Debug, Default)]
pub struct Decoded {
    pub pcm: Vec<i16>,
    /// Set when the END block also has REPEAT (a looping sample).
    pub looped: Option<Loop>,
    /// Blocks consumed (including the terminating END block).
    pub blocks: usize,
}

/// Decode blocks until a block with the END flag (inclusive) or the data ends.
/// Encoders commonly put an all-zero block first and a silent END block last;
/// both decode to silence and are kept so sample offsets match sound RAM.
pub fn decode_stream(data: &[u8]) -> Decoded {
    let mut hist = [0; 2];
    let mut out = Decoded::default();
    let mut loop_start = 0;
    let mut buf = [0i16; 28];
    for block in data.as_chunks::<16>().0 {
        let flags = block[1];
        if flags & FLAG_START != 0 {
            loop_start = out.pcm.len();
        }
        decode_block(block, &mut hist, &mut buf);
        out.pcm.extend_from_slice(&buf);
        out.blocks += 1;
        if flags & FLAG_END != 0 {
            if flags & FLAG_REPEAT != 0 {
                out.looped = Some(Loop { start: loop_start, end: out.pcm.len() });
            }
            break;
        }
    }
    out
}

/// Whether `data` looks like one SPU-ADPCM sample: whole blocks, every
/// header with filter 0-4 and shift 0-12, flags within bits 0-2, and the END
/// flag on the last block and nowhere before the second-to-last (encoders
/// end samples with an END block followed by a silent `flags = 7` block). Used to reject VB bodies that are not contiguous
/// (archives that store a VAB's waveforms elsewhere).
pub fn plausible_sample(data: &[u8]) -> bool {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return false;
    }
    let blocks = data.as_chunks::<16>().0;
    let last = blocks.len() - 1;
    blocks
        .iter()
        .enumerate()
        .all(|(i, b)| b[0] >> 4 <= 4 && b[0] & 0xF <= 12 && b[1] <= 7 && (b[1] & FLAG_END == 0 || i + 1 >= last))
        && blocks[last][1] & FLAG_END != 0
}

/// Test helper: one block with the given header, flags and nibbles.
#[cfg(test)]
pub(crate) fn block(header: u8, flags: u8, nibbles: &[u8; 28]) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0] = header;
    b[1] = flags;
    for (i, pair) in nibbles.chunks(2).enumerate() {
        b[2 + i] = (pair[0] & 0xF) | (pair[1] << 4);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_extent_stops_after_the_first_end_block() {
        let mut data = vec![0u8; 16 * 5];
        data[16 * 2 + 1] = FLAG_END | FLAG_REPEAT;
        assert_eq!(sample_extent(&data, 16), 16..48);
        assert_eq!(sample_extent(&data, 48), 48..80, "no END block: up to the end of the data");
        assert_eq!(sample_extent(&data[..40], 16), 16..32, "partial blocks are not read");
        assert_eq!(sample_sha256(&data, 16), sample_sha256(&data[16..48], 0));
        assert_ne!(sample_sha256(&data, 16), sample_sha256(&data, 0));
    }

    #[test]
    fn filter_zero_is_shifted_nibbles() {
        let mut nib = [0u8; 28];
        nib[0] = 7; // +7 << 12 >> 4 = 0x700
        nib[1] = 8; // -8 << 12 >> 4 = -0x800
        let b = block(0x04, 0, &nib);
        let mut hist = [0; 2];
        let mut out = [0; 28];
        decode_block(&b, &mut hist, &mut out);
        assert_eq!(&out[..3], &[0x700, -0x800, 0]);
    }

    #[test]
    fn filter_one_integrates() {
        // Filter 1: s = raw + old*60/64.
        let mut nib = [0u8; 28];
        nib[0] = 1;
        let b = block(0x10 | 0x0C, 0, &nib); // shift 12: raw = 1
        let mut hist = [0x4000, 0];
        let mut out = [0; 28];
        decode_block(&b, &mut hist, &mut out);
        // (0x4000*60 + 32)/64 + 1 = 15361
        assert_eq!(out[0], 15361);
        assert!(out[27] > 0 && out[27] < out[0]);
    }

    #[test]
    fn stream_stops_at_end_and_reports_loop() {
        let z = [0u8; 28];
        let mut data = Vec::new();
        data.extend_from_slice(&block(0, 0, &z));
        data.extend_from_slice(&block(0, FLAG_START, &z));
        data.extend_from_slice(&block(0, FLAG_END | FLAG_REPEAT, &z));
        data.extend_from_slice(&block(0, 0, &z)); // never reached
        let d = decode_stream(&data);
        assert_eq!(d.blocks, 3);
        assert_eq!(d.pcm.len(), 84);
        assert_eq!(d.looped, Some(Loop { start: 28, end: 84 }));
        assert!(plausible_sample(&data[..48]));
        assert!(!plausible_sample(&data[..32]), "no END flag");
        assert!(!plausible_sample(&data[..47]));
        assert!(!plausible_sample(&data), "END before the last two blocks");
        let mut two_ends = data[..48].to_vec();
        two_ends[17] = FLAG_END;
        assert!(plausible_sample(&two_ends));
        let mut bad = data[..48].to_vec();
        bad[16] = 0x5D; // filter 5
        assert!(!plausible_sample(&bad));
    }
}

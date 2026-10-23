//! STR movie streams: sector demux, frame reassembly and the BS bitstream
//! (psx-spx "CDROM File Video Streaming STR" and "BS Compression").
//!
//! A video sector's user data starts with a 20h-byte STR header (ID 0160h,
//! type 8001h). The rest is one chunk of a frame; the chunks of a frame
//! concatenate into a BS image: an 8-byte header (MDEC size, 3800h, quant,
//! version) and a Huffman bitstream. Games decompress the bitstream in
//! software into MDEC run-length halfwords and hand those to the MDEC; this
//! module does the software half. Turning the halfwords into pixels is the
//! hardware's job (`psx_core::mdec`).
//!
//! Supported: BS v1, v2 and v3. Detected and reported as unsupported: v0,
//! iki, encrypted and other variants (anything whose header is not
//! `3800h` + version 1..3), and non-MDEC streams that reuse the STR header.

use crate::le::{u16le, u32le};
use std::fmt;

/// MDEC end-of-block halfword.
pub const EOB: u16 = 0xFE00;

/// Movie sector types that carry BS frames: 8001h, and 8101h (Chrono Cross disc 2).
fn is_mdec_type(kind: u16) -> bool {
    kind & !0x0100 == 0x8001
}

/// The 20h-byte STR sector header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StrHeader {
    pub kind: u16,
    /// Sector index within the frame.
    pub chunk: u16,
    /// Sectors in the frame.
    pub chunks: u16,
    pub frame: u32,
    /// Frame data bytes (BS header + bitstream, without STR headers or padding).
    pub frame_size: u32,
    pub width: u16,
    pub height: u16,
}

pub const STR_HEADER_LEN: usize = 0x20;

impl StrHeader {
    /// Parse the header of a sector's user data. Returns `None` unless it has ID
    /// 0160h; check [`StrHeader::is_video`] for the MDEC type.
    pub fn parse(data: &[u8]) -> Option<StrHeader> {
        if data.len() < STR_HEADER_LEN || u16le(data, 0) != 0x0160 {
            return None;
        }
        Some(StrHeader {
            kind: u16le(data, 2),
            chunk: u16le(data, 4),
            chunks: u16le(data, 6),
            frame: u32le(data, 8),
            frame_size: u32le(data, 12),
            width: u16le(data, 16),
            height: u16le(data, 18),
        })
    }

    pub fn is_video(&self) -> bool {
        is_mdec_type(self.kind) && self.chunks > 0 && self.chunk < self.chunks
    }
}

/// A reassembled frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub number: u32,
    pub width: u16,
    pub height: u16,
    pub first_lba: u32,
    pub last_lba: u32,
    pub chunks: u16,
    /// Chunks that never arrived.
    pub missing: Vec<u16>,
    pub frame_size: u32,
    /// Concatenated chunk payloads, in chunk order.
    pub data: Vec<u8>,
}

struct Partial {
    header: StrHeader,
    first_lba: u32,
    last_lba: u32,
    payloads: Vec<Option<Vec<u8>>>,
}

impl Partial {
    fn finish(self) -> Frame {
        let missing = (0..self.header.chunks).filter(|&i| self.payloads[i as usize].is_none()).collect();
        let mut data = Vec::new();
        for p in self.payloads.into_iter().flatten() {
            data.extend_from_slice(&p);
        }
        Frame {
            number: self.header.frame,
            width: self.header.width,
            height: self.header.height,
            first_lba: self.first_lba,
            last_lba: self.last_lba,
            chunks: self.header.chunks,
            missing,
            frame_size: self.header.frame_size,
            data,
        }
    }
}

/// Collects video sectors into frames. Feed sectors in disc order; a frame is
/// emitted when all its chunks have arrived, or when a sector of another frame
/// shows up first (then [`Frame::missing`] lists the gaps). Other sectors
/// (audio, padding) may be interleaved freely.
#[derive(Default)]
pub struct FrameAssembler {
    current: Option<Partial>,
}

impl FrameAssembler {
    /// Push one video sector's user data (STR header included). Returns any
    /// frames completed by it, in order.
    pub fn push(&mut self, lba: u32, header: &StrHeader, data: &[u8]) -> Vec<Frame> {
        let mut done = Vec::new();
        let same = self.current.as_ref().is_some_and(|p| {
            p.header.frame == header.frame
                && p.header.chunks == header.chunks
                && p.payloads[header.chunk as usize].is_none()
        });
        if !same {
            if let Some(p) = self.current.take() {
                done.push(p.finish());
            }
            self.current = Some(Partial {
                header: *header,
                first_lba: lba,
                last_lba: lba,
                payloads: vec![None; header.chunks as usize],
            });
        }
        let p = self.current.as_mut().expect("partial frame");
        p.last_lba = lba;
        p.payloads[header.chunk as usize] = Some(data[STR_HEADER_LEN.min(data.len())..].to_vec());
        if p.payloads.iter().all(Option::is_some) {
            done.push(self.current.take().expect("partial frame").finish());
        }
        done
    }

    /// Flush a trailing incomplete frame.
    pub fn finish(&mut self) -> Option<Frame> {
        self.current.take().map(Partial::finish)
    }
}

/// BS frame header (first 8 bytes of a frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BsHeader {
    /// MDEC data size in 32-bit words after Huffman decompression (rounded up to 20h words).
    pub mdec_words: u16,
    pub id: u16,
    pub quant: u16,
    pub version: u16,
}

impl BsHeader {
    pub fn parse(frame: &[u8]) -> Option<BsHeader> {
        (frame.len() >= 8).then(|| BsHeader {
            mdec_words: u16le(frame, 0),
            id: u16le(frame, 2),
            quant: u16le(frame, 4),
            version: u16le(frame, 6),
        })
    }

    /// A short name of the variant, for reports: `v1`..`v3`, or a guess at the
    /// unsupported format.
    pub fn variant(&self) -> String {
        match (self.id, self.version) {
            (0x3800, 1..=3) if self.quant <= 0x3F => format!("v{}", self.version),
            (0x3800, 1..=3) if self.quant >= 0x8000 => "fraquant".into(),
            (0x3800, 0) => "v0".into(),
            (0x3800, _) => "iki_or_encrypted".into(),
            _ => "not_bs".into(),
        }
    }
}

/// Offset of the BS header inside a frame: 0, or 28h for Final Fantasy VII's
/// movies, which put 28h bytes of extra (camera) data before it.
pub fn bs_offset(frame: &[u8]) -> usize {
    let bs_at = |o: usize| frame.get(o..).and_then(BsHeader::parse).is_some_and(|h| h.id == 0x3800);
    if !bs_at(0) && bs_at(0x28) { 0x28 } else { 0 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BsError {
    TooShort,
    /// Not a supported BS header; carries [`BsHeader::variant`].
    Unsupported(String),
    /// The bitstream ran out in this macroblock.
    Truncated {
        macroblock: usize,
    },
    /// An invalid Huffman code.
    BadCode {
        macroblock: usize,
        block: usize,
        bit: usize,
    },
    /// More than 63 AC coefficients in a block.
    BlockOverflow {
        macroblock: usize,
        block: usize,
    },
}

impl fmt::Display for BsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BsError::TooShort => write!(f, "frame shorter than a BS header"),
            BsError::Unsupported(v) => write!(f, "unsupported BS variant {v}"),
            BsError::Truncated { macroblock } => write!(f, "bitstream ends inside macroblock {macroblock}"),
            BsError::BadCode { macroblock, block, bit } => {
                write!(f, "invalid Huffman code in macroblock {macroblock} block {block} at bit {bit}")
            }
            BsError::BlockOverflow { macroblock, block } => {
                write!(f, "more than 63 AC values in macroblock {macroblock} block {block}")
            }
        }
    }
}

/// Run-length MDEC data for one frame.
#[derive(Clone, Debug, Default)]
pub struct Rle {
    /// MDEC halfwords: per block one DC word `(quant << 10) | dc`, AC words
    /// `(run << 10) | level`, and [`EOB`]. Six blocks (Cr, Cb, Y1..Y4) per macroblock.
    pub halfwords: Vec<u16>,
    /// Start index in `halfwords` of each macroblock.
    pub macroblocks: Vec<usize>,
    /// The bitstream's end-of-frame code was seen after the last macroblock.
    pub end_code: bool,
    /// v1 escape codes with level 0 (a known old-compressor quirk; harmless).
    pub v1_zero_escapes: usize,
}

impl Rle {
    /// Halfwords of macroblock `i`.
    pub fn macroblock(&self, i: usize) -> &[u16] {
        let end = self.macroblocks.get(i + 1).copied().unwrap_or(self.halfwords.len());
        &self.halfwords[self.macroblocks[i]..end]
    }
}

/// MSB-first reader over little-endian halfwords; reads past the end yield zeros.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn halfword(&self, i: usize) -> u64 {
        self.data.get(i * 2..i * 2 + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as u64)
    }

    /// Next `n` (≤ 32) bits, MSB first.
    fn peek(&self, n: usize) -> u32 {
        let hw = self.pos / 16;
        let window = (self.halfword(hw) << 32) | (self.halfword(hw + 1) << 16) | self.halfword(hw + 2);
        let shift = 48 - self.pos % 16 - n;
        ((window >> shift) & ((1u64 << n) - 1)) as u32
    }

    fn read(&mut self, n: usize) -> u32 {
        let v = self.peek(n);
        self.pos += n;
        v
    }

    fn exhausted(&self) -> bool {
        self.pos > self.data.len() * 8
    }

    fn leading_zeros(&self, max: usize) -> usize {
        ((self.peek(max) << (32 - max)).leading_zeros() as usize).min(max)
    }
}

/// AC tables by leading-zero count: `(bits after the zeros and the 1, values)`.
/// The last code bit is the sign. Codes with 0 and 2 leading zeros have two
/// sub-tables and are handled separately.
const AC_Z3: [u16; 4] = [0x1C01, 0x1801, 0x0402, 0x1401];
const AC_Z4: [u16; 4] = [0x0802, 0x2401, 0x0004, 0x2001];
const AC_Z2_100: [u16; 8] = [0x3401, 0x0006, 0x3001, 0x2C01, 0x0C02, 0x0403, 0x0005, 0x2801];
const AC_Z6: [u16; 8] = [0x4001, 0x1402, 0x0007, 0x0803, 0x0404, 0x3C01, 0x3801, 0x1002];
const AC_Z7: [u16; 16] = [
    0x000B, 0x2002, 0x1003, 0x000A, 0x0804, 0x1C02, 0x5401, 0x5001, 0x0009, 0x4C01, 0x4801, 0x0405, 0x0C03, 0x0008,
    0x1802, 0x4401,
];
const AC_Z8: [u16; 16] = [
    0x2802, 0x2402, 0x1403, 0x0C04, 0x0805, 0x0407, 0x0406, 0x000F, 0x000E, 0x000D, 0x000C, 0x6801, 0x6401, 0x6001,
    0x5C01, 0x5801,
];
const AC_Z9: [u16; 16] = [
    0x001F, 0x001E, 0x001D, 0x001C, 0x001B, 0x001A, 0x0019, 0x0018, 0x0017, 0x0016, 0x0015, 0x0014, 0x0013, 0x0012,
    0x0011, 0x0010,
];
const AC_Z10: [u16; 16] = [
    0x0028, 0x0027, 0x0026, 0x0025, 0x0024, 0x0023, 0x0022, 0x0021, 0x0020, 0x040E, 0x040D, 0x040C, 0x040B, 0x040A,
    0x0409, 0x0408,
];
const AC_Z11: [u16; 16] = [
    0x0412, 0x0411, 0x0410, 0x040F, 0x1803, 0x4002, 0x3C02, 0x3802, 0x3402, 0x3002, 0x2C02, 0x7C01, 0x7801, 0x7401,
    0x7001, 0x6C01,
];

/// One AC Huffman code, as decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ac {
    Eob,
    /// MDEC halfword (run << 10 | 10-bit level).
    Value(u16),
    /// Escape: raw 16-bit MDEC halfword.
    Escape(u16),
}

fn signed(v: u16, negative: bool) -> u16 {
    if !negative {
        return v;
    }
    let level = (v & 0x3FF) as i32;
    (v & 0xFC00) | ((-level) as u16 & 0x3FF)
}

fn read_ac(b: &mut Bits) -> Option<Ac> {
    let z = b.leading_zeros(12);
    let pick = |b: &mut Bits, table: &[u16], idx_bits: usize| {
        b.pos += z + 1;
        let idx = b.read(idx_bits) as usize;
        let s = b.read(1) == 1;
        Some(Ac::Value(signed(table[idx], s)))
    };
    match z {
        0 => {
            // 10 = EOB, 11s = 0001
            if b.peek(2) == 0b10 {
                b.pos += 2;
                Some(Ac::Eob)
            } else {
                b.pos += 2;
                Some(Ac::Value(signed(0x0001, b.read(1) == 1)))
            }
        }
        1 => {
            // 011s = 0401, 010xs = 0002/0801
            b.pos += 2;
            if b.read(1) == 1 {
                Some(Ac::Value(signed(0x0401, b.read(1) == 1)))
            } else {
                let idx = b.read(1) as usize;
                Some(Ac::Value(signed([0x0002, 0x0801][idx], b.read(1) == 1)))
            }
        }
        2 => {
            // 0011xs, 00101s, 00100xxxs
            b.pos += 3;
            if b.read(1) == 1 {
                let idx = b.read(1) as usize;
                Some(Ac::Value(signed([0x1001, 0x0C01][idx], b.read(1) == 1)))
            } else if b.read(1) == 1 {
                Some(Ac::Value(signed(0x0003, b.read(1) == 1)))
            } else {
                let idx = b.read(3) as usize;
                Some(Ac::Value(signed(AC_Z2_100[idx], b.read(1) == 1)))
            }
        }
        3 => pick(b, &AC_Z3, 2),
        4 => pick(b, &AC_Z4, 2),
        5 => {
            b.pos += 6;
            Some(Ac::Escape(b.read(16) as u16))
        }
        6 => pick(b, &AC_Z6, 3),
        7 => pick(b, &AC_Z7, 4),
        8 => pick(b, &AC_Z8, 4),
        9 => pick(b, &AC_Z9, 4),
        10 => pick(b, &AC_Z10, 4),
        11 => pick(b, &AC_Z11, 4),
        _ => None,
    }
}

/// v3 DC differential: `(prefix, prefix length, size)` for chroma and luma.
const DC_CHROMA: [(u32, usize, usize); 9] = [
    (0b00, 2, 0),
    (0b01, 2, 1),
    (0b10, 2, 2),
    (0b110, 3, 3),
    (0b1110, 4, 4),
    (0b11110, 5, 5),
    (0b111110, 6, 6),
    (0b1111110, 7, 7),
    (0b11111110, 8, 8),
];
const DC_LUMA: [(u32, usize, usize); 9] = [
    (0b100, 3, 0),
    (0b00, 2, 1),
    (0b01, 2, 2),
    (0b101, 3, 3),
    (0b110, 3, 4),
    (0b1110, 4, 5),
    (0b11110, 5, 6),
    (0b111110, 6, 7),
    (0b1111110, 7, 8),
];

/// End-of-frame code: v1/v2 DC value 1FFh in place of Cr; v3 `1111111111`.
const END_OF_FRAME: u32 = 0x1FF;

fn read_dc_v3(b: &mut Bits, chroma: bool) -> Option<i32> {
    let table = if chroma { &DC_CHROMA } else { &DC_LUMA };
    let &(_, len, size) = table.iter().find(|(code, len, _)| b.peek(*len) == *code)?;
    b.pos += len;
    if size == 0 {
        return Some(0);
    }
    let v = b.read(size) as i32;
    let diff = if v >> (size - 1) == 0 { v - ((1 << size) - 1) } else { v };
    Some(diff * 4)
}

/// Decompress a BS frame into MDEC run-length halfwords. `width`/`height` come
/// from the STR header; the macroblock count is `ceil(w/16) * ceil(h/16)`.
pub fn decode_bs(frame: &[u8], width: u16, height: u16) -> Result<(BsHeader, Rle), BsError> {
    let header = BsHeader::parse(frame).ok_or(BsError::TooShort)?;
    let variant = header.variant();
    if !matches!(variant.as_str(), "v1" | "v2" | "v3") {
        return Err(BsError::Unsupported(variant));
    }
    let v3 = header.version == 3;
    let quant = header.quant & 0x3F;
    let total = width.div_ceil(16) as usize * height.div_ceil(16) as usize;
    let mut b = Bits { data: &frame[8..], pos: 0 };
    let mut rle = Rle::default();
    rle.halfwords.reserve(total * 6 * 8);
    // v3 DC predictors: Cr, Cb, Y.
    let mut prev = [0i32; 3];
    for mb in 0..total {
        rle.macroblocks.push(rle.halfwords.len());
        for block in 0..6 {
            let dc = if v3 {
                if b.peek(10) == 0x3FF {
                    return Err(BsError::Truncated { macroblock: mb });
                }
                let p = block.min(2);
                let diff =
                    read_dc_v3(&mut b, block < 2).ok_or(BsError::BadCode { macroblock: mb, block, bit: b.pos })?;
                prev[p] += diff;
                (prev[p] & 0x3FF) as u16
            } else {
                let dc = b.read(10);
                if block == 0 && dc == END_OF_FRAME {
                    return Err(BsError::Truncated { macroblock: mb });
                }
                dc as u16
            };
            rle.halfwords.push((quant << 10) | dc);
            let mut k = 0usize;
            loop {
                if b.exhausted() {
                    return Err(BsError::Truncated { macroblock: mb });
                }
                let at = b.pos;
                match read_ac(&mut b).ok_or(BsError::BadCode { macroblock: mb, block, bit: at })? {
                    Ac::Eob => break,
                    Ac::Value(v) | Ac::Escape(v) => {
                        if v & 0x3FF == 0 {
                            rle.v1_zero_escapes += 1;
                        }
                        k += (v >> 10) as usize + 1;
                        if k > 63 {
                            return Err(BsError::BlockOverflow { macroblock: mb, block });
                        }
                        rle.halfwords.push(v);
                    }
                }
            }
            rle.halfwords.push(EOB);
        }
    }
    if b.exhausted() {
        return Err(BsError::Truncated { macroblock: total.saturating_sub(1) });
    }
    rle.end_code = if v3 { b.peek(10) == 0x3FF } else { b.peek(10) == END_OF_FRAME };
    Ok((header, rle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bit writer producing little-endian halfwords.
    #[derive(Default)]
    struct Writer {
        bits: Vec<bool>,
    }

    impl Writer {
        fn put(&mut self, s: &str) {
            self.bits.extend(s.chars().filter(|c| *c != ' ').map(|c| c == '1'));
        }
        fn num(&mut self, v: u32, n: usize) {
            for i in (0..n).rev() {
                self.bits.push((v >> i) & 1 == 1);
            }
        }
        fn bytes(&self) -> Vec<u8> {
            let mut out = Vec::new();
            for chunk in self.bits.chunks(16) {
                let mut hw = 0u16;
                for (i, &bit) in chunk.iter().enumerate() {
                    hw |= (bit as u16) << (15 - i);
                }
                out.extend_from_slice(&hw.to_le_bytes());
            }
            out.extend_from_slice(&[0; 8]);
            out
        }
    }

    /// Every AC code from the psx-spx table, as `(code bits without sign, value)`.
    fn all_codes() -> Vec<(String, u16)> {
        let mut v = vec![("11".to_string(), 0x0001), ("011".into(), 0x0401), ("00101".into(), 0x0003)];
        let groups: [(&str, usize, &[u16]); 11] = [
            ("010", 1, &[0x0002, 0x0801]),
            ("0011", 1, &[0x1001, 0x0C01]),
            ("00100", 3, &AC_Z2_100),
            ("0001", 2, &AC_Z3),
            ("00001", 2, &AC_Z4),
            ("0000001", 3, &AC_Z6),
            ("00000001", 4, &AC_Z7),
            ("000000001", 4, &AC_Z8),
            ("0000000001", 4, &AC_Z9),
            ("00000000001", 4, &AC_Z10),
            ("000000000001", 4, &AC_Z11),
        ];
        for (prefix, n, table) in groups {
            for (i, &val) in table.iter().enumerate() {
                v.push((format!("{prefix}{:0width$b}", i, width = n), val));
            }
        }
        v
    }

    fn frame(version: u16, quant: u16, w: &Writer) -> Vec<u8> {
        let mut f = vec![0, 0, 0x00, 0x38];
        f.extend_from_slice(&quant.to_le_bytes());
        f.extend_from_slice(&version.to_le_bytes());
        f.extend(w.bytes());
        f
    }

    #[test]
    fn every_ac_code_decodes_with_both_signs() {
        let codes = all_codes();
        assert_eq!(codes.len(), 3 + 2 + 2 + 8 + 4 + 4 + 8 + 16 * 5);
        for (code, val) in codes {
            for neg in [false, true] {
                let mut w = Writer::default();
                w.put(&code);
                w.put(if neg { "1" } else { "0" });
                let mut b = Bits { data: &w.bytes(), pos: 0 };
                let got = read_ac(&mut b).unwrap();
                assert_eq!(got, Ac::Value(signed(val, neg)), "code {code}");
                assert_eq!(b.pos, code.len() + 1, "length of {code}");
            }
        }
    }

    #[test]
    fn spot_checks_against_spec() {
        // 00100 010 1: index 2 = 3001h (run 12, level 1), negated -> level 3FFh.
        let mut w = Writer::default();
        w.put("00100 010 1");
        let mut b = Bits { data: &w.bytes(), pos: 0 };
        assert_eq!(read_ac(&mut b), Some(Ac::Value(0x33FF)));
        // Escape 000001 + raw 16 bits.
        let mut w = Writer::default();
        w.put("000001");
        w.num(0x0C3F, 16);
        let mut b = Bits { data: &w.bytes(), pos: 0 };
        assert_eq!(read_ac(&mut b), Some(Ac::Escape(0x0C3F)));
        // 12 zeros is invalid.
        let mut w = Writer::default();
        w.put("000000000000 1");
        assert_eq!(read_ac(&mut Bits { data: &w.bytes(), pos: 0 }), None);
    }

    #[test]
    fn v2_macroblock_to_rle() {
        // One 16x16 macroblock: Cr DC=5 with AC 11s(+1) then 011s(-1), others DC only.
        let mut w = Writer::default();
        w.num(5, 10);
        w.put("110 0111 10");
        for dc in [0x3FFu32, 1, 2, 3, 4] {
            w.num(dc, 10);
            w.put("10");
        }
        w.num(0x1FF, 10); // end of frame
        let f = frame(2, 7, &w);
        let (h, rle) = decode_bs(&f, 16, 16).unwrap();
        assert_eq!(h.version, 2);
        let q = 7 << 10;
        assert_eq!(
            rle.halfwords,
            vec![q | 5, 0x0001, 0x07FF, EOB, q | 0x3FF, EOB, q | 1, EOB, q | 2, EOB, q | 3, EOB, q | 4, EOB]
        );
        assert!(rle.end_code);
    }

    #[test]
    fn v3_dc_prediction() {
        // Cr: +1*4 via "01 1"; Cb: 0 via "00"; Y1: -3*4 via luma "01 00" (size 2, s=0, n=0 -> -3);
        // Y2: +2*4 via "01 10"; Y3: 0 "100"; Y4: +1*4 "00 1".
        let mut w = Writer::default();
        for code in ["011", "00", "0100", "0110", "100", "001"] {
            w.put(code);
            w.put("10");
        }
        w.put("1111111111");
        let f = frame(3, 1, &w);
        let (_, rle) = decode_bs(&f, 16, 16).unwrap();
        let dcs: Vec<u16> = rle.halfwords.iter().copied().filter(|&v| v != EOB).map(|v| v & 0x3FF).collect();
        // Y predictor runs -12, -4, -4, 0.
        let wrap = |v: i32| (v & 0x3FF) as u16;
        assert_eq!(dcs, vec![4, 0, wrap(-12), wrap(-4), wrap(-4), 0]);
        assert!(rle.end_code);
    }

    #[test]
    fn unsupported_variants_are_named() {
        let w = Writer::default();
        assert_eq!(
            decode_bs(&frame(0x00F0, 0x140, &w), 320, 240).unwrap_err(),
            BsError::Unsupported("iki_or_encrypted".into())
        );
        let mut f = frame(2, 1, &w);
        f[2] = 0x12;
        assert_eq!(decode_bs(&f, 16, 16).unwrap_err(), BsError::Unsupported("not_bs".into()));
    }

    fn sector(frame_no: u32, chunk: u16, chunks: u16, fill: u8) -> Vec<u8> {
        let mut d = vec![fill; 2048];
        d[..32].fill(0);
        d[0..2].copy_from_slice(&0x0160u16.to_le_bytes());
        d[2..4].copy_from_slice(&0x8001u16.to_le_bytes());
        d[4..6].copy_from_slice(&chunk.to_le_bytes());
        d[6..8].copy_from_slice(&chunks.to_le_bytes());
        d[8..12].copy_from_slice(&frame_no.to_le_bytes());
        d[16..18].copy_from_slice(&320u16.to_le_bytes());
        d[18..20].copy_from_slice(&240u16.to_le_bytes());
        d
    }

    #[test]
    fn chunks_reassemble_out_of_order_with_gaps() {
        let mut a = FrameAssembler::default();
        let mut frames = Vec::new();
        // Frame 1: chunks 1, 0, 2 (audio sectors in between are simply not pushed).
        for (lba, chunk) in [(10, 1u16), (11, 0), (13, 2)] {
            let s = sector(1, chunk, 3, 0x10 + chunk as u8);
            frames.extend(a.push(lba, &StrHeader::parse(&s).unwrap(), &s));
        }
        assert_eq!(frames.len(), 1);
        // Frame 2 loses chunk 1; frame 3 starts and flushes it.
        for (f, chunk) in [(2u32, 0u16), (3, 0), (3, 1)] {
            let s = sector(f, chunk, 2, 0x20 + chunk as u8);
            frames.extend(a.push(20 + chunk as u32, &StrHeader::parse(&s).unwrap(), &s));
        }
        assert!(a.finish().is_none());
        assert_eq!(frames.iter().map(|f| f.number).collect::<Vec<_>>(), vec![1, 2, 3]);
        let f1 = &frames[0];
        assert_eq!((f1.first_lba, f1.last_lba, f1.data.len()), (10, 13, 3 * 2016));
        assert!(f1.data[..2016].iter().all(|&b| b == 0x10) && f1.data[4032..].iter().all(|&b| b == 0x12));
        assert_eq!(frames[1].missing, vec![1]);
        assert!(frames[2].missing.is_empty());
    }
}

//! CD-XA ADPCM audio (psx-spx "CDROM XA Audio ADPCM Compression").
//!
//! A Form 2 audio sector carries 18 sound groups of 128 bytes: a 16-byte
//! header block (unit headers at bytes 4..12) and 28 words of sample data.
//! 4-bit audio has 8 units of 28 samples per group, 8-bit has 4. In stereo,
//! even units are left and odd units right. Output is resampled to 44.1 kHz
//! (linear interpolation; the hardware's zig-zag filter is not modelled).

use std::collections::VecDeque;

const POS: [i32; 4] = [0, 60, 115, 98];
const NEG: [i32; 4] = [0, 0, -52, -55];

/// Coding byte (subheader byte 3) fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coding {
    pub stereo: bool,
    pub rate: u32,
    pub bits: u32,
}

impl Coding {
    pub fn from_byte(c: u8) -> Self {
        Coding {
            stereo: c & 3 == 1,
            rate: if (c >> 2) & 3 == 1 { 18_900 } else { 37_800 },
            bits: if (c >> 4) & 3 == 1 { 8 } else { 4 },
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct XaDecoder {
    /// ADPCM history `(old, older)` per channel.
    hist: [(i32, i32); 2],
    /// Resampler: previous input frame and 16.16 position past it.
    prev: (i16, i16),
    frac: u32,
}

fn decode_unit(group: &[u8], unit: usize, bits: u32, hist: &mut (i32, i32), out: &mut Vec<i16>) {
    let header = group[4 + unit];
    let filter = ((header >> 4) & 3) as usize;
    let range = (header & 0xF) as u32;
    let range = if range > 12 { 9 } else { range };
    for j in 0..28 {
        let t = if bits == 4 {
            let byte = group[16 + unit / 2 + j * 4];
            (((byte >> ((unit & 1) * 4)) << 4) as i8 as i32) << 8 // nibble in bits 12..15
        } else {
            (group[16 + unit + j * 4] as i8 as i32) << 8 // byte in bits 8..15
        };
        let t = t >> range;
        let s = (t + (hist.0 * POS[filter] + hist.1 * NEG[filter] + 32) / 64).clamp(-0x8000, 0x7FFF);
        *hist = (s, hist.0);
        out.push(s as i16);
    }
}

impl XaDecoder {
    /// Decode one raw 2352-byte sector into native-rate frames.
    pub fn decode(&mut self, raw: &[u8], coding: Coding) -> Vec<(i16, i16)> {
        let units = if coding.bits == 4 { 8 } else { 4 };
        let mut left = Vec::with_capacity(18 * 8 * 28);
        let mut right = Vec::with_capacity(18 * 4 * 28);
        for g in 0..18 {
            let group = &raw[24 + g * 128..24 + (g + 1) * 128];
            for unit in 0..units {
                if coding.stereo && unit & 1 == 1 {
                    decode_unit(group, unit, coding.bits, &mut self.hist[1], &mut right);
                } else {
                    decode_unit(group, unit, coding.bits, &mut self.hist[0], &mut left);
                }
            }
        }
        if coding.stereo {
            left.into_iter().zip(right).collect()
        } else {
            left.into_iter().map(|s| (s, s)).collect()
        }
    }

    /// Resample native-rate frames to 44.1 kHz onto `out`.
    pub fn resample(&mut self, frames: &[(i16, i16)], rate: u32, out: &mut VecDeque<(i16, i16)>) {
        let step = ((rate as u64) << 16) / 44_100;
        for &cur in frames {
            while self.frac < 1 << 16 {
                let f = self.frac as i32;
                let lerp = |a: i16, b: i16| (a as i32 + (((b as i32 - a as i32) * f) >> 16)) as i16;
                out.push_back((lerp(self.prev.0, cur.0), lerp(self.prev.1, cur.1)));
                self.frac += step as u32;
            }
            self.frac -= 1 << 16;
            self.prev = cur;
        }
    }

    pub fn reset(&mut self) {
        *self = XaDecoder::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_fields() {
        assert_eq!(Coding::from_byte(0x01), Coding { stereo: true, rate: 37_800, bits: 4 });
        assert_eq!(Coding::from_byte(0x14), Coding { stereo: false, rate: 18_900, bits: 8 });
    }

    #[test]
    fn decodes_constant_nibbles_and_resamples() {
        // Every unit: filter 0, range 12 (shift 0) and nibble 3 -> sample 3.
        let mut raw = vec![0u8; 2352];
        for g in 0..18 {
            let group = &mut raw[24 + g * 128..24 + (g + 1) * 128];
            group[4..12].fill(0x0C);
            group[16..].fill(0x33);
        }
        let mut d = XaDecoder::default();
        let frames = d.decode(&raw, Coding::from_byte(0x01));
        assert_eq!(frames.len(), 18 * 4 * 28);
        assert!(frames.iter().all(|&f| f == (3, 3)));
        let mut out = VecDeque::new();
        d.resample(&frames, 37_800, &mut out);
        // 37.8 kHz -> 44.1 kHz is 7/6.
        assert!((out.len() as i64 - frames.len() as i64 * 7 / 6).abs() <= 1, "{}", out.len());
        assert!(out.iter().skip(2).all(|&f| f == (3, 3)));
    }
}

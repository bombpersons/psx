//! CD-XA ADPCM audio (psx-spx "CDROM XA Audio ADPCM Compression").
//!
//! A Form 2 audio sector carries 18 sound groups of 128 bytes: a 16-byte
//! header block (unit headers at bytes 4..12) and 28 words of sample data.
//! 4-bit audio has 8 units of 28 samples per group, 8-bit has 4. In stereo,
//! even units are left and odd units right. Output is resampled to 44.1 kHz
//! by the hardware's 25-point zig-zag interpolator (psx-spx "25-point Zigzag
//! Interpolation"): every 37.8 kHz sample enters a 32-entry ring, and every
//! sixth produces seven outputs through seven 29-tap tables. 18.9 kHz audio
//! enters the ring twice per sample (DuckStation's reading of psx-spx's
//! "two-step theory"; psx-spx doesn't settle it).

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

/// The zig-zag interpolation tables (psx-spx, Table1..Table7, taps 1..29).
#[rustfmt::skip]
const ZIGZAG: [[i32; 29]; 7] = [
    [0, 0, 0, 0, 0, -0x0002, 0x000A, -0x0022, 0x0041, -0x0054, 0x0034, 0x0009, -0x010A, 0x0400, -0x0A78,
     0x234C, 0x6794, -0x1780, 0x0BCD, -0x0623, 0x0350, -0x016D, 0x006B, 0x000A, -0x0010, 0x0011, -0x0008, 0x0003, -0x0001],
    [0, 0, 0, -0x0002, 0, 0x0003, -0x0013, 0x003C, -0x004B, 0x00A2, -0x00E3, 0x0132, -0x0043, -0x0267, 0x0C9D,
     0x74BB, -0x11B4, 0x09B8, -0x05BF, 0x0372, -0x01A8, 0x00A6, -0x001B, 0x0005, 0x0006, -0x0008, 0x0003, -0x0001, 0],
    [0, 0, -0x0001, 0x0003, -0x0002, -0x0005, 0x001F, -0x004A, 0x00B3, -0x0192, 0x02B1, -0x039E, 0x04F8, -0x05A6, 0x7939,
     -0x05A6, 0x04F8, -0x039E, 0x02B1, -0x0192, 0x00B3, -0x004A, 0x001F, -0x0005, -0x0002, 0x0003, -0x0001, 0, 0],
    [0, -0x0001, 0x0003, -0x0008, 0x0006, 0x0005, -0x001B, 0x00A6, -0x01A8, 0x0372, -0x05BF, 0x09B8, -0x11B4, 0x74BB, 0x0C9D,
     -0x0267, -0x0043, 0x0132, -0x00E3, 0x00A2, -0x004B, 0x003C, -0x0013, 0x0003, 0, -0x0002, 0, 0, 0],
    [-0x0001, 0x0003, -0x0008, 0x0011, -0x0010, 0x000A, 0x006B, -0x016D, 0x0350, -0x0623, 0x0BCD, -0x1780, 0x6794, 0x234C, -0x0A78,
     0x0400, -0x010A, 0x0009, 0x0034, -0x0054, 0x0041, -0x0022, 0x000A, -0x0001, 0, 0x0001, 0, 0, 0],
    [0x0002, -0x0008, 0x0010, -0x0023, 0x002B, 0x001A, -0x00EB, 0x027B, -0x0548, 0x0AFA, -0x16FA, 0x53E0, 0x3C07, -0x1249, 0x080E,
     -0x0347, 0x015B, -0x0044, -0x0017, 0x0046, -0x0023, 0x0011, -0x0005, 0, 0, 0, 0, 0, 0],
    [-0x0005, 0x0011, -0x0023, 0x0046, -0x0017, -0x0044, 0x015B, -0x0347, 0x080E, -0x1249, 0x3C07, 0x53E0, -0x16FA, 0x0AFA, -0x0548,
     0x027B, -0x00EB, 0x001A, 0x002B, -0x0023, 0x0010, -0x0008, 0x0002, 0, 0, 0, 0, 0, 0],
];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct XaDecoder {
    /// ADPCM history `(old, older)` per channel.
    hist: [(i32, i32); 2],
    /// Zig-zag resampler: the last 32 native-rate samples per side, the next
    /// write position, and the six-step counter.
    ring: [[i16; 32]; 2],
    pos: usize,
    sixstep: u8,
}

impl Default for XaDecoder {
    fn default() -> Self {
        // The six-step counter powers up "uninitialized random"; start a full step.
        XaDecoder { hist: [(0, 0); 2], ring: [[0; 32]; 2], pos: 0, sixstep: 6 }
    }
}

/// One zig-zag output: `sum(ring[p - i] * table[i]) >> 15` over taps 1..29, clamped.
fn zigzag(ring: &[i16; 32], pos: usize, table: &[i32; 29]) -> i16 {
    let mut sum = 0i32;
    for (i, &c) in table.iter().enumerate() {
        sum += (ring[(pos + 32 - 1 - i) & 31] as i32 * c) >> 15;
    }
    sum.clamp(-0x8000, 0x7FFF) as i16
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
        if coding.stereo { left.into_iter().zip(right).collect() } else { left.into_iter().map(|s| (s, s)).collect() }
    }

    /// Resample native-rate frames to 44.1 kHz onto `out` (seven outputs per six inputs).
    pub fn resample(&mut self, frames: &[(i16, i16)], rate: u32, out: &mut VecDeque<(i16, i16)>) {
        let repeat = if rate == 18_900 { 2 } else { 1 };
        for &(l, r) in frames {
            for _ in 0..repeat {
                self.ring[0][self.pos] = l;
                self.ring[1][self.pos] = r;
                self.pos = (self.pos + 1) & 31;
                self.sixstep -= 1;
                if self.sixstep == 0 {
                    self.sixstep = 6;
                    for t in &ZIGZAG {
                        out.push_back((zigzag(&self.ring[0], self.pos, t), zigzag(&self.ring[1], self.pos, t)));
                    }
                }
            }
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
        // 37.8 kHz -> 44.1 kHz is exactly 7/6 (a sector holds a multiple of six samples).
        assert_eq!(out.len(), frames.len() * 7 / 6);
    }

    #[test]
    fn zigzag_tables_match_psx_spx() {
        // Spot checks against psx-spx's table listing (index 1..29 down each column).
        assert_eq!((ZIGZAG[0][0], ZIGZAG[0][13], ZIGZAG[0][16]), (0, 0x0400, 0x6794));
        assert_eq!((ZIGZAG[2][14], ZIGZAG[6][11], ZIGZAG[6][0]), (0x7939, 0x53E0, -0x0005));
        // Table 3 is symmetric about its centre tap; tables 1/5, 2/4 and 6/7 mirror each other.
        for i in 0..29 {
            assert_eq!(ZIGZAG[2][i], ZIGZAG[2][28 - i]);
        }
        // The taps sum to 73E5h..741Dh: a DC gain of about 0.906 (-0.86 dB), not unity.
        for t in &ZIGZAG {
            let sum: i32 = t.iter().sum();
            assert!((0x73E5..=0x741D).contains(&sum), "{sum:#x}");
        }
    }

    #[test]
    fn zigzag_dc_gain_and_flat_passband() {
        let mut d = XaDecoder::default();
        let mut out = VecDeque::new();
        // DC: once the 29 taps are filled, the level settles at the tables' gain (about 0.906),
        // with the small per-phase ripple psx-spx calls "22050Hz noise".
        d.resample(&vec![(1000, -1000); 600], 37_800, &mut out);
        let tail: Vec<_> = out.iter().skip(60).collect();
        assert!(tail.iter().all(|&&(l, r)| (885..=925).contains(&l) && (-925..=-885).contains(&r)), "{:?}", &tail[..7]);
        // The passband is flat to about 15 kHz (linear interpolation loses ~2 dB by 10 kHz).
        // Amplitude of the tone in the 44.1 kHz output, by projection onto it.
        let tone = |f: f64| -> f64 {
            let mut d = XaDecoder::default();
            let mut out = VecDeque::new();
            let input: Vec<(i16, i16)> = (0..18_900)
                .map(|n| {
                    let s = (10_000.0 * (2.0 * std::f64::consts::PI * f * n as f64 / 37_800.0).sin()) as i16;
                    (s, s)
                })
                .collect();
            d.resample(&input, 37_800, &mut out);
            let y: Vec<f64> = out.iter().skip(500).map(|&(l, _)| l as f64).collect();
            let w = 2.0 * std::f64::consts::PI * f / 44_100.0;
            let (re, im) = y
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(re, im), (m, &v)| (re + v * (w * m as f64).cos(), im + v * (w * m as f64).sin()));
            2.0 * (re * re + im * im).sqrt() / y.len() as f64
        };
        let (low, mid) = (tone(1000.0), tone(10_000.0));
        assert!((8_900.0..=9_200.0).contains(&low), "{low}");
        assert!((mid / low - 1.0).abs() < 0.02, "{low} {mid}");
    }

    #[test]
    fn half_rate_audio_enters_the_ring_twice() {
        let mut d = XaDecoder::default();
        let mut out = VecDeque::new();
        d.resample(&vec![(500, 500); 18 * 4 * 28], 18_900, &mut out);
        // 18.9 kHz -> 44.1 kHz is 7/3.
        assert_eq!(out.len(), 18 * 4 * 28 * 7 / 3);
    }
}

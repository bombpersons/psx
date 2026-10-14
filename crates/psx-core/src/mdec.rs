//! Macroblock decoder (psx-spx "Macroblock Decoder (MDEC)").
//!
//! Modelled: MDEC0 command/parameter port and data-out port, MDEC1 control and
//! status, the three commands (decode macroblocks, set quant tables, set scale
//! table), RLE decode with zig-zag dequantisation, the two-pass IDCT using the
//! loaded scale table, YUV→RGB in 15/24-bit (colour) and 4/8-bit (mono) depths,
//! and DMA in (channel 0) / out (channel 1). Decoding is instantaneous: the
//! busy and request timing of real hardware is not modelled.

use std::collections::VecDeque;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
    54, 47, 55, 62, 63,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    Bit4 = 0,
    Bit8 = 1,
    Bit24 = 2,
    Bit15 = 3,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Mdec {
    /// Words still expected for the current command, and the command word.
    remaining: u32,
    command: u32,
    params: Vec<u32>,
    out: VecDeque<u32>,
    #[serde(with = "crate::state::big_array")]
    lum_q: [u8; 64],
    #[serde(with = "crate::state::big_array")]
    col_q: [u8; 64],
    #[serde(with = "crate::state::big_array")]
    scale: [i16; 64],
    dma_in: bool,
    dma_out: bool,
    /// Macroblocks decoded since reset.
    pub macroblocks: u64,
}

impl Default for Mdec {
    fn default() -> Self {
        Mdec {
            remaining: 0,
            command: 0,
            params: Vec::new(),
            out: VecDeque::new(),
            lum_q: [1; 64],
            col_q: [1; 64],
            scale: [0; 64],
            dma_in: false,
            dma_out: false,
            macroblocks: 0,
        }
    }
}

fn sign10(v: u16) -> i32 {
    ((v as i32) << 22) >> 22
}

impl Mdec {
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0 => self.out.pop_front().unwrap_or(0),
            _ => self.status(),
        }
    }

    fn status(&self) -> u32 {
        let mut s = 0u32;
        if self.out.is_empty() {
            s |= 1 << 31;
        }
        if self.remaining > 0 {
            s |= 1 << 29;
        }
        if self.dma_in && self.remaining > 0 {
            s |= 1 << 28;
        }
        if self.dma_out && !self.out.is_empty() {
            s |= 1 << 27;
        }
        s |= ((self.command >> 25) & 0xF) << 23; // depth, signed, bit15
        s |= self.remaining.wrapping_sub(1) & 0xFFFF;
        s
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0 => self.write_word(v),
            _ => {
                if v & (1 << 31) != 0 {
                    let keep = (self.lum_q, self.col_q, self.scale, self.macroblocks);
                    *self = Mdec::default();
                    (self.lum_q, self.col_q, self.scale, self.macroblocks) = keep;
                }
                self.dma_in = v & (1 << 30) != 0;
                self.dma_out = v & (1 << 29) != 0;
            }
        }
    }

    /// Word written to MDEC0 by the CPU or DMA channel 0.
    pub fn write_word(&mut self, v: u32) {
        if self.remaining == 0 {
            self.command = v;
            self.params.clear();
            self.remaining = match v >> 29 {
                1 => v & 0xFFFF,
                2 => if v & 1 != 0 { 32 } else { 16 },
                3 => 32,
                _ => 0,
            };
            return;
        }
        self.params.push(v);
        self.remaining -= 1;
        if self.remaining == 0 {
            self.execute();
        }
    }

    /// Output words ready for DMA channel 1.
    pub fn out_available(&self) -> usize {
        self.out.len()
    }

    /// Next output word for DMA channel 1.
    pub fn dma_out_word(&mut self) -> u32 {
        self.out.pop_front().unwrap_or(0)
    }

    fn execute(&mut self) {
        let params = std::mem::take(&mut self.params);
        let bytes: Vec<u8> = params.iter().flat_map(|w| w.to_le_bytes()).collect();
        match self.command >> 29 {
            1 => self.decode(&params),
            2 => {
                self.lum_q.copy_from_slice(&bytes[..64]);
                if self.command & 1 != 0 {
                    self.col_q.copy_from_slice(&bytes[64..128]);
                }
            }
            3 => {
                for (i, s) in self.scale.iter_mut().enumerate() {
                    *s = i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
                }
            }
            _ => {}
        }
    }

    /// RLE-decode one 8×8 block (psx-spx `decode_block`); false at end of input.
    /// The 0xFE00 end code has run 63, which pushes `k` past 63 and ends the block.
    fn decode_block(&self, halves: &mut std::iter::Peekable<impl Iterator<Item = u16>>, q: &[u8; 64], blk: &mut [i32; 64]) -> bool {
        *blk = [0; 64];
        let mut n = loop {
            match halves.next() {
                None => return false,
                Some(0xFE00) => continue, // padding between macroblocks
                Some(v) => break v,
            }
        };
        let q_scale = ((n >> 10) & 0x3F) as i32;
        let mut k = 0usize;
        let mut val = sign10(n) * q[0] as i32;
        loop {
            if q_scale == 0 {
                val = sign10(n) * 2;
            }
            let v = val.clamp(-0x400, 0x3FF);
            if q_scale > 0 {
                blk[ZIGZAG[k]] = v;
            } else {
                blk[k] = v;
            }
            let Some(next) = halves.next() else { break };
            n = next;
            k += ((n >> 10) & 0x3F) as usize + 1;
            if k > 63 {
                break;
            }
            val = (sign10(n) * q[k] as i32 * q_scale + 4) / 8;
        }
        self.idct(blk);
        true
    }

    fn idct(&self, blk: &mut [i32; 64]) {
        let mut tmp = [0i32; 64];
        let (mut src, mut dst) = (*blk, [0i32; 64]);
        for _ in 0..2 {
            for x in 0..8 {
                for y in 0..8 {
                    let mut sum: i64 = 0;
                    for z in 0..8 {
                        sum += src[y + z * 8] as i64 * (self.scale[x + z * 8] as i64 / 8);
                    }
                    dst[x + y * 8] = ((sum + 0xFFF) / 0x2000) as i32;
                }
            }
            std::mem::swap(&mut src, &mut dst);
        }
        tmp.copy_from_slice(&src);
        *blk = tmp;
    }

    fn decode(&mut self, params: &[u32]) {
        let depth = match (self.command >> 27) & 3 {
            0 => Depth::Bit4,
            1 => Depth::Bit8,
            2 => Depth::Bit24,
            _ => Depth::Bit15,
        };
        let signed = self.command & (1 << 26) != 0;
        let bit15 = if self.command & (1 << 25) != 0 { 0x8000u32 } else { 0 };
        let mut halves = params.iter().flat_map(|w| [*w as u16, (*w >> 16) as u16]).peekable();
        let (lq, cq) = (self.lum_q, self.col_q);
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            match depth {
                Depth::Bit4 | Depth::Bit8 => {
                    let mut y = [0i32; 64];
                    if !self.decode_block(&mut halves, &lq, &mut y) {
                        break;
                    }
                    self.macroblocks += 1;
                    let px: Vec<u8> = y
                        .iter()
                        .map(|&v| {
                            let v = v.clamp(-128, 127);
                            if signed { v as i8 as u8 } else { (v + 128) as u8 }
                        })
                        .collect();
                    if depth == Depth::Bit8 {
                        bytes.extend_from_slice(&px);
                    } else {
                        bytes.extend(px.chunks(2).map(|p| (p[0] >> 4) | (p[1] & 0xF0)));
                    }
                }
                _ => {
                    let (mut cr, mut cb) = ([0i32; 64], [0i32; 64]);
                    if !self.decode_block(&mut halves, &cq, &mut cr) || !self.decode_block(&mut halves, &cq, &mut cb) {
                        break;
                    }
                    let mut mb = [[0u8; 3]; 256];
                    for (yb, (xx, yy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].into_iter().enumerate() {
                        let mut y = [0i32; 64];
                        if !self.decode_block(&mut halves, &lq, &mut y) {
                            break;
                        }
                        let _ = yb;
                        for py in 0..8 {
                            for pxx in 0..8 {
                                let ci = (pxx + xx) / 2 + ((py + yy) / 2) * 8;
                                let (r, b) = (cr[ci] as f64, cb[ci] as f64);
                                let g = -0.3437 * b - 0.7143 * r;
                                let (r, b) = (1.402 * r, 1.772 * b);
                                let lum = y[pxx + py * 8] as f64;
                                let c = |v: f64| {
                                    let v = (lum + v).round().clamp(-128.0, 127.0) as i32;
                                    if signed { v as i8 as u8 } else { (v + 128) as u8 }
                                };
                                mb[(pxx + xx) + (py + yy) * 16] = [c(r), c(g), c(b)];
                            }
                        }
                    }
                    self.macroblocks += 1;
                    for p in mb {
                        if depth == Depth::Bit24 {
                            bytes.extend_from_slice(&p);
                        } else {
                            let v = (p[0] as u32 >> 3) | ((p[1] as u32 >> 3) << 5) | ((p[2] as u32 >> 3) << 10) | bit15;
                            bytes.extend_from_slice(&(v as u16).to_le_bytes());
                        }
                    }
                }
            }
        }
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        self.out.extend(bytes.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_block_decodes_to_flat_grey() {
        let mut m = Mdec::default();
        // Scale table: an orthonormal-ish IDCT with only DC weights (0x5A82 ≈ sqrt(1/8)<<15).
        m.write_word(3 << 29);
        for _ in 0..32 {
            m.write_word(0x5A82_5A82);
        }
        m.write_word(2 << 29); // luminance quant table only
        for _ in 0..16 {
            m.write_word(0x0101_0101);
        }
        // Mono 8-bit decode of one block: DC=0 with qscale 1, then end of block.
        m.write_word((1 << 29) | (1 << 27) | 1);
        m.write_word((0xFE00 << 16) | (1 << 10));
        assert_eq!(m.macroblocks, 1);
        let out: Vec<u32> = (0..16).map(|_| m.dma_out_word()).collect();
        assert!(out.iter().all(|&w| w == 0x8080_8080), "{out:08x?}");
    }
}

//! Macroblock decoder, ported from Mednafen's `mdec.cpp`
//! (`crates/psx-oracle/mednafen/src/psx/mdec.cpp`).
//!
//! Words go through a 32-word input FIFO and come out of a 32-word output
//! FIFO. The decoder is a state machine that runs on the DMA controller's
//! updates (with at most 128 clocks banked) and whenever a FIFO is written or
//! read, stopping where it would wait: for input, for room in the output, or
//! for clocks. A command costs a clock and each finished 8×8 block 512. The
//! IDCT is two fixed-point passes through the loaded matrix (stored shifted
//! right by 3), colour conversion uses Mednafen's integer formulas, and
//! DMA channel 1 writes each output word at an offset that lays the 8-pixel
//! rows of a block out as a 16-pixel-wide macroblock.

use std::collections::VecDeque;

const FIFO: usize = 0x20;

const ZIGZAG: [usize; 64] = [
    0x00, 0x08, 0x01, 0x02, 0x09, 0x10, 0x18, 0x11, 0x0a, 0x03, 0x04, 0x0b, 0x12, 0x19, 0x20, 0x28, 0x21, 0x1a, 0x13,
    0x0c, 0x05, 0x06, 0x0d, 0x14, 0x1b, 0x22, 0x29, 0x30, 0x38, 0x31, 0x2a, 0x23, 0x1c, 0x15, 0x0e, 0x07, 0x0f, 0x16,
    0x1d, 0x24, 0x2b, 0x32, 0x39, 0x3a, 0x33, 0x2c, 0x25, 0x1e, 0x17, 0x1f, 0x26, 0x2d, 0x34, 0x3b, 0x3c, 0x35, 0x2e,
    0x27, 0x2f, 0x36, 0x3d, 0x3e, 0x37, 0x3f,
];

/// Where `run` stopped (Mednafen's `MDRPhase`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Phase {
    /// Waiting for a command word.
    Command,
    /// The command's clock.
    CommandClock,
    DecodeRead,
    DecodeClocks,
    DecodeOut,
    QuantRead,
    IdctRead,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Mdec {
    clock_counter: i32,
    phase: Phase,
    input: VecDeque<u32>,
    output: VecDeque<u32>,
    #[serde(with = "crate::state::big_array")]
    block_y: [i8; 64],
    #[serde(with = "crate::state::big_array")]
    block_cb: [i8; 64],
    #[serde(with = "crate::state::big_array")]
    block_cr: [i8; 64],
    control: u32,
    command: u32,
    in_command: bool,
    #[serde(with = "crate::state::big_array")]
    qmatrix: [u8; 128],
    qm_index: u32,
    #[serde(with = "crate::state::big_array")]
    idct_matrix: [i16; 64],
    idct_index: u32,
    qscale: u8,
    #[serde(with = "crate::state::big_array")]
    coeff: [i16; 64],
    coeff_index: usize,
    decode_wb: u32,
    #[serde(with = "crate::state::big_array")]
    pixels: [u8; 192],
    pixel_read: usize,
    /// Words in `pixels`.
    pixel_count: usize,
    in_counter: u16,
    ram_offset_y: u8,
    ram_offset_counter: u8,
    ram_offset_wws: u8,
}

impl Default for Mdec {
    fn default() -> Self {
        Mdec {
            clock_counter: 0,
            phase: Phase::Command,
            input: VecDeque::with_capacity(FIFO),
            output: VecDeque::with_capacity(FIFO),
            block_y: [0; 64],
            block_cb: [0; 64],
            block_cr: [0; 64],
            control: 0,
            command: 0,
            in_command: false,
            qmatrix: [0; 128],
            qm_index: 0,
            idct_matrix: [0; 64],
            idct_index: 0,
            qscale: 0,
            coeff: [0; 64],
            coeff_index: 0,
            decode_wb: 0,
            pixels: [0; 192],
            pixel_read: 0,
            pixel_count: 0,
            in_counter: 0,
            ram_offset_y: 0,
            ram_offset_counter: 0,
            ram_offset_wws: 0,
        }
    }
}

fn sign10(v: u32) -> i32 {
    ((v as i32) << 22) >> 22
}

/// Mednafen's `Mask9ClampS8`: the low 9 bits, signed, clamped to a byte.
fn mask9_clamp(v: i32) -> i8 {
    (((v << 23) >> 23).clamp(-128, 127)) as i8
}

fn ycbcr_to_rgb(y: i8, cb: i8, cr: i8) -> [u8; 3] {
    let (y, cb, cr) = (y as i32, cb as i32, cr as i32);
    let r = mask9_clamp(y + ((359 * cr + 0x80) >> 8));
    let g = mask9_clamp(y + ((((-88 * cb) & !0x1F) + ((-183 * cr) & !0x07) + 0x80) >> 8));
    let b = mask9_clamp(y + ((454 * cb + 0x80) >> 8));
    [r as u8 ^ 0x80, g as u8 ^ 0x80, b as u8 ^ 0x80]
}

fn rgb555(p: [u8; 3]) -> u16 {
    let c = |v: u8| ((v as u16 + 4) >> 3).min(0x1F);
    c(p[0]) | c(p[1]) << 5 | c(p[2]) << 10
}

impl Mdec {
    /// One pass of the IDCT (Mednafen's `IDCT_1D_Multi`): row `col` of the
    /// input times row `x` of the matrix, rounded.
    fn idct_pass(&self, input: &[i16; 64], mut put: impl FnMut(usize, usize, i32)) {
        for col in 0..8 {
            for x in 0..8 {
                let sum = (0..8).fold(0i32, |s, u| {
                    s.wrapping_add((input[col * 8 + u] as i32).wrapping_mul(self.idct_matrix[x * 8 + u] as i32))
                });
                put(col, x, sum.wrapping_add(0x4000) >> 15);
            }
        }
    }

    fn idct(&self) -> [i8; 64] {
        let mut tmp = [0i16; 64];
        self.idct_pass(&self.coeff, |col, x, v| tmp[x * 8 + col] = v as i16);
        let mut out = [0i8; 64];
        self.idct_pass(&tmp, |col, x, v| out[col * 8 + x] = mask9_clamp(v));
        out
    }

    /// Fill the pixel buffer from the blocks (Mednafen's `EncodeImage`); `ybn`
    /// is which of the four luminance blocks of a colour macroblock this is.
    fn encode_image(&mut self, ybn: usize) {
        let signed = self.command & (1 << 26) != 0;
        let chroma =
            |b: &[i8; 64], y: usize, x: usize| b[((y >> 1) | ((ybn & 2) << 1)) * 8 + ((ybn & 1) << 2) + (x >> 1)];
        match (self.command >> 27) & 3 {
            0 => {
                let xor = if signed { 0 } else { 0x88 };
                for i in 0..32 {
                    let p0 = (self.block_y[2 * i] as i32 + 8).min(127) as u8;
                    let p1 = (self.block_y[2 * i + 1] as i32 + 8).min(127) as u8;
                    self.pixels[i] = ((p0 >> 4) | (p1 & 0xF0)) ^ xor;
                }
                self.pixel_count = 8;
            }
            1 => {
                let xor = if signed { 0 } else { 0x80 };
                for i in 0..64 {
                    self.pixels[i] = self.block_y[i] as u8 ^ xor;
                }
                self.pixel_count = 16;
            }
            2 => {
                let xor = if signed { 0x80 } else { 0 };
                for y in 0..8 {
                    for x in 0..8 {
                        let rgb = ycbcr_to_rgb(
                            self.block_y[y * 8 + x],
                            chroma(&self.block_cb, y, x),
                            chroma(&self.block_cr, y, x),
                        );
                        let o = (y * 8 + x) * 3;
                        for (d, c) in self.pixels[o..o + 3].iter_mut().zip(rgb) {
                            *d = c ^ xor;
                        }
                    }
                }
                self.pixel_count = 48;
            }
            _ => {
                let xor = if self.command & (1 << 25) != 0 { 0x8000 } else { 0 } | if signed { 0x4210 } else { 0 };
                for y in 0..8 {
                    for x in 0..8 {
                        let rgb = ycbcr_to_rgb(
                            self.block_y[y * 8 + x],
                            chroma(&self.block_cb, y, x),
                            chroma(&self.block_cr, y, x),
                        );
                        let o = (y * 8 + x) * 2;
                        self.pixels[o..o + 2].copy_from_slice(&(xor ^ rgb555(rgb)).to_le_bytes());
                    }
                }
                self.pixel_count = 32;
            }
        }
    }

    /// One halfword of macroblock data (Mednafen's `WriteImageData`); returns
    /// the clocks it costs.
    fn write_image_data(&mut self, v: u32) -> i32 {
        let v = v & 0xFFFF;
        let qm = if self.decode_wb < 2 { 64 } else { 0 };
        let quant = |ci: i32, q: i32, shift: u32| -> i16 {
            let tmp = if q != 0 {
                ((((ci * q) >> shift) as u32) << 4) as i32
                    + if ci == 0 {
                        0
                    } else if ci < 0 {
                        8
                    } else {
                        -8
                    }
            } else {
                ((ci * 2) as u32 as i32) << 4
            };
            tmp.clamp(-0x4000, 0x3FFF) as i16
        };
        if self.coeff_index == 0 {
            if v == 0xFE00 {
                return 0;
            }
            self.qscale = (v >> 10) as u8;
            self.coeff[ZIGZAG[0]] = quant(sign10(v), self.qmatrix[qm] as i32, 0);
            self.coeff_index = 1;
        } else if v == 0xFE00 {
            while self.coeff_index < 64 {
                self.coeff[ZIGZAG[self.coeff_index]] = 0;
                self.coeff_index += 1;
            }
        } else {
            for _ in 0..v >> 10 {
                if self.coeff_index >= 64 {
                    break;
                }
                self.coeff[ZIGZAG[self.coeff_index]] = 0;
                self.coeff_index += 1;
            }
            if self.coeff_index < 64 {
                let q = self.qscale as i32 * self.qmatrix[qm + self.coeff_index] as i32;
                self.coeff[ZIGZAG[self.coeff_index]] = quant(sign10(v), q, 3);
                self.coeff_index += 1;
            }
        }
        if self.coeff_index < 64 {
            return 0;
        }
        self.coeff_index = 0;
        let block = self.idct();
        match self.decode_wb {
            0 => self.block_cr = block,
            1 => self.block_cb = block,
            _ => self.block_y = block,
        }
        if self.decode_wb >= 2 {
            self.encode_image(((self.decode_wb + 4) % 6) as usize);
        }
        let colour = (self.command >> 27) & 2 != 0;
        self.decode_wb += 1;
        if self.decode_wb == if colour { 6 } else { 3 } {
            self.decode_wb = if colour { 0 } else { 2 };
        }
        512
    }

    fn end_command(&mut self) {
        self.in_command = false;
        self.phase = Phase::Command;
    }

    /// Run for `clocks` more clocks (Mednafen's `MDEC_Run`).
    pub fn run(&mut self, clocks: i32) {
        self.clock_counter = (self.clock_counter + clocks).min(128);
        loop {
            match self.phase {
                Phase::Command => {
                    let Some(c) = self.input.pop_front() else { return };
                    self.command = c;
                    self.in_command = true;
                    self.clock_counter -= 1;
                    self.phase = Phase::CommandClock;
                }
                Phase::CommandClock => {
                    if self.clock_counter <= 0 {
                        return;
                    }
                    match (self.command >> 29) & 7 {
                        1 => {
                            self.in_counter = self.command as u16;
                            self.output.clear();
                            self.pixel_count = 0;
                            self.coeff_index = 0;
                            let depth = (self.command >> 27) & 3;
                            self.decode_wb = if depth & 2 != 0 { 0 } else { 2 };
                            self.ram_offset_wws = [0, 0, 6, 4][depth as usize];
                            self.ram_offset_y = 0;
                            self.ram_offset_counter = self.ram_offset_wws;
                            self.in_counter = self.in_counter.wrapping_sub(1);
                            self.phase = Phase::DecodeRead;
                        }
                        2 => {
                            self.qm_index = 0;
                            self.in_counter = if self.command & 1 != 0 { 0x20 } else { 0x10 } - 1;
                            self.phase = Phase::QuantRead;
                        }
                        3 => {
                            self.idct_index = 0;
                            self.in_counter = 0x20 - 1;
                            self.phase = Phase::IdctRead;
                        }
                        _ => {
                            self.in_counter = self.command as u16;
                            self.end_command();
                        }
                    }
                }
                Phase::DecodeRead => {
                    let Some(w) = self.input.pop_front() else { return };
                    self.in_counter = self.in_counter.wrapping_sub(1);
                    self.pixel_count = 0;
                    let eat = self.write_image_data(w) + self.write_image_data(w >> 16);
                    self.clock_counter -= eat;
                    self.phase = Phase::DecodeClocks;
                }
                Phase::DecodeClocks => {
                    if self.clock_counter <= 0 {
                        return;
                    }
                    self.pixel_read = 0;
                    self.phase = Phase::DecodeOut;
                }
                Phase::DecodeOut => {
                    while self.pixel_read < self.pixel_count {
                        if self.output.len() >= FIFO {
                            return;
                        }
                        let o = self.pixel_read * 4;
                        self.output.push_back(u32::from_le_bytes(self.pixels[o..o + 4].try_into().unwrap()));
                        self.pixel_read += 1;
                    }
                    if self.in_counter == 0xFFFF {
                        self.end_command();
                    } else {
                        self.phase = Phase::DecodeRead;
                    }
                }
                Phase::QuantRead => {
                    let Some(mut w) = self.input.pop_front() else { return };
                    self.in_counter = self.in_counter.wrapping_sub(1);
                    for _ in 0..4 {
                        self.qmatrix[self.qm_index as usize] = w as u8;
                        self.qm_index = (self.qm_index + 1) & 0x7F;
                        w >>= 8;
                    }
                    if self.in_counter == 0xFFFF {
                        self.end_command();
                    }
                }
                Phase::IdctRead => {
                    let Some(mut w) = self.input.pop_front() else { return };
                    self.in_counter = self.in_counter.wrapping_sub(1);
                    for _ in 0..2 {
                        let i = self.idct_index as usize;
                        self.idct_matrix[((i & 7) << 3) | ((i >> 3) & 7)] = (w as u16 as i16) >> 3;
                        self.idct_index = (self.idct_index + 1) & 0x3F;
                        w >>= 16;
                    }
                    if self.in_counter == 0xFFFF {
                        self.end_command();
                    }
                }
            }
        }
    }

    /// A word from DMA channel 0.
    pub fn dma_write(&mut self, v: u32) {
        if self.input.len() < FIFO {
            self.input.push_back(v);
            self.run(0);
        }
    }

    /// A word for DMA channel 1, and the offset in words from the channel's
    /// address at which it goes.
    pub fn dma_read(&mut self) -> (u32, u32) {
        let Some(v) = self.output.pop_front() else { return (0, 0) };
        let wws = self.ram_offset_wws as u32;
        let mut offs = (self.ram_offset_y as u32 & 7) * wws;
        if self.ram_offset_y & 8 != 0 {
            offs = offs.wrapping_sub(wws * 7);
        }
        self.ram_offset_counter = self.ram_offset_counter.wrapping_sub(1);
        if self.ram_offset_counter == 0 {
            self.ram_offset_counter = self.ram_offset_wws;
            self.ram_offset_y = self.ram_offset_y.wrapping_add(1);
        }
        self.run(0);
        (v, offs)
    }

    pub fn dma_can_write(&self) -> bool {
        self.input.is_empty() && self.control & (1 << 30) != 0 && self.in_command && self.in_counter != 0xFFFF
    }

    pub fn dma_can_read(&self) -> bool {
        self.output.len() >= FIFO && self.control & (1 << 29) != 0
    }

    /// A write to MDEC0 (`a & 4 == 0`) or MDEC1.
    pub fn write(&mut self, a: u32, v: u32) {
        if a & 4 != 0 {
            if v & (1 << 31) != 0 {
                self.phase = Phase::Command;
                self.in_counter = 0;
                self.command = 0;
                self.in_command = false;
                self.pixel_count = 0;
                self.clock_counter = 0;
                self.qm_index = 0;
                self.idct_index = 0;
                self.qscale = 0;
                self.coeff = [0; 64];
                self.coeff_index = 0;
                self.decode_wb = 0;
                self.input.clear();
                self.output.clear();
            }
            self.control = v & 0x7FFF_FFFF;
        } else if self.input.len() < FIFO {
            self.input.push_back(v);
            if !self.in_command && self.clock_counter < 1 {
                self.clock_counter = 1;
            }
            self.run(0);
        }
    }

    /// Write `words` to MDEC0 with as many clocks as it takes, returning the
    /// output words where DMA channel 1 would put them (untimed, for tools
    /// that decode movie frames).
    pub fn feed_untimed(&mut self, words: impl IntoIterator<Item = u32>) -> Vec<u32> {
        let mut out = Vec::new();
        let mut n = 0u32;
        for w in words {
            self.write(0, w);
            loop {
                if self.output.is_empty() {
                    if self.input.is_empty() && self.clock_counter > 0 {
                        break;
                    }
                    self.run(128);
                    continue;
                }
                let (v, offs) = self.dma_read();
                let i = n.wrapping_add(offs) as usize;
                if out.len() <= i {
                    out.resize(i + 1, 0);
                }
                out[i] = v;
                n += 1;
            }
        }
        out
    }

    /// A read of MDEC0 (the output FIFO, `a & 4 == 0`) or MDEC1 (status).
    pub fn read(&mut self, a: u32) -> u32 {
        if a & 4 == 0 {
            return self.output.pop_front().unwrap_or(0);
        }
        (self.output.is_empty() as u32) << 31
            | ((self.input.len() >= FIFO) as u32) << 30
            | (self.in_command as u32) << 29
            | (self.dma_can_write() as u32) << 28
            | (self.dma_can_read() as u32) << 27
            | ((self.command >> 25) & 0xF) << 23
            | self.in_counter as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed words through MDEC0 with enough clocks to finish.
    fn feed(m: &mut Mdec, words: &[u32]) {
        for &w in words {
            m.write(0, w);
            m.run(128);
        }
        for _ in 0..8 {
            m.run(128);
        }
    }

    #[test]
    fn dc_only_block_decodes_to_flat_grey() {
        let mut m = Mdec::default();
        // IDCT matrix: every entry 0x5A82 (sqrt(1/8) << 15), so only DC matters.
        let mut words = vec![3 << 29];
        words.extend([0x5A82_5A82; 32]);
        words.push(2 << 29); // luminance quant table only
        words.extend([0x0101_0101; 16]);
        // Mono 8-bit decode of one block (two words): DC 0, then end of block.
        words.extend([(1 << 29) | (1 << 27) | 2, (0xFE00 << 16), 0xFE00_FE00]);
        feed(&mut m, &words);
        let out: Vec<u32> = (0..16).map(|_| m.read(0)).collect();
        assert!(out.iter().all(|&w| w == 0x8080_8080), "{out:08x?}");
        assert_eq!(m.read(4) >> 31, 1, "output empty");
        assert_eq!(m.read(4) & 0xFFFF, 0xFFFF, "no words left");
        assert_eq!(m.read(4) >> 29 & 1, 0, "command done");
    }

    #[test]
    fn decoding_waits_for_clocks() {
        let mut m = Mdec::default();
        m.write(0, (1 << 29) | (1 << 27) | 1);
        // A write starts the clock at 1, which the command's clock uses up.
        m.write(0, 0xFE00 << 16);
        assert!(m.output.is_empty());
        for _ in 0..4 {
            m.run(128);
        }
        assert!(m.output.is_empty(), "the block's 512 clocks are run but the counter is at 0");
        m.run(1);
        assert_eq!(m.output.len(), 16);
    }

    #[test]
    fn colour_output_offsets_build_a_16_pixel_row() {
        // 15-bit colour: four words per 8-pixel row.
        let mut m = Mdec { command: 3 << 27, ram_offset_wws: 4, ram_offset_counter: 4, ..Default::default() };
        m.output.extend(0..64u32);
        let offs: Vec<u32> = (0..12).map(|_| m.dma_read().1).collect();
        assert_eq!(offs, [0, 0, 0, 0, 4, 4, 4, 4, 8, 8, 8, 8]);
    }
}

//! Sound processing unit (psx-spx "Sound Processing Unit (SPU)").
//!
//! Modelled:
//! - 512 KiB sound RAM; manual FIFO and DMA-4 transfers (both directions trap the IRQ address);
//! - 24 voices decoding SPU-ADPCM (shift/filter, loop start/end/repeat flags, ENDX, the
//!   repeat-address register with CPU writes overriding loop-start flags until the next key on);
//! - voices keep walking their sample after release, as the hardware does ("all voices are
//!   permanently reading data from SPU RAM"), so silent voices still set ENDX and trap IRQs;
//! - the pitch counter (4.12) with pitch modulation (PMON) and 4-point Gaussian interpolation;
//! - the noise generator (NON, SPUCNT shift/step);
//! - ADSR envelopes and volume sweeps through one envelope engine (psx-spx "Envelope Operation"),
//!   including sweep phase (negative volumes), the slower exponential increase above 6000h and
//!   the frozen all-ones rates;
//! - reverb (psx-spx formula) at 22.05 kHz, left and right on alternate samples, with the 39-tap
//!   FIR resampler on its input and output; reverb RAM accesses trap the IRQ address;
//! - the capture buffers (CD left/right, voices 1 and 3) in the first 4 KiB of sound RAM;
//! - CD audio input, SPUCNT/SPUSTAT, the current-volume registers;
//! - 44.1 kHz stereo mixing into a capture buffer.

pub const RAM_SIZE: usize = 512 * 1024;
/// System clocks per output sample (33.8688 MHz / 44.1 kHz).
pub const CYCLES_PER_SAMPLE: u64 = 768;

/// The 512-entry 4-point Gaussian interpolation table (psx-spx "4-Point Gaussian Interpolation").
#[rustfmt::skip]
const GAUSS: [i32; 512] = [
    -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001,
    -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001, -0x001,
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0001,
    0x0001, 0x0001, 0x0001, 0x0002, 0x0002, 0x0002, 0x0003, 0x0003,
    0x0003, 0x0004, 0x0004, 0x0005, 0x0005, 0x0006, 0x0007, 0x0007,
    0x0008, 0x0009, 0x0009, 0x000A, 0x000B, 0x000C, 0x000D, 0x000E,
    0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x0015, 0x0016, 0x0018,
    0x0019, 0x001B, 0x001C, 0x001E, 0x0020, 0x0021, 0x0023, 0x0025,
    0x0027, 0x0029, 0x002C, 0x002E, 0x0030, 0x0033, 0x0035, 0x0038,
    0x003A, 0x003D, 0x0040, 0x0043, 0x0046, 0x0049, 0x004D, 0x0050,
    0x0054, 0x0057, 0x005B, 0x005F, 0x0063, 0x0067, 0x006B, 0x006F,
    0x0074, 0x0078, 0x007D, 0x0082, 0x0087, 0x008C, 0x0091, 0x0096,
    0x009C, 0x00A1, 0x00A7, 0x00AD, 0x00B3, 0x00BA, 0x00C0, 0x00C7,
    0x00CD, 0x00D4, 0x00DB, 0x00E3, 0x00EA, 0x00F2, 0x00FA, 0x0101,
    0x010A, 0x0112, 0x011B, 0x0123, 0x012C, 0x0135, 0x013F, 0x0148,
    0x0152, 0x015C, 0x0166, 0x0171, 0x017B, 0x0186, 0x0191, 0x019C,
    0x01A8, 0x01B4, 0x01C0, 0x01CC, 0x01D9, 0x01E5, 0x01F2, 0x0200,
    0x020D, 0x021B, 0x0229, 0x0237, 0x0246, 0x0255, 0x0264, 0x0273,
    0x0283, 0x0293, 0x02A3, 0x02B4, 0x02C4, 0x02D6, 0x02E7, 0x02F9,
    0x030B, 0x031D, 0x0330, 0x0343, 0x0356, 0x036A, 0x037E, 0x0392,
    0x03A7, 0x03BC, 0x03D1, 0x03E7, 0x03FC, 0x0413, 0x042A, 0x0441,
    0x0458, 0x0470, 0x0488, 0x04A0, 0x04B9, 0x04D2, 0x04EC, 0x0506,
    0x0520, 0x053B, 0x0556, 0x0572, 0x058E, 0x05AA, 0x05C7, 0x05E4,
    0x0601, 0x061F, 0x063E, 0x065C, 0x067C, 0x069B, 0x06BB, 0x06DC,
    0x06FD, 0x071E, 0x0740, 0x0762, 0x0784, 0x07A7, 0x07CB, 0x07EF,
    0x0813, 0x0838, 0x085D, 0x0883, 0x08A9, 0x08D0, 0x08F7, 0x091E,
    0x0946, 0x096F, 0x0998, 0x09C1, 0x09EB, 0x0A16, 0x0A40, 0x0A6C,
    0x0A98, 0x0AC4, 0x0AF1, 0x0B1E, 0x0B4C, 0x0B7A, 0x0BA9, 0x0BD8,
    0x0C07, 0x0C38, 0x0C68, 0x0C99, 0x0CCB, 0x0CFD, 0x0D30, 0x0D63,
    0x0D97, 0x0DCB, 0x0E00, 0x0E35, 0x0E6B, 0x0EA1, 0x0ED7, 0x0F0F,
    0x0F46, 0x0F7F, 0x0FB7, 0x0FF1, 0x102A, 0x1065, 0x109F, 0x10DB,
    0x1116, 0x1153, 0x118F, 0x11CD, 0x120B, 0x1249, 0x1288, 0x12C7,
    0x1307, 0x1347, 0x1388, 0x13C9, 0x140B, 0x144D, 0x1490, 0x14D4,
    0x1517, 0x155C, 0x15A0, 0x15E6, 0x162C, 0x1672, 0x16B9, 0x1700,
    0x1747, 0x1790, 0x17D8, 0x1821, 0x186B, 0x18B5, 0x1900, 0x194B,
    0x1996, 0x19E2, 0x1A2E, 0x1A7B, 0x1AC8, 0x1B16, 0x1B64, 0x1BB3,
    0x1C02, 0x1C51, 0x1CA1, 0x1CF1, 0x1D42, 0x1D93, 0x1DE5, 0x1E37,
    0x1E89, 0x1EDC, 0x1F2F, 0x1F82, 0x1FD6, 0x202A, 0x207F, 0x20D4,
    0x2129, 0x217F, 0x21D5, 0x222C, 0x2282, 0x22DA, 0x2331, 0x2389,
    0x23E1, 0x2439, 0x2492, 0x24EB, 0x2545, 0x259E, 0x25F8, 0x2653,
    0x26AD, 0x2708, 0x2763, 0x27BE, 0x281A, 0x2876, 0x28D2, 0x292E,
    0x298B, 0x29E7, 0x2A44, 0x2AA1, 0x2AFF, 0x2B5C, 0x2BBA, 0x2C18,
    0x2C76, 0x2CD4, 0x2D33, 0x2D91, 0x2DF0, 0x2E4F, 0x2EAE, 0x2F0D,
    0x2F6C, 0x2FCC, 0x302B, 0x308B, 0x30EA, 0x314A, 0x31AA, 0x3209,
    0x3269, 0x32C9, 0x3329, 0x3389, 0x33E9, 0x3449, 0x34A9, 0x3509,
    0x3569, 0x35C9, 0x3629, 0x3689, 0x36E8, 0x3748, 0x37A8, 0x3807,
    0x3867, 0x38C6, 0x3926, 0x3985, 0x39E4, 0x3A43, 0x3AA2, 0x3B00,
    0x3B5F, 0x3BBD, 0x3C1B, 0x3C79, 0x3CD7, 0x3D35, 0x3D92, 0x3DEF,
    0x3E4C, 0x3EA9, 0x3F05, 0x3F62, 0x3FBD, 0x4019, 0x4074, 0x40D0,
    0x412A, 0x4185, 0x41DF, 0x4239, 0x4292, 0x42EB, 0x4344, 0x439C,
    0x43F4, 0x444C, 0x44A3, 0x44FA, 0x4550, 0x45A6, 0x45FC, 0x4651,
    0x46A6, 0x46FA, 0x474E, 0x47A1, 0x47F4, 0x4846, 0x4898, 0x48E9,
    0x493A, 0x498A, 0x49D9, 0x4A29, 0x4A77, 0x4AC5, 0x4B13, 0x4B5F,
    0x4BAC, 0x4BF7, 0x4C42, 0x4C8D, 0x4CD7, 0x4D20, 0x4D68, 0x4DB0,
    0x4DF7, 0x4E3E, 0x4E84, 0x4EC9, 0x4F0E, 0x4F52, 0x4F95, 0x4FD7,
    0x5019, 0x505A, 0x509A, 0x50DA, 0x5118, 0x5156, 0x5194, 0x51D0,
    0x520C, 0x5247, 0x5281, 0x52BA, 0x52F3, 0x532A, 0x5361, 0x5397,
    0x53CC, 0x5401, 0x5434, 0x5467, 0x5499, 0x54CA, 0x54FA, 0x5529,
    0x5558, 0x5585, 0x55B2, 0x55DE, 0x5609, 0x5632, 0x565B, 0x5684,
    0x56AB, 0x56D1, 0x56F6, 0x571B, 0x573E, 0x5761, 0x5782, 0x57A3,
    0x57C3, 0x57E2, 0x57FF, 0x581C, 0x5838, 0x5853, 0x586D, 0x5886,
    0x589E, 0x58B5, 0x58CB, 0x58E0, 0x58F4, 0x5907, 0x5919, 0x592A,
    0x593A, 0x5949, 0x5958, 0x5965, 0x5971, 0x597C, 0x5986, 0x598F,
    0x5997, 0x599E, 0x59A4, 0x59A9, 0x59AD, 0x59B0, 0x59B2, 0x59B3,
];

/// Reverb input/output resampling filter (psx-spx "Reverb Buffer Resampling").
#[rustfmt::skip]
const FIR: [i32; 39] = [
    -0x0001, 0x0000, 0x0002, 0x0000, -0x000A, 0x0000, 0x0023, 0x0000,
    -0x0067, 0x0000, 0x010A, 0x0000, -0x0268, 0x0000, 0x0534, 0x0000,
    -0x0B90, 0x0000, 0x2806, 0x4000, 0x2806, 0x0000, -0x0B90, 0x0000,
    0x0534, 0x0000, -0x0268, 0x0000, 0x010A, 0x0000, -0x0067, 0x0000,
    0x0023, 0x0000, -0x000A, 0x0000, 0x0002, 0x0000, -0x0001,
];
/// Length of the resampler's history rings (a power of two above the 39 taps).
const RING: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Phase {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// One envelope configuration, shared by ADSR phases and volume sweeps
/// (psx-spx "Envelope Operation depending on Shift/Step/Mode/Direction").
#[derive(Clone, Copy, Debug)]
struct Env {
    /// Shift in bits 2-6, step in bits 0-1.
    rate: u32,
    exp: bool,
    dec: bool,
    /// Sweep phase: negative volumes.
    neg: bool,
    /// The rate field is all ones: the level never steps.
    frozen: bool,
}

impl Env {
    /// One 44.1 kHz tick: returns the new level.
    fn tick(self, level: i32, counter: &mut u32) -> i32 {
        let shift = (self.rate >> 2) as i32;
        // "The Phase bit seems to have no effect in Exponential Decrease mode."
        let neg = self.neg && !(self.exp && self.dec);
        let mut step = 7 - (self.rate & 3) as i32;
        if self.dec != neg {
            step = !step; // +7..+4 -> -8..-5
        }
        step <<= (11 - shift).max(0);
        let mut inc = 0x8000u32 >> (shift - 11).max(0);
        if self.exp && !self.dec && level > 0x6000 {
            // "Exponential increase" is a slower linear increase at high levels.
            if shift < 10 {
                step >>= 2;
            } else if shift >= 11 {
                inc >>= 2;
            } else {
                step >>= 1;
                inc >>= 1;
            }
        } else if self.exp && self.dec {
            step = (step * level) >> 15;
        }
        if !self.frozen {
            inc = inc.max(1);
        }
        *counter += inc;
        if *counter & 0x8000 == 0 {
            return level;
        }
        *counter = 0;
        let l = level + step;
        if !self.dec {
            l.clamp(-0x8000, 0x7FFF)
        } else if neg {
            l.clamp(-0x8000, 0)
        } else {
            l.max(0)
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Voice {
    vol_l: Volume,
    vol_r: Volume,
    pitch: u16,
    start: u16,
    adsr_lo: u16,
    adsr_hi: u16,
    repeat: u16,
    /// Current block address in bytes.
    addr: u32,
    counter: u32,
    samples: [i16; 28],
    /// The last three samples of the previous block (oldest first), for interpolation.
    old: [i16; 3],
    /// ADPCM filter history.
    hist: [i32; 2],
    phase: Phase,
    level: i32,
    env_counter: u32,
    /// Block header flags of the current block.
    flags: u8,
    decoded: bool,
    /// The CPU wrote the repeat address: loop-start flags don't replace it until the next key on.
    ignore_loop: bool,
    /// Latest output after the envelope (the pitch-modulation source and voice capture).
    out: i32,
}

impl Voice {
    /// Gaussian interpolation of the four most recent samples at the pitch counter.
    fn interpolate(&self) -> i32 {
        let idx = (self.counter >> 12) as isize;
        let at = |k: isize| -> i32 { if k >= 0 { self.samples[k as usize] as i32 } else { self.old[(3 + k) as usize] as i32 } };
        let i = ((self.counter >> 4) & 0xFF) as usize;
        ((GAUSS[0x0FF - i] * at(idx - 3)) >> 15)
            + ((GAUSS[0x1FF - i] * at(idx - 2)) >> 15)
            + ((GAUSS[0x100 + i] * at(idx - 1)) >> 15)
            + ((GAUSS[i] * at(idx)) >> 15)
    }

    /// The pitch counter step for this sample, modulated by the previous voice's output.
    fn pitch_step(&self, modulator: Option<i32>) -> u32 {
        let mut step = self.pitch as u32;
        if let Some(m) = modulator {
            let factor = m + 0x8000;
            // The hardware sign-extends the pitch and keeps the low 16 bits of the product.
            step = (((self.pitch as i16 as i32) * factor) >> 15) as u32 & 0xFFFF;
        }
        step.min(0x4000)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Spu {
    pub ram: Vec<u8>,
    voices: Vec<Voice>,
    main_l: Volume,
    main_r: Volume,
    cnt: u16,
    xfer_addr: u32,
    xfer_ctrl: u16,
    irq_addr: u16,
    irq_flag: bool,
    endx: u32,
    /// Other registers kept as plain storage (reverb, CD/ext volume, ...).
    regs: Vec<u16>,
    cycles: u64,
    /// Mixed stereo output (L, R interleaved) since the last [`Spu::take_samples`].
    #[serde(skip)]
    out: Vec<i16>,
    #[serde(skip)]
    pub capture: bool,
    pub keyed_on: u64,
    /// CD audio input sample for the next output sample (from CD-DA/XA).
    pub cd_in: (i16, i16),
    reverb: Reverb,
    noise_level: u16,
    noise_timer: i32,
    /// Halfword position in the four capture buffers (0..1FFh).
    capture_pos: u32,
    /// Analysis only: every voice ever given noise (NON) or pitch modulation
    /// (PMON), and the number of SPU interrupts raised.
    #[serde(skip)]
    pub noise_seen: u32,
    #[serde(skip)]
    pub pmon_seen: u32,
    #[serde(skip)]
    pub irqs: u64,
    /// Analysis only: ADPCM blocks a sounding voice fetched with a reserved shift (13-15) or filter (5-7).
    #[serde(skip)]
    pub reserved_blocks: u64,
}

/// The reverb unit (psx-spx "SPU Reverb Formula"): runs at 22.05 kHz on a
/// ring buffer in sound RAM from mBASE to the end, reading/writing it with
/// offsets from the reverb registers. The hardware computes the left side on
/// one 44.1 kHz sample and the right side on the next; input and output pass
/// through the 39-tap FIR resampler.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Reverb {
    /// Current buffer address (bytes).
    addr: u32,
    /// This sample computes the right side (else the left).
    right: bool,
    /// Ring position of the newest sample.
    pos: usize,
    /// 44.1 kHz input history, left then right ([`RING`] each).
    input: Vec<i16>,
    /// Zero-stuffed 44.1 kHz output history (one computed sample every other slot per side).
    output: Vec<i16>,
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb { addr: 0, right: false, pos: 0, input: vec![0; 2 * RING], output: vec![0; 2 * RING] }
    }
}

/// Reverb register offsets (from 1F801C00h).
mod rv {
    pub const D_APF1: usize = 0x1C0;
    pub const D_APF2: usize = 0x1C2;
    pub const V_IIR: usize = 0x1C4;
    pub const V_COMB: [usize; 4] = [0x1C6, 0x1C8, 0x1CA, 0x1CC];
    pub const V_WALL: usize = 0x1CE;
    pub const V_APF1: usize = 0x1D0;
    pub const V_APF2: usize = 0x1D2;
    pub const M_SAME: [usize; 2] = [0x1D4, 0x1D6];
    pub const M_COMB12: [[usize; 2]; 2] = [[0x1D8, 0x1DA], [0x1DC, 0x1DE]];
    pub const D_SAME: [usize; 2] = [0x1E0, 0x1E2];
    pub const M_DIFF: [usize; 2] = [0x1E4, 0x1E6];
    pub const M_COMB34: [[usize; 2]; 2] = [[0x1E8, 0x1EA], [0x1EC, 0x1EE]];
    pub const D_DIFF: [usize; 2] = [0x1F0, 0x1F2];
    pub const M_APF1: [usize; 2] = [0x1F4, 0x1F6];
    pub const M_APF2: [usize; 2] = [0x1F8, 0x1FA];
    pub const V_IN: [usize; 2] = [0x1FC, 0x1FE];
    pub const V_OUT: [usize; 2] = [0x184, 0x186];
    pub const M_BASE: usize = 0x1A2;
}

#[inline]
fn mul15(a: i32, b: i32) -> i32 {
    (a * b) >> 15
}

#[inline]
fn sat(v: i32) -> i32 {
    v.clamp(-0x8000, 0x7FFF)
}

/// FIR over a ring of [`RING`] samples ending at `pos` (newest).
#[inline]
fn fir(ring: &[i16], pos: usize) -> i32 {
    let mut acc = 0;
    for (t, &c) in FIR.iter().enumerate() {
        if c != 0 {
            acc += c * ring[(pos + RING - t) % RING] as i32;
        }
    }
    acc
}

impl Reverb {
    /// One side's 22.05 kHz step on input `x` (already resampled): returns the
    /// APF2 output (before the output volume) and whether a RAM access hit `irq`
    /// (byte address). Writes the buffer only when `write` (SPUCNT bit 7).
    fn side(&self, ram: &mut [u8], regs: &[u16], write: bool, side: usize, x: i32, irq: Option<u32>) -> (i32, bool) {
        let reg = |o: usize| regs[o >> 1] as i16 as i32;
        let off = |o: usize| regs[o >> 1] as i64 * 8;
        let base = regs[rv::M_BASE >> 1] as i64 * 8;
        let size = 0x80000 - base;
        let cur = self.addr as i64;
        let at = |o: i64| -> usize { (base + (cur - base + o).rem_euclid(size.max(2))) as usize & 0x7FFFE };
        let hit = std::cell::Cell::new(false);
        let touch = |a: usize| {
            if irq == Some(a as u32) {
                hit.set(true);
            }
        };
        let rd = |ram: &[u8], o: i64| {
            let a = at(o);
            touch(a);
            i16::from_le_bytes([ram[a], ram[a + 1]]) as i32
        };
        // With the master enable off, the hardware turns the writes into reads.
        let wr = |ram: &mut [u8], o: i64, v: i32| {
            let a = at(o);
            touch(a);
            if write {
                ram[a..a + 2].copy_from_slice(&(sat(v) as i16).to_le_bytes());
            }
        };
        let (iir, wall) = (reg(rv::V_IIR), reg(rv::V_WALL));
        let other = 1 - side;
        let x = mul15(x, reg(rv::V_IN[side]));
        // Same-side and cross-side reflections (IIR).
        let m_same = off(rv::M_SAME[side]);
        let prev = rd(ram, m_same - 2);
        let same = sat(mul15(sat(x + mul15(rd(ram, off(rv::D_SAME[side])), wall) - prev), iir) + prev);
        wr(ram, m_same, same);
        let m_diff = off(rv::M_DIFF[side]);
        let prev = rd(ram, m_diff - 2);
        let diff = sat(mul15(sat(x + mul15(rd(ram, off(rv::D_DIFF[other])), wall) - prev), iir) + prev);
        wr(ram, m_diff, diff);
        // Early echo: four comb taps.
        let combs = [rv::M_COMB12[0][side], rv::M_COMB12[1][side], rv::M_COMB34[0][side], rv::M_COMB34[1][side]];
        let mut o = 0;
        for (k, m) in combs.iter().enumerate() {
            o += mul15(reg(rv::V_COMB[k]), rd(ram, off(*m)));
        }
        let mut o = sat(o);
        // Late reverb: two all-pass filters.
        for (m_apf, d_apf, v_apf) in [(rv::M_APF1, rv::D_APF1, rv::V_APF1), (rv::M_APF2, rv::D_APF2, rv::V_APF2)] {
            let m = off(m_apf[side]);
            let delayed = rd(ram, m - off(d_apf));
            o = sat(o - mul15(reg(v_apf), delayed));
            wr(ram, m, o);
            o = sat(mul15(o, reg(v_apf)) + delayed);
        }
        (o, hit.get())
    }

    /// Advance the buffer address after both sides: `MAX(ESA, (addr+2) AND 7FFFEh)`.
    fn advance(&mut self, regs: &[u16]) {
        let base = regs[rv::M_BASE >> 1] as u32 * 8;
        self.addr = ((self.addr + 2) & 0x7FFFE).max(base);
    }

    /// One 44.1 kHz sample: takes the reverb input, returns the output (after
    /// the output volume) and whether an access hit the IRQ address.
    fn tick(&mut self, ram: &mut [u8], regs: &[u16], write: bool, input: (i32, i32), irq: Option<u32>) -> ((i32, i32), bool) {
        self.pos = (self.pos + 1) % RING;
        let pos = self.pos;
        self.input[pos] = sat(input.0) as i16;
        self.input[RING + pos] = sat(input.1) as i16;
        let side = self.right as usize;
        let x = sat(fir(&self.input[side * RING..(side + 1) * RING], pos) >> 15);
        let (y, hit) = self.side(ram, regs, write, side, x, irq);
        self.output[side * RING + pos] = y as i16;
        self.output[(1 - side) * RING + pos] = 0;
        if self.right {
            self.advance(regs);
        }
        self.right = !self.right;
        // Upsampling a zero-stuffed stream halves it: the filter runs at gain 2.
        let up = |s: usize| sat(fir(&self.output[s * RING..(s + 1) * RING], pos) >> 14);
        let vol = |s: usize| regs[rv::V_OUT[s] >> 1] as i16 as i32;
        ((mul15(up(0), vol(0)), mul15(up(1), vol(1))), hit)
    }
}

impl Default for Spu {
    fn default() -> Self {
        Spu {
            ram: vec![0; RAM_SIZE],
            voices: vec![Voice::default(); 24],
            main_l: Volume::default(),
            main_r: Volume::default(),
            cnt: 0,
            xfer_addr: 0,
            xfer_ctrl: 0,
            irq_addr: 0,
            irq_flag: false,
            endx: 0,
            regs: vec![0; 0x200],
            cycles: 0,
            out: Vec::new(),
            capture: false,
            keyed_on: 0,
            cd_in: (0, 0),
            reverb: Reverb::default(),
            noise_level: 0,
            noise_timer: 0,
            capture_pos: 0,
            noise_seen: 0,
            pmon_seen: 0,
            irqs: 0,
            reserved_blocks: 0,
        }
    }
}

/// A volume register: fixed (bit 15 clear: 15-bit signed volume ×2) or a
/// sweep (bit 15 set) that ramps the current level every sample.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Volume {
    raw: u16,
    /// Current level (what the current-volume registers read back).
    level: i32,
    counter: u32,
}

impl Volume {
    fn write(&mut self, v: u16) {
        self.raw = v;
        if v & 0x8000 == 0 {
            self.level = ((v << 1) as i16) as i32;
        }
        self.counter = 0;
    }

    #[inline]
    fn tick(&mut self) {
        if self.raw & 0x8000 == 0 {
            return;
        }
        let rate = (self.raw & 0x7F) as u32;
        let env = Env { rate, exp: self.raw & 0x4000 != 0, dec: self.raw & 0x2000 != 0, neg: self.raw & 0x1000 != 0, frozen: rate == 0x7F };
        self.level = env.tick(self.level, &mut self.counter);
    }
}

/// One sounding voice, for reports.
#[derive(Clone, Debug, serde::Serialize)]
pub struct VoiceInfo {
    pub voice: usize,
    pub phase: String,
    pub level: i32,
    pub vol_l: u16,
    pub vol_r: u16,
    pub pitch: u16,
    /// Start and current addresses in sound RAM (bytes).
    pub start: u32,
    pub addr: u32,
    pub adsr: u32,
}

impl Spu {
    /// Voices not in the Off phase.
    pub fn active_voices(&self) -> Vec<VoiceInfo> {
        self.voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.phase != Phase::Off)
            .map(|(i, v)| VoiceInfo {
                voice: i,
                phase: format!("{:?}", v.phase).to_lowercase(),
                level: v.level,
                vol_l: v.vol_l.raw,
                vol_r: v.vol_r.raw,
                pitch: v.pitch,
                start: (v.start as u32) << 3,
                addr: v.addr,
                adsr: (v.adsr_hi as u32) << 16 | v.adsr_lo as u32,
            })
            .collect()
    }

    /// Voices past their END block since their last key on (ENDX).
    pub fn endx(&self) -> u32 {
        self.endx
    }

    /// The next sound RAM byte address a transfer writes.
    pub fn transfer_addr(&self) -> u32 {
        self.xfer_addr
    }

    /// A voice's registers as a key on would use them: start (bytes), pitch,
    /// left/right volume registers, ADSR, and whether noise / pitch modulation
    /// (NON/PMON) apply to it.
    pub fn voice_settings(&self, i: usize) -> (u32, u16, [u16; 2], u32, bool, bool) {
        let v = &self.voices[i];
        let (noise, pmon) = (self.mask(0x194), self.mask(0x190));
        (
            (v.start as u32) << 3,
            v.pitch,
            [v.vol_l.raw, v.vol_r.raw],
            (v.adsr_hi as u32) << 16 | v.adsr_lo as u32,
            noise & (1 << i) != 0,
            i > 0 && pmon & (1 << i) != 0,
        )
    }

    pub fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.out)
    }

    /// A 32-bit voice mask from a pair of flag registers (e.g. 0x198/0x19A for EON).
    fn mask(&self, lo: u32) -> u32 {
        (self.regs[(lo >> 1) as usize] as u32 | (self.regs[(lo >> 1) as usize + 1] as u32) << 16) & 0xFF_FFFF
    }

    pub fn status(&self) -> u16 {
        let mut s = self.cnt & 0x3F;
        if self.irq_flag {
            s |= 1 << 6;
        }
        // Transfer requests mirror the selected DMA direction; never busy.
        match (self.cnt >> 4) & 3 {
            2 => s |= (1 << 7) | (1 << 8),
            3 => s |= (1 << 7) | (1 << 9),
            _ => {}
        }
        // Which half of the capture buffers is being written.
        if self.capture_pos & 0x100 != 0 {
            s |= 1 << 11;
        }
        s
    }

    pub fn read16(&mut self, off: u32) -> u16 {
        match off {
            0x000..=0x17F => {
                let v = &self.voices[(off >> 4) as usize];
                match off & 0xF {
                    0x0 => v.vol_l.raw,
                    0x2 => v.vol_r.raw,
                    0x4 => v.pitch,
                    0x6 => v.start,
                    0x8 => v.adsr_lo,
                    0xA => v.adsr_hi,
                    0xC => v.level as u16,
                    _ => v.repeat,
                }
            }
            0x180 => self.main_l.raw,
            0x182 => self.main_r.raw,
            0x1B8 => self.main_l.level as u16,
            0x1BA => self.main_r.level as u16,
            0x200..=0x25F => {
                let v = &self.voices[((off - 0x200) >> 2) as usize];
                (if off & 2 == 0 { v.vol_l.level } else { v.vol_r.level }) as u16
            }
            0x19C => self.endx as u16,
            0x19E => (self.endx >> 16) as u16,
            0x1A4 => self.irq_addr,
            0x1A6 => (self.xfer_addr >> 3) as u16,
            0x1AA => self.cnt,
            0x1AC => self.xfer_ctrl,
            0x1AE => self.status(),
            _ => self.regs[(off >> 1) as usize & 0x1FF],
        }
    }

    /// Returns true when this write raised the SPU interrupt.
    pub fn write16(&mut self, off: u32, v: u16) -> bool {
        self.regs[(off >> 1) as usize & 0x1FF] = v;
        match off {
            0x000..=0x17F => {
                let voice = &mut self.voices[(off >> 4) as usize];
                match off & 0xF {
                    0x0 => voice.vol_l.write(v),
                    0x2 => voice.vol_r.write(v),
                    0x4 => voice.pitch = v,
                    0x6 => voice.start = v,
                    0x8 => voice.adsr_lo = v,
                    0xA => voice.adsr_hi = v,
                    0xC => voice.level = v as i16 as i32,
                    _ => {
                        voice.repeat = v;
                        voice.ignore_loop = true;
                    }
                }
            }
            0x180 => self.main_l.write(v),
            0x182 => self.main_r.write(v),
            0x188 => self.key_on(v as u32),
            0x18A => self.key_on((v as u32) << 16),
            0x18C => self.key_off(v as u32),
            0x18E => self.key_off((v as u32) << 16),
            0x1A2 => self.reverb.addr = (v as u32) << 3,
            0x1A4 => self.irq_addr = v,
            0x1A6 => self.xfer_addr = (v as u32) << 3,
            0x1A8 => return self.fifo_write(v),
            0x1AA => {
                self.cnt = v;
                if v & (1 << 6) == 0 {
                    self.irq_flag = false; // IRQ9 disable acknowledges
                }
            }
            0x1AC => self.xfer_ctrl = v,
            0x190 => self.pmon_seen |= v as u32 & !1,
            0x192 => self.pmon_seen |= (v as u32) << 16,
            0x194 => self.noise_seen |= v as u32,
            0x196 => self.noise_seen |= (v as u32) << 16,
            _ => {}
        }
        false
    }

    /// The IRQ address as a byte address, when the interrupt is enabled
    /// (SPUCNT bit 6, "only when Bit15=1") and not already raised.
    fn irq_target(&self) -> Option<u32> {
        (self.cnt & 0x8040 == 0x8040 && !self.irq_flag).then_some((self.irq_addr as u32) << 3)
    }

    fn check_irq(&mut self, addr: u32, len: u32) -> bool {
        match self.irq_target() {
            Some(irq) if irq >= addr && irq < addr + len => {
                self.irq_flag = true;
                self.irqs += 1;
                true
            }
            _ => false,
        }
    }

    /// Manual transfer / DMA-4 write of one halfword at the transfer address.
    pub fn fifo_write(&mut self, v: u16) -> bool {
        let a = self.xfer_addr as usize & (RAM_SIZE - 1);
        self.ram[a..a + 2].copy_from_slice(&v.to_le_bytes());
        self.xfer_addr = (self.xfer_addr + 2) & (RAM_SIZE as u32 - 1);
        self.check_irq(a as u32, 2)
    }

    /// DMA-4 read of one halfword; returns it and whether it trapped the IRQ address.
    pub fn fifo_read(&mut self) -> (u16, bool) {
        let a = self.xfer_addr as usize & (RAM_SIZE - 1);
        self.xfer_addr = (self.xfer_addr + 2) & (RAM_SIZE as u32 - 1);
        (u16::from_le_bytes([self.ram[a], self.ram[a + 1]]), self.check_irq(a as u32, 2))
    }

    fn key_on(&mut self, mask: u32) {
        for i in 0..24 {
            if mask & (1 << i) != 0 {
                let v = &mut self.voices[i];
                v.addr = (v.start as u32) << 3;
                v.counter = 0;
                v.hist = [0, 0];
                v.old = [0; 3];
                v.decoded = false;
                v.ignore_loop = false;
                v.phase = Phase::Attack;
                v.level = 0;
                v.env_counter = 0;
                self.endx &= !(1 << i);
                self.keyed_on += 1;
            }
        }
    }

    /// Key off starts the release from whatever phase the voice is in.
    fn key_off(&mut self, mask: u32) {
        for i in 0..24 {
            if mask & (1 << i) != 0 && self.voices[i].phase != Phase::Off {
                self.voices[i].phase = Phase::Release;
                self.voices[i].env_counter = 0;
            }
        }
    }

    /// Advance by `cycles`, pulling one CD audio frame per output sample from
    /// `cd`; returns true if the SPU interrupt fired.
    pub fn tick(&mut self, cycles: u64, cd: &mut std::collections::VecDeque<(i16, i16)>) -> bool {
        self.cycles += cycles;
        let mut irq = false;
        while self.cycles >= CYCLES_PER_SAMPLE {
            self.cycles -= CYCLES_PER_SAMPLE;
            self.cd_in = cd.pop_front().unwrap_or((0, 0));
            irq |= self.sample();
        }
        irq
    }

    fn decode_block(&mut self, i: usize) -> bool {
        let a = self.voices[i].addr as usize & (RAM_SIZE - 1) & !0xF;
        let irq = self.check_irq(a as u32, 16);
        let header = self.ram[a];
        if (header & 0xF > 12 || header >> 4 > 4) && self.voices[i].phase != Phase::Off {
            self.reserved_blocks += 1;
        }
        let v = &mut self.voices[i];
        v.flags = self.ram[a + 1];
        if v.flags & 4 != 0 && !v.ignore_loop {
            v.repeat = (a >> 3) as u16;
        }
        psx_assets::adpcm::decode_block(&self.ram[a..a + 16], &mut v.hist, &mut v.samples);
        v.decoded = true;
        irq
    }

    fn envelope(v: &mut Voice) {
        let env = match v.phase {
            Phase::Off => return,
            Phase::Attack => {
                let rate = ((v.adsr_lo >> 8) & 0x7F) as u32;
                Env { rate, exp: v.adsr_lo & 0x8000 != 0, dec: false, neg: false, frozen: rate == 0x7F }
            }
            // Decay: always exponential, step -8, 4-bit shift.
            Phase::Decay => Env { rate: (((v.adsr_lo >> 4) & 0xF) as u32) << 2, exp: true, dec: true, neg: false, frozen: false },
            Phase::Sustain => {
                let rate = ((v.adsr_hi >> 6) & 0x7F) as u32;
                Env { rate, exp: v.adsr_hi & 0x8000 != 0, dec: v.adsr_hi & 0x4000 != 0, neg: false, frozen: rate == 0x7F }
            }
            // Release: step -8, 5-bit shift (all ones never steps).
            Phase::Release => {
                let shift = (v.adsr_hi & 0x1F) as u32;
                Env { rate: shift << 2, exp: v.adsr_hi & 0x20 != 0, dec: true, neg: false, frozen: shift == 0x1F }
            }
        };
        v.level = env.tick(v.level, &mut v.env_counter);
        let next = match v.phase {
            Phase::Attack if v.level >= 0x7FFF => Phase::Decay,
            Phase::Decay if v.level <= (((v.adsr_lo & 0xF) as i32) + 1) * 0x800 => Phase::Sustain,
            Phase::Release if v.level <= 0 => {
                v.level = 0;
                Phase::Off
            }
            p => p,
        };
        if next != v.phase {
            v.phase = next;
            v.env_counter = 0;
        }
    }

    /// One noise generator step (psx-spx "SPU Noise Generator").
    fn noise_tick(&mut self) {
        let step = 4 + ((self.cnt >> 8) & 3) as i32;
        let reload = 0x20000 >> ((self.cnt >> 10) & 0xF);
        let l = self.noise_level;
        let parity = ((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1;
        self.noise_timer -= step;
        if self.noise_timer < 0 {
            self.noise_level = (l << 1) | parity;
            self.noise_timer += reload;
            if self.noise_timer < 0 {
                self.noise_timer += reload;
            }
        }
    }

    /// Write one halfword to a capture buffer at the current position.
    fn capture_write(&mut self, base: usize, v: i32) -> bool {
        let a = base + self.capture_pos as usize * 2;
        self.ram[a..a + 2].copy_from_slice(&(sat(v) as i16).to_le_bytes());
        // "Capture IRQs do NOT occur if 1F801DACh.bit3-2 are both zero."
        self.xfer_ctrl & 0xC != 0 && self.check_irq(a as u32, 2)
    }

    fn sample(&mut self) -> bool {
        let mut irq = false;
        self.noise_tick();
        let (eon, non, pmon) = (self.mask(0x198), self.mask(0x194), self.mask(0x190) & !1);
        let noise = self.noise_level as i16 as i32;
        let (mut l, mut r) = (0i32, 0i32);
        // Reverb input: voices with their EON bit set.
        let (mut rl, mut rr) = (0i32, 0i32);
        for i in 0..24 {
            // Sweeps run whether or not the voice is sounding.
            self.voices[i].vol_l.tick();
            self.voices[i].vol_r.tick();
            // Every voice walks its sample, sounding or not (IRQ address, ENDX).
            if !self.voices[i].decoded {
                irq |= self.decode_block(i);
            }
            let modulator = (pmon & (1 << i) != 0).then(|| self.voices[i - 1].out);
            let v = &mut self.voices[i];
            if v.phase == Phase::Off {
                v.out = 0;
            } else {
                let s = if non & (1 << i) != 0 { noise } else { v.interpolate() };
                Self::envelope(v);
                let s = (s * v.level) >> 15;
                v.out = s;
                let (vl, vr) = ((s * v.vol_l.level) >> 15, (s * v.vol_r.level) >> 15);
                l += vl;
                r += vr;
                if eon & (1 << i) != 0 {
                    rl += vl;
                    rr += vr;
                }
            }
            v.counter += v.pitch_step(modulator);
            if v.counter >> 12 >= 28 {
                v.counter -= 28 << 12;
                v.old = [v.samples[25], v.samples[26], v.samples[27]];
                // Block finished: loop end / repeat flags.
                if v.flags & 1 != 0 {
                    self.endx |= 1 << i;
                    v.addr = (v.repeat as u32) << 3;
                    // End without repeat mutes the voice, except in noise mode.
                    if v.flags & 2 == 0 && non & (1 << i) == 0 {
                        v.phase = Phase::Off;
                        v.level = 0;
                    }
                } else {
                    v.addr = (v.addr + 16) & (RAM_SIZE as u32 - 1);
                }
                v.decoded = false;
            }
        }
        // Capture buffers: CD audio before volume, voices 1 and 3 after the envelope.
        let (cd_l, cd_r) = (self.cd_in.0 as i32, self.cd_in.1 as i32);
        let (v1, v3) = (self.voices[1].out, self.voices[3].out);
        irq |= self.capture_write(0x000, cd_l);
        irq |= self.capture_write(0x400, cd_r);
        irq |= self.capture_write(0x800, v1);
        irq |= self.capture_write(0xC00, v3);
        self.capture_pos = (self.capture_pos + 1) & 0x1FF;
        // SPUCNT bits 15 (enable) and 14 (unmute) silence the voices, not CD audio.
        if self.cnt & 0xC000 != 0xC000 {
            l = 0;
            r = 0;
        }
        // CD audio input (SPUCNT bit 0) through the CD volume (0x1DB0/0x1DB2).
        if self.cnt & 1 != 0 {
            let (cl, cr) = (mul15(cd_l, self.regs[0x1B0 >> 1] as i16 as i32), mul15(cd_r, self.regs[0x1B2 >> 1] as i16 as i32));
            l += cl;
            r += cr;
            // SPUCNT bit 2: CD audio into the reverb.
            if self.cnt & 4 != 0 {
                rl += cl;
                rr += cr;
            }
        }
        self.cd_in = (0, 0);
        // Reverb RAM accesses trap the IRQ address. psx-spx doesn't know which
        // ones do; only trapping them with the master enable on keeps a game
        // that never set up reverb (mBASE 0, sweeping all of RAM) from taking
        // stray interrupts.
        let master = self.cnt & 0x80 != 0;
        let target = self.irq_target().filter(|_| master);
        let ((ol, or), hit) = self.reverb.tick(&mut self.ram, &self.regs, master, (rl, rr), target);
        if hit {
            self.irq_flag = true;
            self.irqs += 1;
            irq = true;
        }
        l += ol;
        r += or;
        self.main_l.tick();
        self.main_r.tick();
        if self.capture {
            // The main volume applies after clamping the mix.
            self.out.push(sat(mul15(sat(l), self.main_l.level)) as i16);
            self.out.push(sat(mul15(sat(r), self.main_r.level)) as i16);
        }
        irq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs_with(pairs: &[(usize, u16)]) -> Vec<u16> {
        let mut regs = vec![0u16; 0x200];
        for &(off, v) in pairs {
            regs[off >> 1] = v;
        }
        regs
    }

    #[test]
    fn reverb_delay_line_round_trips_through_sound_ram() {
        let mut ram = vec![0u8; RAM_SIZE];
        let regs = regs_with(&[
            (rv::M_BASE, 0xE000), // work area from 70000h
            (rv::V_IN[0], 0x7FFF),
            (rv::V_IIR, 0x7FFF),
            (rv::V_COMB[0], 0x7FFF),
            (rv::M_SAME[0], 0x10), // written 64 steps ahead...
            (rv::M_COMB12[0][0], 0), // ...of where comb 1 reads
            // Keep the other taps clear of that line. With vAPF = 0 each all-pass
            // stage is a pure delay of dAPF (8 bytes = 4 steps).
            (rv::M_DIFF[0], 0x200),
            (rv::M_APF1[0], 0x300),
            (rv::M_APF2[0], 0x380),
            (rv::D_APF1, 1),
            (rv::D_APF2, 1),
        ]);
        let mut r = Reverb { addr: 0x70000, ..Default::default() };
        let mut outs = Vec::new();
        for step in 0..200 {
            let input = if step == 0 { 0x4000 } else { 0 };
            outs.push(r.side(&mut ram, &regs, true, 0, input, None).0);
            r.advance(&regs);
        }
        let first = outs.iter().position(|&o| o != 0);
        assert_eq!(first, Some(72), "impulse arrives after 64 + 4 + 4 steps");
        assert!(outs[72] > 0x3000, "{}", outs[72]);
        // Nothing is written with the master enable off.
        let mut quiet = vec![0u8; RAM_SIZE];
        let r = Reverb { addr: 0x70000, ..Default::default() };
        r.side(&mut quiet, &regs, false, 0, 0x4000, None);
        assert!(quiet.iter().all(|&b| b == 0));
    }

    #[test]
    fn fir_coefficients_have_unity_dc_gain() {
        // The taps sum to 7FFEh: unity at DC for the decimator (>> 15), and the
        // zero-stuffed interpolator (>> 14) sees half of them on either phase.
        assert_eq!(FIR.iter().sum::<i32>(), 0x7FFE);
        let even: i32 = FIR.iter().step_by(2).sum();
        let odd: i32 = FIR.iter().skip(1).step_by(2).sum();
        assert_eq!((even, odd), (0x3FFE, 0x4000));
    }

    #[test]
    fn reverb_alternates_sides_and_resamples() {
        // A delay line per side through comb 1 only (vAPF = 0, dAPF = 1): a
        // constant input reaches the output at its own level, through both FIRs.
        let regs = regs_with(&[
            (rv::M_BASE, 0xE000),
            (rv::V_IN[0], 0x7FFF),
            (rv::V_IN[1], 0x7FFF),
            (rv::V_OUT[0], 0x7FFF),
            (rv::V_OUT[1], 0x7FFF),
            (rv::V_IIR, 0x7FFF),
            (rv::V_COMB[0], 0x7FFF),
            (rv::M_SAME[0], 0x10),
            (rv::M_SAME[1], 0x810),
            (rv::M_COMB12[0][1], 0x800),
            (rv::M_DIFF[0], 0x200),
            (rv::M_DIFF[1], 0xA00),
            (rv::M_APF1[0], 0x300),
            (rv::M_APF1[1], 0xB00),
            (rv::M_APF2[0], 0x380),
            (rv::M_APF2[1], 0xB80),
            (rv::D_APF1, 1),
            (rv::D_APF2, 1),
        ]);
        let mut ram = vec![0u8; RAM_SIZE];
        let mut r = Reverb { addr: 0x70000, ..Default::default() };
        let mut last = (0, 0);
        for n in 0..2000 {
            assert_eq!(r.right, n % 2 == 1, "left on even samples, right on odd");
            last = r.tick(&mut ram, &regs, true, (0x1000, -0x1000), None).0;
        }
        assert!((last.0 - 0x1000).abs() < 8 && (last.1 + 0x1000).abs() < 8, "{last:?}");
        // A single impulse spreads over the filter's taps: no single sample
        // carries it, unlike the old pair-average-and-hold.
        let mut ram = vec![0u8; RAM_SIZE];
        let mut r = Reverb { addr: 0x70000, ..Default::default() };
        let mut outs = Vec::new();
        for n in 0..400 {
            outs.push(r.tick(&mut ram, &regs, true, (if n == 0 { 0x7FFF } else { 0 }, 0), None).0 .0);
        }
        let nonzero = outs.iter().filter(|&&o| o != 0).count();
        assert!(nonzero > 20, "impulse response spans the FIR ({nonzero} samples)");
        assert!(outs.iter().any(|&o| o < 0), "with the FIR's negative lobes");
    }

    #[test]
    fn reverb_accesses_trap_the_irq_address() {
        let mut s = Spu::default();
        s.write16(rv::M_BASE as u32, 0xE000);
        s.write16(rv::M_SAME[0] as u32, 0x10); // left same-side write at base + 80h
        s.write16(0x1A4, ((0x70000 + 0x80) >> 3) as u16);
        s.write16(0x1AA, 0x80C0); // reverb master on
        let mut fired = false;
        for _ in 0..4 {
            fired |= s.tick(CYCLES_PER_SAMPLE, &mut Default::default());
        }
        assert!(fired && s.status() & 0x40 != 0);
    }

    #[test]
    fn gauss_table_matches_psx_spx() {
        assert_eq!((GAUSS[0x000], GAUSS[0x017], GAUSS[0x07F]), (-0x001, 0x0001, 0x019C));
        assert_eq!((GAUSS[0x080], GAUSS[0x0FF], GAUSS[0x100]), (0x01A8, 0x12C7, 0x1307));
        assert_eq!((GAUSS[0x17F], GAUSS[0x180], GAUSS[0x1FF]), (0x3DEF, 0x3E4C, 0x59B3));
        // "each four values ... sum up to 7F7Fh..7F81h".
        for i in 0..256 {
            let sum = GAUSS[i] + GAUSS[0xFF - i] + GAUSS[0x100 + i] + GAUSS[0x1FF - i];
            assert!((0x7F7F..=0x7F81).contains(&sum), "{i:#x}: {sum:#x}");
        }
        // The table is monotonic from entry 10h up (the first 16 are -1).
        assert!(GAUSS[0x10..].windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn gaussian_interpolation_weights_the_four_latest_samples() {
        let mut v = Voice { samples: [0x4000; 28], old: [0x4000; 3], ..Default::default() };
        for c in [0, 0x10, 0x7F0, 0x800, 0xFF0, 5 << 12] {
            v.counter = c;
            let out = v.interpolate();
            // A constant comes out at ~7F80h/8000h of its level.
            assert!((0x3FBC..=0x3FC1).contains(&out), "{c:#x}: {out:#x}");
        }
        // At fraction 0 the output is dominated by the sample two back.
        let mut v = Voice::default();
        v.samples[3] = 0x1000;
        v.counter = 5 << 12;
        assert_eq!(v.interpolate(), (GAUSS[0x1FF] * 0x1000) >> 15);
        // History from the previous block feeds the first samples of the next.
        let v = Voice { old: [0, 0x1000, 0], counter: 0, ..Default::default() };
        assert_eq!(v.interpolate(), (GAUSS[0x1FF] * 0x1000) >> 15);
    }

    #[test]
    fn noise_generator_follows_the_lfsr() {
        // Shift 0Fh, step 3 (7): the timer reloads by 4 and steps every sample.
        let mut s = Spu { cnt: 0xF << 10 | 3 << 8, ..Default::default() };
        let mut model: u16 = 0;
        for _ in 0..1000 {
            s.noise_tick();
            let p = ((model >> 15) ^ (model >> 12) ^ (model >> 11) ^ (model >> 10) ^ 1) & 1;
            model = (model << 1) | p;
            assert_eq!(s.noise_level, model);
        }
        // The first bits shifted in from zero are all ones.
        let mut s = Spu { cnt: 0xF << 10 | 3 << 8, ..Default::default() };
        for _ in 0..10 {
            s.noise_tick();
        }
        assert_eq!(s.noise_level, 0x3FF);
        // Shift 0, step 0 (4): one level change per 20000h/4 = 8000h samples.
        let mut s = Spu::default();
        let mut changes = 0;
        for _ in 0..0x20000 {
            let before = s.noise_level;
            s.noise_tick();
            changes += (s.noise_level != before) as u32;
        }
        assert_eq!(changes, 4);
        // Shift 10, step 2 (6): reload 80h, 6 per sample -> a step every 128/6 samples.
        let mut s = Spu { cnt: 10 << 10 | 2 << 8, ..Default::default() };
        let mut steps = 0;
        for _ in 0..6400 {
            let before = s.noise_level;
            s.noise_tick();
            steps += (s.noise_level != before) as u32;
        }
        assert!((290..=301).contains(&steps), "{steps}");
    }

    /// A one-block looping square wave at 1000h, voice `n` keyed with full volumes.
    fn spu_with_square(voices: &[usize]) -> Spu {
        let mut s = Spu { capture: true, ..Default::default() };
        s.write16(0x1A6, 0x1000 >> 3);
        let mut block = [0x77u8; 16];
        block[0] = 0x00;
        block[1] = 0x07;
        for b in &mut block[9..] {
            *b = 0x88;
        }
        for w in block.chunks(2) {
            s.write16(0x1A8, u16::from_le_bytes([w[0], w[1]]));
        }
        for &n in voices {
            let b = (n as u32) << 4;
            s.write16(b + 0x6, 0x1000 >> 3);
            s.write16(b + 0x4, 0x1000);
            s.write16(b, 0x3FFF);
            s.write16(b + 0x2, 0x3FFF);
            s.write16(b + 0x8, 0x000F);
            s.write16(b + 0xA, 0x0000);
        }
        s.write16(0x180, 0x3FFF);
        s.write16(0x182, 0x3FFF);
        s.write16(0x1AA, 0xC000);
        s
    }

    #[test]
    fn noise_voice_plays_the_generator_through_its_envelope() {
        let mut s = spu_with_square(&[2]);
        s.write16(0x194, 1 << 2);
        s.write16(0x1AA, 0xC000 | 0xF << 10 | 3 << 8);
        s.write16(0x188, 1 << 2);
        s.tick(CYCLES_PER_SAMPLE * 2000, &mut Default::default());
        let out = s.take_samples();
        let distinct: std::collections::HashSet<i16> = out.iter().copied().collect();
        assert!(distinct.len() > 100, "noise, not the square wave ({} values)", distinct.len());
        assert_eq!(s.voices[2].out, (s.noise_level as i16 as i32 * s.voices[2].level) >> 15);
    }

    #[test]
    fn pitch_modulation_scales_by_the_previous_voice() {
        let v = Voice { pitch: 0x1000, ..Default::default() };
        assert_eq!(v.pitch_step(None), 0x1000);
        assert_eq!(v.pitch_step(Some(0)), 0x1000);
        assert_eq!(v.pitch_step(Some(0x7FFF)), 0x1FFF);
        assert_eq!(v.pitch_step(Some(-0x8000)), 0);
        assert_eq!(v.pitch_step(Some(-0x4000)), 0x800);
        // Clipped after modulation to 4000h; pitches from 8000h are taken as negative.
        let v = Voice { pitch: 0x3000, ..Default::default() };
        assert_eq!(v.pitch_step(Some(0x7FFF)), 0x4000);
        let v = Voice { pitch: 0x8000, ..Default::default() };
        assert_eq!(v.pitch_step(Some(0)), 0x4000);
        // In the mixer: voice 1 modulated by a silent voice 0 runs at its own pitch;
        // voice 0 can't be modulated (PMON bit 0 is ignored).
        let mut s = spu_with_square(&[0, 1]);
        s.write16(0x190, 0b11);
        s.write16(0x000, 0);
        s.write16(0x002, 0);
        s.write16(0x188, 0b10);
        s.tick(CYCLES_PER_SAMPLE * 7, &mut Default::default());
        assert_eq!(s.voices[1].counter, 7 * 0x1000);
        // With voice 0 sounding, voice 1's counter no longer advances evenly.
        let mut s = spu_with_square(&[0, 1]);
        s.write16(0x190, 0b11);
        s.write16(0x188, 0b11);
        let mut steps = std::collections::HashSet::new();
        for _ in 0..60 {
            let before = s.voices[1].counter;
            let blocks = s.voices[1].addr;
            s.tick(CYCLES_PER_SAMPLE, &mut Default::default());
            if s.voices[1].addr == blocks && s.voices[1].counter > before {
                steps.insert(s.voices[1].counter - before);
            }
            assert_eq!(s.voices[0].counter % 0x1000, 0, "voice 0 unmodulated");
        }
        assert!(steps.len() > 3, "{steps:?}");
    }

    #[test]
    fn volume_sweeps() {
        let mut v = Volume::default();
        v.write(0x3FFF);
        assert_eq!(v.level, 0x7FFE);
        // Linear decrease, rate 0x20: reaches 0 and holds.
        v.write(0x8000 | 0x2000 | 0x20);
        for _ in 0..44_100 {
            v.tick();
        }
        assert_eq!(v.level, 0);
        // Exponential increase from 0 reaches full scale.
        v.write(0x8000 | 0x4000 | 0x10);
        for _ in 0..44_100 * 4 {
            v.tick();
        }
        assert_eq!(v.level, 0x7FFF);
    }

    #[test]
    fn sweep_phase_negative() {
        // Linear increase with phase negative heads for -8000h.
        let mut v = Volume::default();
        v.write(0x8000 | 0x1000 | 0x10);
        for _ in 0..44_100 {
            v.tick();
        }
        assert_eq!(v.level, -0x8000);
        // Linear decrease with phase negative climbs back to 0 and stops there.
        v.write(0x8000 | 0x2000 | 0x1000 | 0x10);
        for _ in 0..44_100 {
            v.tick();
        }
        assert_eq!(v.level, 0);
        // A positive level with an inverted increase sweeps down through zero.
        v.write(0x2000);
        v.write(0x8000 | 0x1000 | 0x10);
        let mut seen_zero = false;
        for _ in 0..44_100 {
            v.tick();
            seen_zero |= v.level == 0;
        }
        assert!(seen_zero && v.level == -0x8000);
        // Wipeout 3's case: an inverted sweep with all rate bits set never steps.
        v.write(0x2000);
        v.write(0x8000 | 0x1000 | 0x7F);
        for _ in 0..44_100 {
            v.tick();
        }
        assert_eq!(v.level, 0x4000);
    }

    #[test]
    fn envelope_rates_follow_psx_spx() {
        let lin_inc = |rate: u32| Env { rate, exp: false, dec: false, neg: false, frozen: rate == 0x7F };
        // Shift <= 11: a step every sample of (7-step) << (11-shift).
        let mut c = 0;
        assert_eq!(lin_inc(0).tick(0, &mut c), 7 << 11);
        assert_eq!(lin_inc(11 << 2 | 3).tick(0, &mut c), 4);
        // Shift 13: one step of +7 every 4 samples.
        let (mut c, mut l) = (0, 0);
        let mut levels = Vec::new();
        for _ in 0..8 {
            l = lin_inc(13 << 2).tick(l, &mut c);
            levels.push(l);
        }
        assert_eq!(levels, [0, 0, 0, 7, 7, 7, 7, 14]);
        // Exponential increase above 6000h: shift < 10 quarters the step...
        let exp_inc = |rate: u32| Env { rate, exp: true, dec: false, neg: false, frozen: false };
        let mut c = 0;
        assert_eq!(exp_inc(8 << 2).tick(0x6000, &mut c), 0x6000 + (7 << 3));
        assert_eq!(exp_inc(8 << 2).tick(0x6001, &mut c), 0x6001 + ((7 << 3) >> 2));
        // ...shift 10 halves step and rate, shift >= 11 quarters the rate.
        let count = |rate: u32, level: i32| {
            let (mut c, mut n) = (0, 0);
            while exp_inc(rate).tick(level, &mut c) == level {
                n += 1;
            }
            n + 1
        };
        assert_eq!(count(10 << 2, 0x7000), 2);
        assert_eq!(exp_inc(10 << 2).tick(0x7000, &mut 0x4000), 0x7000 + ((7 << 1) >> 1));
        assert_eq!(count(12 << 2, 0x5000), 2);
        assert_eq!(count(12 << 2, 0x7000), 8);
        // Exponential decrease scales the step by the level (rounding down).
        let exp_dec = Env { rate: 0, exp: true, dec: true, neg: true, frozen: false };
        assert_eq!(exp_dec.tick(0x4000, &mut 0), 0x4000 - ((8 << 11) >> 1));
        // The slowest unfrozen rates still step (increment clamped to 1).
        assert_eq!(count(0x7E, 0), 0x8000);
        let frozen = lin_inc(0x7F);
        let mut c = 0;
        for _ in 0..0x10000 {
            assert_eq!(frozen.tick(0x1234, &mut c), 0x1234);
        }
    }

    #[test]
    fn exponential_release_reaches_silence() {
        // Release: exponential, rate 13 (Croc's boot voices hold at 4095 without the fix).
        let mut v = Voice { phase: Phase::Release, level: 0x7FFF, adsr_hi: 0x20 | 13, ..Default::default() };
        for _ in 0..44_100 * 4 {
            Spu::envelope(&mut v);
        }
        assert_eq!((v.phase, v.level), (Phase::Off, 0));
        // Release shift 1Fh never steps.
        let mut v = Voice { phase: Phase::Release, level: 0x4000, adsr_hi: 0x1F, ..Default::default() };
        for _ in 0..0x10000 {
            Spu::envelope(&mut v);
        }
        assert_eq!((v.phase, v.level), (Phase::Release, 0x4000));
    }

    #[test]
    fn adsr_phases_and_mid_envelope_writes() {
        // Fast linear attack, decay to sustain level 8 (4800h), sustain flat.
        let mut v = Voice { phase: Phase::Attack, adsr_lo: 0x0048, adsr_hi: 0x1FC0, ..Default::default() };
        for _ in 0..3 {
            Spu::envelope(&mut v);
        }
        assert_eq!((v.phase, v.level), (Phase::Decay, 0x7FFF));
        for _ in 0..1000 {
            Spu::envelope(&mut v);
        }
        assert_eq!(v.phase, Phase::Sustain);
        assert!(v.level <= 0x4800 && v.level > 0x4000, "{:#x}", v.level);
        // Sustain rate 7Fh is frozen; a write switching to a linear decrease
        // takes effect on the next step.
        let held = v.level;
        Spu::envelope(&mut v);
        assert_eq!(v.level, held);
        v.adsr_hi = 0x4000;
        Spu::envelope(&mut v);
        assert_eq!(v.level, held - (8 << 11));
        // Key off during the attack goes straight to release.
        let mut s = spu_with_square(&[0]);
        s.write16(0x008, 0x7F0F); // very slow attack
        s.write16(0x00A, 0x0005);
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 10, &mut Default::default());
        assert_eq!(s.voices[0].phase, Phase::Attack);
        s.write16(0x18C, 1);
        assert_eq!(s.voices[0].phase, Phase::Release);
    }

    #[test]
    fn keyed_voice_plays_looped_square_wave() {
        let mut s = spu_with_square(&[0]);
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 200, &mut Default::default());
        let out = s.take_samples();
        assert_eq!(out.len(), 400);
        assert!(out.iter().any(|&x| x > 1000) && out.iter().any(|&x| x < -1000), "audible square wave");
        assert_eq!(s.read16(0x19C) & 1, 1, "ENDX set after loop end");
        // Key on clears ENDX.
        s.write16(0x188, 1);
        assert_eq!(s.read16(0x19C) & 1, 0);
        // Muting silences the voices.
        s.write16(0x1AA, 0x8000);
        s.take_samples();
        s.tick(CYCLES_PER_SAMPLE * 50, &mut Default::default());
        assert!(s.take_samples().iter().all(|&x| x == 0));
    }

    /// Writes `blocks` (16 bytes each) at `addr` through the FIFO.
    fn load(s: &mut Spu, addr: u32, blocks: &[[u8; 16]]) {
        s.write16(0x1A6, (addr >> 3) as u16);
        for b in blocks {
            for w in b.chunks(2) {
                s.write16(0x1A8, u16::from_le_bytes([w[0], w[1]]));
            }
        }
    }

    fn block(flags: u8, nibbles: u8) -> [u8; 16] {
        let mut b = [nibbles; 16];
        b[0] = 0;
        b[1] = flags;
        b
    }

    #[test]
    fn one_shot_end_jumps_to_repeat_and_keeps_walking() {
        let mut s = spu_with_square(&[]);
        // 2000h: one block with loop end (code 1); 3000h: a silent loop.
        load(&mut s, 0x2000, &[block(1, 0x77)]);
        load(&mut s, 0x3000, &[block(7, 0x00)]);
        s.write16(0x006, 0x2000 >> 3);
        s.write16(0x004, 0x1000);
        s.write16(0x008, 0x000F);
        s.write16(0x188, 1);
        s.write16(0x00E, 0x3000 >> 3); // redirect the one-shot after key on
        s.tick(CYCLES_PER_SAMPLE * 30, &mut Default::default());
        assert_eq!(s.voices[0].phase, Phase::Off);
        assert_eq!(s.read16(0x19C) & 1, 1);
        assert_eq!(s.voices[0].addr, 0x3000, "jumped to the repeat address");
        // The silent voice keeps walking: it traps an IRQ address in the loop.
        s.write16(0x1A4, 0x3000 >> 3);
        s.write16(0x1AA, 0xC040);
        let mut fired = false;
        for _ in 0..60 {
            fired |= s.tick(CYCLES_PER_SAMPLE, &mut Default::default());
        }
        assert!(fired);
    }

    #[test]
    fn repeat_address_write_overrides_loop_start_until_key_on() {
        let mut s = spu_with_square(&[]);
        // 2000h: loop start; 2010h: end+repeat. 4000h: a loop the CPU points at.
        load(&mut s, 0x2000, &[block(4, 0x11), block(3, 0x11)]);
        load(&mut s, 0x4000, &[block(7, 0x22)]);
        s.write16(0x006, 0x2000 >> 3);
        s.write16(0x004, 0x1000);
        s.write16(0x00E, 0x4000 >> 3); // before key on: the loop-start flag wins
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 60, &mut Default::default());
        assert_eq!(s.voices[0].repeat, 0x2000 >> 3);
        assert_eq!(s.voices[0].addr, 0x2000);
        // After key on: the write sticks through the loop-start block.
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 2, &mut Default::default());
        s.write16(0x00E, 0x4000 >> 3);
        s.tick(CYCLES_PER_SAMPLE * 60, &mut Default::default());
        assert_eq!(s.voices[0].repeat, 0x4000 >> 3);
        assert_eq!(s.voices[0].addr & !0xF, 0x4000);
    }

    #[test]
    fn transfers_and_capture_buffers_trap_the_irq_address() {
        // DMA read.
        let mut s = Spu::default();
        s.write16(0x1A4, 0x2000 >> 3);
        s.write16(0x1AA, 0x8040);
        s.write16(0x1A6, (0x2000 - 8) >> 3);
        let fired: Vec<bool> = (0..6).map(|_| s.fifo_read().1).collect();
        assert_eq!(fired, [false, false, false, false, true, false], "once, at 2000h");
        // Disabled without SPUCNT bit 15.
        let mut s = Spu::default();
        s.write16(0x1A4, 0x2000 >> 3);
        s.write16(0x1AA, 0x0040);
        s.write16(0x1A6, 0x2000 >> 3);
        assert!(!s.fifo_write(0));
        // Capture: CD left at 000h.., voice 1 at 800h..; IRQ when RAM_CTRL bits 2-3 are set.
        let mut s = Spu::default();
        s.write16(0x1AC, 0x0004);
        s.write16(0x1A4, (0x400 + 0x10) >> 3); // CD right, sample 8
        s.write16(0x1AA, 0x8040);
        let mut cd: std::collections::VecDeque<(i16, i16)> = (0..20).map(|n| (n as i16, -(n as i16))).collect();
        let mut at = None;
        for n in 0..20 {
            if s.tick(CYCLES_PER_SAMPLE, &mut cd) {
                at.get_or_insert(n);
            }
        }
        assert_eq!(at, Some(8));
        assert_eq!(i16::from_le_bytes([s.ram[6], s.ram[7]]), 3);
        assert_eq!(i16::from_le_bytes([s.ram[0x406], s.ram[0x407]]), -3);
        assert_eq!(s.status() & 0x800, 0);
        s.tick(CYCLES_PER_SAMPLE * 0x100, &mut cd);
        assert_eq!(s.status() & 0x800, 0x800, "second half");
    }
}

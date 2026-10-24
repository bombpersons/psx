//! Sound processing unit, ported from Mednafen's `spu.cpp` and
//! `spu_reverb.inc` (`crates/psx-oracle/mednafen/src/psx/`).
//!
//! The SPU runs a sample (pair) at a time, every 768 system clocks, clocked by
//! the CD-ROM controller as in Mednafen. Each sample, every voice decodes
//! SPU-ADPCM four samples at a time, interpolates with the 4-point Gaussian
//! table, applies its envelope and volume sweeps and steps its pitch counter
//! (with pitch modulation); voices 1 and 3 and the CD input go to the capture
//! buffers; then key on and key off take effect, the noise generator steps,
//! and reverb (at 22.05 kHz, with its resamplers) is mixed in. Key on takes
//! four samples to start a voice. SPUSTAT is latched once per sample. The
//! interrupt is asserted when a voice's decoder, a transfer or a capture or
//! reverb access touches the IRQ address, and held until SPUCNT bit 6 is
//! cleared.

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

const ATTACK: u32 = 0;
const DECAY: u32 = 1;
const SUSTAIN: u32 = 2;
const RELEASE: u32 = 3;

/// The ADPCM filters (5-15 decode as 0).
const WEIGHTS: [[i32; 2]; 16] = [
    [0, 0],
    [60, 0],
    [115, -52],
    [98, -55],
    [122, -60],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
    [0, 0],
];

/// Reverb resampler taps, without the zeros and the middle (16384).
const RESAMP: [i32; 20] =
    [-1, 2, -10, 35, -103, 266, -616, 1332, -2960, 10246, 10246, -2960, 1332, -616, 266, -103, 35, -10, 2, -1];

/// Mednafen's `CalcVCDelta`: the envelope step and divider increment for a rate.
fn vc_delta(zs: u32, speed: u32, log: bool, dec: bool, inv_inc: bool, current: i16) -> (i32, i32) {
    let mut inc = 7 - (speed & 3) as i32;
    if inv_inc {
        inc = !inc;
    }
    let mut div = 32768i32;
    if speed < 0x2C {
        inc = (inc as u32).wrapping_shl((0x2F - speed) >> 2) as i32;
    }
    if speed >= 0x30 {
        div >>= (speed - 0x2C) >> 2;
    }
    if log {
        if dec {
            inc = (current as i32 * inc) >> 15;
        } else if (current as i32 & 0x7FFF) >= 0x6000 {
            if speed < 0x28 {
                inc >>= 2;
            } else if speed >= 0x2C {
                div >>= 2;
            } else {
                inc >>= 1;
                div >>= 1;
            }
        }
    }
    if div == 0 && speed < zs {
        div = 1;
    }
    (inc, div)
}

/// A volume register: fixed, or sweeping (Mednafen's `SPU_Sweep`).
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Sweep {
    control: u16,
    current: u16,
    divider: u32,
}

impl Sweep {
    fn volume(&self) -> i32 {
        self.current as i16 as i32
    }

    fn clock(&mut self) {
        let c = self.control;
        if c & 0x8000 == 0 {
            self.current = (c & 0x7FFF) << 1;
            return;
        }
        let log = c & 0x4000 != 0;
        let dec = c & 0x2000 != 0;
        let inv = c & 0x1000 != 0;
        let inv_inc = (dec ^ inv) | (dec & log);
        let xor = if inv && !(dec && log) { 0xFFFF } else { 0 };
        let test_invert = if inv { 0xFFFF } else { 0 };
        let (inc, div) = vc_delta(0x7F, (c & 0x7F) as u32, log, dec, inv_inc, (self.current ^ xor) as i16);
        if (dec && !(inv && log)) && ((self.current & 0x8000) == (if inv { 0 } else { 0x8000 }) || self.current == 0) {
            self.current = 0;
        } else {
            self.divider = self.divider.wrapping_add(div as u32);
            if self.divider & 0x8000 != 0 {
                self.divider = 0;
                if dec || (self.current ^ test_invert) != 0x7FFF {
                    let prev = self.current;
                    self.current = (self.current as i32).wrapping_add(inc) as u16;
                    if !dec && (self.current ^ prev) & 0x8000 != 0 && (self.current ^ test_invert) & 0x8000 != 0 {
                        self.current = 0x7FFF ^ test_invert;
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Adsr {
    level: u16,
    divider: u32,
    phase: u32,
    attack_exp: bool,
    sustain_exp: bool,
    sustain_dec: bool,
    release_exp: bool,
    attack_rate: u32,
    decay_rate: u32,
    sustain_rate: u32,
    release_rate: u32,
    sustain_level: u32,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Voice {
    buf: [i16; 32],
    m2: i16,
    m1: i16,
    play_delay: u32,
    write_pos: u32,
    read_pos: u32,
    avail: u32,
    shift: u8,
    weight: u8,
    flags: u8,
    ignore_loop: bool,
    sweep: [Sweep; 2],
    pitch: u16,
    phase: u32,
    /// Halfword addresses in sound RAM.
    start: u32,
    cur: u32,
    loop_addr: u32,
    adsr_ctrl: u32,
    /// After enveloping, before the L/R volumes.
    pre: i32,
    adsr: Adsr,
}

impl Voice {
    /// Mednafen's `CacheEnvelope`.
    fn cache_envelope(&mut self) {
        let raw = self.adsr_ctrl;
        let a = &mut self.adsr;
        a.attack_exp = raw & (1 << 15) != 0;
        a.release_exp = raw & (1 << 21) != 0;
        a.sustain_exp = raw & (1 << 31) != 0;
        a.sustain_dec = raw & (1 << 30) != 0;
        a.attack_rate = (raw >> 8) & 0x7F;
        a.decay_rate = ((raw >> 4) & 0xF) << 2;
        a.sustain_rate = (raw >> 22) & 0x7F;
        a.release_rate = ((raw >> 16) & 0x1F) << 2;
        a.sustain_level = ((raw & 0xF) + 1) << 11;
    }

    fn run_envelope(&mut self) {
        let a = &mut self.adsr;
        if a.phase == ATTACK && a.level == 0x7FFF {
            a.phase += 1;
        }
        let level = a.level as i16;
        let ((inc, div), reset) = match a.phase {
            DECAY => (vc_delta(0x1F << 2, a.decay_rate, true, true, true, level), 0),
            SUSTAIN => (
                vc_delta(0x7F, a.sustain_rate, a.sustain_exp, a.sustain_dec, a.sustain_dec, level),
                if a.sustain_dec { 0 } else { 0x7FFF },
            ),
            RELEASE => (vc_delta(0x1F << 2, a.release_rate, a.release_exp, true, true, level), 0),
            _ => (vc_delta(0x7F, a.attack_rate, a.attack_exp, false, false, level), 0x7FFF),
        };
        a.divider = a.divider.wrapping_add(div as u32);
        if a.divider & 0x8000 != 0 {
            let prev = a.level;
            a.divider = 0;
            a.level = (a.level as i32).wrapping_add(inc) as u16;
            if a.phase == ATTACK {
                if (prev ^ a.level) & a.level & 0x8000 != 0 {
                    a.level = reset;
                }
            } else if a.level & 0x8000 != 0 {
                a.level = reset;
            }
            if a.phase == DECAY && (a.level as u32) < a.sustain_level {
                a.phase += 1;
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Spu {
    pub ram: Vec<u8>,
    voices: Vec<Voice>,
    noise_divider: u32,
    noise_counter: u32,
    lfsr: u16,
    fm_mode: u32,
    noise_mode: u32,
    reverb_mode: u32,
    reverb_wa: u32,
    main: [Sweep; 2],
    reverb_vol: [i32; 2],
    cd_vol: [i32; 2],
    extern_vol: [i32; 2],
    irq_addr: u32,
    rw_addr: u32,
    control: u16,
    voice_on: u32,
    voice_off: u32,
    block_end: u32,
    /// Halfword position in the capture buffers (0..1FFh).
    cwa: u32,
    /// The register file as written (0x000-0x1FF); SPUSTAT is latched into
    /// 0x1AE each sample, and reverb reads its registers from 0x1C0-0x1FF.
    regs: Vec<u16>,
    aux: [u16; 16],
    rdsb: [Vec<i16>; 2],
    rusb: [Vec<i16>; 2],
    rvb_res_pos: u32,
    reverb_cur: u32,
    irq_asserted: bool,
    /// The interrupt line rose since the last call that reports it.
    #[serde(skip)]
    irq_rose: bool,
    clock_divider: i32,
    /// Mixed stereo output (L, R interleaved) since the last [`Spu::take_samples`].
    #[serde(skip)]
    out: Vec<i16>,
    #[serde(skip)]
    pub capture: bool,
    pub keyed_on: u64,
    /// Analysis only: every voice ever given noise (NON) or pitch modulation
    /// (PMON), the number of SPU interrupts raised, and ADPCM blocks a
    /// sounding voice fetched with a reserved shift (13-15) or filter (5-15).
    #[serde(skip)]
    pub noise_seen: u32,
    #[serde(skip)]
    pub pmon_seen: u32,
    #[serde(skip)]
    pub irqs: u64,
    #[serde(skip)]
    pub reserved_blocks: u64,
}

impl Default for Spu {
    fn default() -> Self {
        Spu {
            ram: vec![0; RAM_SIZE],
            voices: vec![Voice::default(); 24],
            noise_divider: 0,
            noise_counter: 0,
            lfsr: 0,
            fm_mode: 0,
            noise_mode: 0,
            reverb_mode: 0,
            reverb_wa: 0,
            main: [Sweep::default(); 2],
            reverb_vol: [0; 2],
            cd_vol: [0; 2],
            extern_vol: [0; 2],
            irq_addr: 0,
            rw_addr: 0,
            control: 0,
            voice_on: 0,
            voice_off: 0,
            block_end: 0,
            cwa: 0,
            regs: vec![0; 0x100],
            aux: [0; 16],
            rdsb: [vec![0; 128], vec![0; 128]],
            rusb: [vec![0; 64], vec![0; 64]],
            rvb_res_pos: 0,
            reverb_cur: 0,
            irq_asserted: false,
            irq_rose: false,
            clock_divider: 768,
            out: Vec::new(),
            capture: false,
            keyed_on: 0,
            noise_seen: 0,
            pmon_seen: 0,
            irqs: 0,
            reserved_blocks: 0,
        }
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

fn sat16(v: i32) -> i32 {
    v.clamp(-32768, 32767)
}

fn reverb_neg(s: i16) -> i32 {
    if s == -32768 { 0x7FFF } else { -(s as i32) }
}

fn iiasm(alpha: i16, s: i16) -> i32 {
    if alpha == -32768 { if s == -32768 { 0 } else { s as i32 * -65536 } } else { s as i32 * (32768 - alpha as i32) }
}

impl Spu {
    /// Voices with a non-zero envelope.
    pub fn active_voices(&self) -> Vec<VoiceInfo> {
        self.voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.adsr.level != 0)
            .map(|(i, v)| VoiceInfo {
                voice: i,
                phase: ["attack", "decay", "sustain", "release"][v.adsr.phase as usize & 3].to_string(),
                level: v.adsr.level as i16 as i32,
                vol_l: v.sweep[0].control,
                vol_r: v.sweep[1].control,
                pitch: v.pitch,
                start: v.start << 1,
                addr: v.cur << 1,
                adsr: v.adsr_ctrl,
            })
            .collect()
    }

    /// Voices past their END block since their last key on (ENDX).
    pub fn endx(&self) -> u32 {
        self.block_end
    }

    /// The next sound RAM byte address a transfer writes.
    pub fn transfer_addr(&self) -> u32 {
        self.rw_addr << 1
    }

    /// A voice's registers as a key on would use them: start (bytes), pitch,
    /// left/right volume registers, ADSR, and whether noise / pitch modulation
    /// (NON/PMON) apply to it.
    pub fn voice_settings(&self, i: usize) -> (u32, u16, [u16; 2], u32, bool, bool) {
        let v = &self.voices[i];
        (
            v.start << 1,
            v.pitch,
            [v.sweep[0].control, v.sweep[1].control],
            v.adsr_ctrl,
            self.noise_mode & (1 << i) != 0,
            i > 0 && self.fm_mode & (1 << i) != 0,
        )
    }

    pub fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.out)
    }

    /// Whether the interrupt line rose since the last call (or the last
    /// write, DMA access or tick, which report it).
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq_rose)
    }

    fn assert_irq(&mut self) {
        if !self.irq_asserted {
            self.irq_rose = true;
            self.irqs += 1;
        }
        self.irq_asserted = true;
    }

    fn check_irq_addr(&mut self, addr: u32) {
        if self.control & 0x40 != 0 && self.irq_addr == addr {
            self.assert_irq();
        }
    }

    fn ram16(&self, a: u32) -> u16 {
        let a = (a as usize & 0x3FFFF) << 1;
        u16::from_le_bytes([self.ram[a], self.ram[a + 1]])
    }

    fn write_ram(&mut self, a: u32, v: u16) {
        self.check_irq_addr(a);
        let o = (a as usize & 0x3FFFF) << 1;
        self.ram[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn read_ram(&mut self, a: u32) -> u16 {
        self.check_irq_addr(a);
        self.ram16(a)
    }

    /// SPUSTAT as last latched.
    pub fn status(&self) -> u16 {
        self.regs[0xD7]
    }

    /// A halfword read at `a` (0x000-0x3FF), Mednafen's `PS_SPU::Read`.
    pub fn read16(&mut self, a: u32) -> u16 {
        let a = a & 0x3FF;
        if a >= 0x200 {
            return if a < 0x260 {
                self.voices[((a - 0x200) >> 2) as usize].sweep[((a & 2) >> 1) as usize].current
            } else if a < 0x280 {
                self.aux[((a & 0x1F) >> 1) as usize]
            } else {
                0xFFFF
            };
        }
        if a < 0x180 {
            let v = &self.voices[(a >> 4) as usize];
            match a & 0xF {
                0xC => return v.adsr.level,
                0xE => return (v.loop_addr >> 2) as u16,
                _ => {}
            }
        } else {
            match a & 0x7F {
                0x1C => return self.block_end as u16,
                0x1E => return (self.block_end >> 16) as u16,
                0x28 => {
                    let v = self.read_ram(self.rw_addr);
                    self.rw_addr = (self.rw_addr + 1) & 0x3FFFF;
                    self.check_irq_addr(self.rw_addr);
                    return v;
                }
                0x2A => return self.control,
                0x3C => return 0,
                0x38 | 0x3A => return self.main[((a & 2) >> 1) as usize].current,
                _ => {}
            }
        }
        self.regs[((a & 0x1FF) >> 1) as usize]
    }

    /// A halfword write at `a`, Mednafen's `PS_SPU::Write`. Returns true when
    /// it raised the SPU interrupt.
    pub fn write16(&mut self, a: u32, v: u16) -> bool {
        let a = a & 0x3FF;
        if a >= 0x200 {
            if a < 0x260 {
                self.voices[((a - 0x200) >> 2) as usize].sweep[((a & 2) >> 1) as usize].current = v;
            } else if a < 0x280 {
                self.aux[((a & 0x1F) >> 1) as usize] = v;
            }
            return false;
        }
        if a < 0x180 {
            let voice = &mut self.voices[(a >> 4) as usize];
            match a & 0xF {
                0x0 | 0x2 => voice.sweep[((a & 2) >> 1) as usize].control = v,
                0x4 => voice.pitch = v,
                0x6 => voice.start = ((v as u32) << 2) & 0x3FFFF,
                0x8 => {
                    voice.adsr_ctrl = (voice.adsr_ctrl & 0xFFFF_0000) | v as u32;
                    voice.cache_envelope();
                }
                0xA => {
                    voice.adsr_ctrl = (voice.adsr_ctrl & 0xFFFF) | (v as u32) << 16;
                    voice.cache_envelope();
                }
                0xC => voice.adsr.level = v,
                0xE => {
                    voice.loop_addr = ((v as u32) << 2) & 0x3FFFF;
                    voice.ignore_loop = true;
                }
                _ => {}
            }
        } else {
            let lo = |r: u32| (r & 0xFFFF_0000) | v as u32;
            let hi = |r: u32| (r & 0xFFFF) | ((v as u32) & 0xFF) << 16;
            match a & 0x7F {
                0x00 | 0x02 => self.main[((a & 2) >> 1) as usize].control = v,
                0x04 => self.reverb_vol[0] = v as i16 as i32,
                0x06 => self.reverb_vol[1] = v as i16 as i32,
                0x08 => {
                    self.voice_on = lo(self.voice_on);
                    self.keyed_on += v.count_ones() as u64;
                }
                0x0A => {
                    self.voice_on = hi(self.voice_on);
                    self.keyed_on += (v & 0xFF).count_ones() as u64;
                }
                0x0C => self.voice_off = lo(self.voice_off),
                0x0E => self.voice_off = hi(self.voice_off),
                0x10 => {
                    self.fm_mode = lo(self.fm_mode);
                    self.pmon_seen |= v as u32 & !1;
                }
                0x12 => {
                    self.fm_mode = hi(self.fm_mode);
                    self.pmon_seen |= (v as u32) << 16;
                }
                0x14 => {
                    self.noise_mode = lo(self.noise_mode);
                    self.noise_seen |= v as u32;
                }
                0x16 => {
                    self.noise_mode = hi(self.noise_mode);
                    self.noise_seen |= (v as u32) << 16;
                }
                0x18 => self.reverb_mode = lo(self.reverb_mode),
                0x1A => self.reverb_mode = hi(self.reverb_mode),
                0x1C => self.block_end = (self.block_end & 0xFFFF_0000) | v as u32,
                0x1E => self.block_end = (self.block_end & 0xFFFF) | (v as u32) << 16,
                0x22 => {
                    self.reverb_wa = ((v as u32) << 2) & 0x3FFFF;
                    self.reverb_cur = self.reverb_wa;
                }
                0x24 => {
                    self.irq_addr = ((v as u32) << 2) & 0x3FFFF;
                    self.check_irq_addr(self.rw_addr);
                }
                0x26 => {
                    self.rw_addr = ((v as u32) << 2) & 0x3FFFF;
                    self.check_irq_addr(self.rw_addr);
                }
                0x28 => {
                    self.write_ram(self.rw_addr, v);
                    self.rw_addr = (self.rw_addr + 1) & 0x3FFFF;
                    self.check_irq_addr(self.rw_addr);
                }
                0x2A => {
                    self.control = v;
                    if v & 0x40 == 0 {
                        self.irq_asserted = false;
                    }
                    self.check_irq_addr(self.rw_addr);
                }
                0x30 => self.cd_vol[0] = v as i16 as i32,
                0x32 => self.cd_vol[1] = v as i16 as i32,
                0x34 => self.extern_vol[0] = v as i16 as i32,
                0x36 => self.extern_vol[1] = v as i16 as i32,
                0x38 | 0x3A => self.main[((a & 2) >> 1) as usize].current = v,
                _ => {}
            }
        }
        self.regs[((a & 0x1FF) >> 1) as usize] = v;
        self.take_irq()
    }

    /// A DMA write of one word (Mednafen's `WriteDMA`); returns whether it
    /// raised the interrupt.
    pub fn dma_write(&mut self, v: u32) -> bool {
        self.write_ram(self.rw_addr, v as u16);
        self.rw_addr = (self.rw_addr + 1) & 0x3FFFF;
        self.write_ram(self.rw_addr, (v >> 16) as u16);
        self.rw_addr = (self.rw_addr + 1) & 0x3FFFF;
        self.check_irq_addr(self.rw_addr);
        self.take_irq()
    }

    /// A DMA read of one word (Mednafen's `ReadDMA`), and whether it raised
    /// the interrupt.
    pub fn dma_read(&mut self) -> (u32, bool) {
        let lo = self.read_ram(self.rw_addr) as u32;
        self.rw_addr = (self.rw_addr + 1) & 0x3FFFF;
        let hi = self.read_ram(self.rw_addr) as u32;
        self.rw_addr = (self.rw_addr + 1) & 0x3FFFF;
        self.check_irq_addr(self.rw_addr);
        (lo | hi << 16, self.take_irq())
    }

    /// Advance by `cycles`, pulling one CD audio frame per output sample from
    /// `cd`; returns true if the SPU interrupt rose.
    pub fn tick(&mut self, cycles: u64, cd: &mut std::collections::VecDeque<(i16, i16)>) -> bool {
        self.tick_with(cycles, || cd.pop_front().unwrap_or((0, 0)))
    }

    /// Advance by `cycles`, taking each output sample's CD audio input from
    /// `cd` (Mednafen's `UpdateFromCDC`).
    pub fn tick_with(&mut self, cycles: u64, mut cd: impl FnMut() -> (i16, i16)) -> bool {
        let mut samples = 0;
        self.clock_divider -= cycles as i32;
        while self.clock_divider <= 0 {
            self.clock_divider += CYCLES_PER_SAMPLE as i32;
            samples += 1;
        }
        for _ in 0..samples {
            let c = cd();
            self.sample(c);
        }
        self.take_irq()
    }

    /// System clocks until the next output sample (Mednafen's `clock_divider`).
    pub fn clocks_to_sample(&self) -> u64 {
        self.clock_divider as u64
    }

    /// Mednafen's `RunDecoder`.
    fn run_decoder(&mut self, n: usize) {
        if self.voices[n].avail >= 11 {
            if self.control & 0x40 != 0 {
                let t = self.voices[n].cur.wrapping_sub(1) & 0x3FFFF;
                if self.irq_addr == t || self.irq_addr == t & 0x3FFF8 {
                    self.assert_irq();
                }
            }
            return;
        }
        if self.voices[n].cur & 7 == 0 && self.voices[n].flags & 1 != 0 {
            let v = &mut self.voices[n];
            v.cur = v.loop_addr & !7;
            self.block_end |= 1 << n;
            if v.flags & 2 == 0 && self.noise_mode & (1 << n) == 0 {
                v.adsr.phase = RELEASE;
                v.adsr.level = 0;
            }
        }
        if self.control & 0x40 != 0 {
            let t = self.voices[n].cur & 0x3FFFF;
            if self.irq_addr == t || self.irq_addr == t & 0x3FFF8 {
                self.assert_irq();
            }
        }
        if self.voices[n].cur & 7 == 0 {
            let cv = self.ram16(self.voices[n].cur);
            let v = &mut self.voices[n];
            v.shift = (cv & 0xF) as u8;
            v.weight = ((cv >> 4) & 0xF) as u8;
            v.flags = (cv >> 8) as u8;
            if (v.shift > 12 || v.weight > 4) && v.adsr.level != 0 {
                self.reserved_blocks += 1;
            }
            if v.flags & 4 != 0 && !v.ignore_loop {
                v.loop_addr = v.cur;
            }
            v.cur = (v.cur + 1) & 0x3FFFF;
        }
        let mut cv = self.ram16(self.voices[n].cur);
        let v = &mut self.voices[n];
        let [w1, w2] = WEIGHTS[v.weight as usize];
        let mut shift = v.shift as u32;
        if shift > 12 {
            shift = 8;
            cv &= 0x8888;
        }
        let mut coded = (cv as u32) << 12;
        for i in 0..4 {
            let mut s = ((coded & 0xF000) as u16 as i16 as i32) >> shift;
            s += (v.m2 as i32 * w2) >> 6;
            s += (v.m1 as i32 * w1) >> 6;
            let s = sat16(s) as i16;
            v.buf[(v.write_pos as usize + i) & 0x1F] = s;
            v.m2 = v.m1;
            v.m1 = s;
            coded >>= 4;
        }
        v.write_pos = (v.write_pos + 4) & 0x1F;
        v.avail += 4;
        v.cur = (v.cur + 1) & 0x3FFFF;
    }

    fn run_noise(&mut self) {
        let rf = ((self.control >> 8) & 0x3F) as u32;
        let (mut div_inc, mut cnt_inc) = (2u32 << (rf >> 2), 4 + (rf & 3));
        if rf >= 0x3C {
            div_inc = 0x8000;
            cnt_inc = 8;
        }
        self.noise_divider += div_inc;
        if self.noise_divider & 0x8000 != 0 {
            self.noise_divider = 0;
            self.noise_counter += cnt_inc;
            if self.noise_counter & 8 != 0 {
                self.noise_counter &= 7;
                let l = self.lfsr;
                self.lfsr = (l << 1) | (((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1);
            }
        }
    }

    fn reverb_offset(&self, off: u32) -> u32 {
        let mut o = self.reverb_cur.wrapping_add(off & 0x3FFFF);
        o = o.wrapping_add(self.reverb_wa & (((o << 13) as i32) >> 31) as u32);
        o & 0x3FFFF
    }

    fn rd_rvb(&mut self, raw: u16, extra: i32) -> i16 {
        let a = self.reverb_offset(((raw as u32) << 2).wrapping_add(extra as u32));
        self.read_ram(a) as i16
    }

    fn wr_rvb(&mut self, raw: u16, s: i16) {
        if self.control & 0x80 != 0 {
            let a = self.reverb_offset((raw as u32) << 2);
            self.write_ram(a, s as u16);
        }
    }

    /// Mednafen's `RunReverb`.
    fn run_reverb(&mut self, input: [i32; 2]) -> [i32; 2] {
        let pos = self.rvb_res_pos as usize;
        for (ring, &x) in self.rdsb.iter_mut().zip(&input) {
            ring[pos] = x as i16;
            ring[pos | 0x40] = x as i16;
        }
        let mut up = [0i32; 2];
        let r = |s: &Self, i: usize| s.regs[0xE0 + i];
        let rs = |s: &Self, i: usize| s.regs[0xE0 + i] as i16 as i32;
        if pos & 1 != 0 {
            let mut down = [0i32; 2];
            for (lr, d) in down.iter_mut().enumerate() {
                let base = (pos.wrapping_sub(38)) & 0x3F;
                let src = &self.rdsb[lr][base..];
                let mut out: i32 = (0..20).map(|i| RESAMP[i] * src[i * 2] as i32).sum();
                out += 0x4000 * src[19] as i32;
                *d = sat16(out >> 15);
            }
            let (fb_src_a, fb_src_b) = (r(self, 0), r(self, 1));
            let (iir_alpha, iir_coef) = (rs(self, 2) as i16, rs(self, 7));
            let (acc_a, acc_b, acc_c, acc_d) = (rs(self, 3), rs(self, 4), rs(self, 5), rs(self, 6));
            let (fb_alpha, fb_x) = (rs(self, 8) as i16, rs(self, 9) as i16);
            for (lr, &down) in down.iter().enumerate() {
                let in_coef = rs(self, 30 + lr);
                let a = self.rd_rvb(r(self, 16 + lr), 0) as i32;
                let iir_in_a = sat16((((a * iir_coef) >> 14) + ((down * in_coef) >> 14)) >> 1) as i16;
                let b = self.rd_rvb(r(self, 24 + (lr ^ 1)), 0) as i32;
                let iir_in_b = sat16((((b * iir_coef) >> 14) + ((down * in_coef) >> 14)) >> 1) as i16;
                let da = self.rd_rvb(r(self, 10 + lr), -1);
                let iir_a = sat16((((iir_in_a as i32 * iir_alpha as i32) >> 14) + (iiasm(iir_alpha, da) >> 14)) >> 1);
                let db = self.rd_rvb(r(self, 18 + lr), -1);
                let iir_b = sat16((((iir_in_b as i32 * iir_alpha as i32) >> 14) + (iiasm(iir_alpha, db) >> 14)) >> 1);
                self.wr_rvb(r(self, 10 + lr), iir_a as i16);
                self.wr_rvb(r(self, 18 + lr), iir_b as i16);
                let acc = ((self.rd_rvb(r(self, 12 + lr), 0) as i32 * acc_a) >> 14)
                    + ((self.rd_rvb(r(self, 14 + lr), 0) as i32 * acc_b) >> 14)
                    + ((self.rd_rvb(r(self, 20 + lr), 0) as i32 * acc_c) >> 14)
                    + ((self.rd_rvb(r(self, 22 + lr), 0) as i32 * acc_d) >> 14);
                let fb_a = self.rd_rvb(r(self, 26 + lr).wrapping_sub(fb_src_a), 0) as i32;
                let fb_b = self.rd_rvb(r(self, 28 + lr).wrapping_sub(fb_src_b), 0) as i32;
                let mda = sat16((acc + ((fb_a * reverb_neg(fb_alpha)) >> 14)) >> 1);
                let mdb = sat16(fb_a + ((((mda * fb_alpha as i32) >> 14) + ((fb_b * reverb_neg(fb_x)) >> 14)) >> 1));
                let ivb = sat16(fb_b + ((mdb * fb_x as i32) >> 15));
                self.wr_rvb(r(self, 26 + lr), mda as i16);
                self.wr_rvb(r(self, 28 + lr), mdb as i16);
                self.rusb[lr][(pos >> 1) | 0x20] = ivb as i16;
                self.rusb[lr][pos >> 1] = ivb as i16;
            }
            self.reverb_cur = (self.reverb_cur + 1) & 0x3FFFF;
            if self.reverb_cur == 0 {
                self.reverb_cur = self.reverb_wa;
            }
            for (lr, u) in up.iter_mut().enumerate() {
                let src = &self.rusb[lr][((pos >> 1).wrapping_sub(19)) & 0x1F..];
                let out: i32 = (0..20).map(|i| RESAMP[i] * src[i] as i32).sum();
                *u = sat16(out >> 14);
            }
        } else {
            for (lr, u) in up.iter_mut().enumerate() {
                *u = self.rusb[lr][(((pos >> 1).wrapping_sub(19)) & 0x1F) + 9] as i32;
            }
        }
        self.rvb_res_pos = (self.rvb_res_pos + 1) & 0x3F;
        up
    }

    /// One output sample (the body of Mednafen's `UpdateFromCDC` loop).
    fn sample(&mut self, cd: (i16, i16)) {
        let mut accum = [0i32; 2];
        let mut accum_fv = [0i32; 2];
        let mut status = self.control & 0x3F;
        if self.irq_asserted {
            status |= 0x40;
        }
        if self.regs[0xD6] == 4 && self.cwa & 0x100 != 0 {
            status |= 0x800;
        }
        self.regs[0xD7] = status;
        let fm = self.fm_mode & !1;
        for n in 0..24 {
            self.voices[n].pre = 0;
            if self.voices[n].play_delay != 0 {
                self.voices[n].ignore_loop = false;
            }
            self.run_decoder(n);
            let lfsr = self.lfsr;
            let v = &self.voices[n];
            let mut pvs = if self.noise_mode & (1 << n) != 0 {
                lfsr as i16 as i32
            } else {
                let si = v.read_pos as usize;
                let pi = ((v.phase & 0xFFF) >> 4) as usize;
                let b = |k: usize| v.buf[(si + k) & 0x1F] as i32;
                (b(0) * GAUSS[0xFF - pi] + b(1) * GAUSS[0x1FF - pi] + b(2) * GAUSS[0x100 + pi] + b(3) * GAUSS[pi]) >> 15
            };
            pvs = (pvs * v.adsr.level as i16 as i32) >> 15;
            self.voices[n].pre = pvs;
            if n == 1 || n == 3 {
                let a = 0x400 | ((n as u32 >> 1) * 0x200) | self.cwa;
                self.write_ram(a, pvs as u16);
            }
            let v = &self.voices[n];
            let l = (pvs * v.sweep[0].volume()) >> 15;
            let r = (pvs * v.sweep[1].volume()) >> 15;
            accum[0] += l;
            accum[1] += r;
            if self.reverb_mode & (1 << n) != 0 {
                accum_fv[0] += l;
                accum_fv[1] += r;
            }
            let prev_pre = if n > 0 { self.voices[n - 1].pre } else { 0 };
            let v = &mut self.voices[n];
            v.sweep[0].clock();
            v.sweep[1].clock();
            if v.play_delay == 0 {
                v.run_envelope();
                let mut inc = if fm & (1 << n) != 0 {
                    (v.pitch as i32 + ((v.pitch as i16 as i32 * prev_pre) >> 15)) as u32
                } else {
                    v.pitch as u32
                };
                if inc > 0x3FFF {
                    inc = 0x3FFF;
                }
                let t = v.phase + inc;
                let used = t >> 12;
                v.phase = t & 0xFFF;
                v.avail = v.avail.wrapping_sub(used);
                v.read_pos = (v.read_pos + used) & 0x1F;
            } else {
                v.play_delay -= 1;
            }
            if self.voice_off & (1 << n) != 0 && v.adsr.phase != RELEASE && v.play_delay < 3 {
                v.adsr.divider = 0;
                v.adsr.phase = RELEASE;
            }
            if self.voice_on & (1 << n) != 0 {
                v.adsr.level = 0;
                v.adsr.divider = 0;
                v.adsr.phase = ATTACK;
                v.flags = 0;
                v.write_pos = 0;
                v.read_pos = 0;
                v.avail = 0;
                v.play_delay = 4;
                self.block_end &= !(1 << n);
                v.m2 = 0;
                v.m1 = 0;
                v.phase = 0;
                v.cur = v.start & !7;
                v.ignore_loop = false;
            }
            if self.control & 0x8000 == 0 {
                v.adsr.phase = RELEASE;
                v.adsr.level = 0;
            }
        }
        self.voice_off = 0;
        self.voice_on = 0;
        if self.control & 0x4000 == 0 {
            accum = [0; 2];
            accum_fv = [0; 2];
        }
        let raw = [cd.0 as i32, cd.1 as i32];
        self.write_ram(self.cwa, raw[0] as u16);
        self.write_ram(self.cwa | 0x200, raw[1] as u16);
        if self.control & 1 != 0 {
            for lr in 0..2 {
                let v = (raw[lr] * self.cd_vol[lr]) >> 15;
                accum[lr] += v;
                if self.control & 4 != 0 {
                    accum_fv[lr] += v;
                }
            }
        }
        self.cwa = (self.cwa + 1) & 0x1FF;
        self.run_noise();
        let reverb = self.run_reverb([sat16(accum_fv[0]), sat16(accum_fv[1])]);
        for lr in 0..2 {
            accum[lr] = sat16(accum[lr] + ((reverb[lr] * self.reverb_vol[lr]) >> 15));
            let out = sat16((accum[lr] * self.main[lr].volume()) >> 15);
            if self.capture {
                self.out.push(out as i16);
            }
        }
        self.main[0].clock();
        self.main[1].clock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauss_rows_match_mednafen_fir_table() {
        let row = |pi: usize| [GAUSS[0xFF - pi], GAUSS[0x1FF - pi], GAUSS[0x100 + pi], GAUSS[pi]];
        assert_eq!(row(0), [0x12C7, 0x59B3, 0x1307, -1]);
        assert_eq!(row(127), [0x01A8, 0x3E4C, 0x3DEF, 0x019C]);
        assert_eq!(row(255), [-1, 0x1307, 0x59B3, 0x12C7]);
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
    fn keyed_voice_plays_looped_square_wave() {
        let mut s = spu_with_square(&[0]);
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 200, &mut Default::default());
        let out = s.take_samples();
        assert_eq!(out.len(), 400);
        assert!(out.iter().any(|&x| x > 1000) && out.iter().any(|&x| x < -1000), "audible square wave");
        assert_eq!(s.read16(0x19C) & 1, 1, "ENDX set after loop end");
        // Key on clears ENDX at the next sample.
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE, &mut Default::default());
        assert_eq!(s.read16(0x19C) & 1, 0);
        // Muting silences the voices.
        s.write16(0x1AA, 0x8000);
        s.take_samples();
        s.tick(CYCLES_PER_SAMPLE * 50, &mut Default::default());
        assert!(s.take_samples().iter().all(|&x| x == 0));
    }

    #[test]
    fn attack_reaches_full_level_then_releases_to_zero() {
        let mut s = spu_with_square(&[0]);
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 100, &mut Default::default());
        assert_eq!(s.read16(0x00C), 0x7FFF, "attack rate 0 is immediate");
        s.write16(0x00A, 0x0005); // release rate 5, linear
        s.write16(0x18C, 1);
        s.tick(CYCLES_PER_SAMPLE * 2000, &mut Default::default());
        assert_eq!(s.read16(0x00C), 0);
    }

    #[test]
    fn spustat_is_latched_each_sample() {
        let mut s = Spu::default();
        s.write16(0x1AA, 0x0020);
        assert_eq!(s.read16(0x1AE), 0, "not before the next sample");
        s.tick(CYCLES_PER_SAMPLE, &mut Default::default());
        assert_eq!(s.read16(0x1AE), 0x20);
    }

    #[test]
    fn transfer_to_the_irq_address_raises_the_interrupt_once() {
        let mut s = Spu::default();
        s.write16(0x1A4, 0x0100); // IRQ at halfword 400h
        s.write16(0x1A6, 0x00FF); // transfers from halfword 3FCh
        s.write16(0x1AA, 0x0040);
        for _ in 0..3 {
            assert!(!s.write16(0x1A8, 0));
        }
        assert!(s.write16(0x1A8, 0), "the transfer address reaches the IRQ address");
        assert!(!s.write16(0x1A8, 0), "held, not raised again");
        s.write16(0x1AA, 0); // acknowledge
        s.write16(0x1A6, 0x00FF);
        s.write16(0x1AA, 0x0040);
        assert!(!s.dma_write(0));
        assert!(s.dma_write(0), "a DMA write there");
    }
}

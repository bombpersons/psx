//! Sound processing unit (psx-spx "Sound Processing Unit (SPU)").
//!
//! Modelled: 512 KiB sound RAM; manual FIFO and DMA-4 transfers; 24 voices
//! decoding SPU-ADPCM (shift/filter, loop start/end/repeat flags, ENDX);
//! pitch stepping (4.12 counter); ADSR envelopes with the documented
//! step/shift rate formula; key on/off; fixed voice and main volumes; the
//! IRQ address check on voice block fetches and transfers; SPUCNT/SPUSTAT;
//! 44.1 kHz stereo mixing into a capture buffer.
//!
//! Approximations: linear (not Gaussian) interpolation. Not modelled yet:
//! reverb, noise, pitch modulation, volume sweeps, CD-DA/XA input, capture buffers.

pub const RAM_SIZE: usize = 512 * 1024;
/// System clocks per output sample (33.8688 MHz / 44.1 kHz).
pub const CYCLES_PER_SAMPLE: u64 = 768;

const FILTER_POS: [i32; 5] = [0, 60, 115, 98, 122];
const FILTER_NEG: [i32; 5] = [0, 0, -52, -55, -60];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Phase {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
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
    /// Previous decoded sample (for interpolation) and ADPCM history.
    prev: i16,
    hist: [i32; 2],
    phase: Phase,
    level: i32,
    env_wait: u32,
    /// Block header flags of the current block.
    flags: u8,
    decoded: bool,
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
}

/// The reverb unit (psx-spx "SPU Reverb Formula"): runs at 22.05 kHz on a
/// ring buffer in sound RAM from mBASE to the end, reading/writing it with
/// offsets from the reverb registers. Simplified: the hardware's 39-tap FIR
/// resampling is replaced by averaging two input samples and holding the
/// output for two.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Reverb {
    /// Current buffer address (bytes).
    addr: u32,
    /// Second sample of the 22.05 kHz pair.
    odd: bool,
    /// Input held from the first sample of the pair.
    held_in: (i32, i32),
    out: (i32, i32),
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

impl Reverb {
    /// One 22.05 kHz step for both sides; writes the buffer only when `write` (SPUCNT bit 7).
    fn step(&mut self, ram: &mut [u8], regs: &[u16], write: bool, input: (i32, i32)) -> (i32, i32) {
        let reg = |o: usize| regs[o >> 1] as i16 as i32;
        let off = |o: usize| regs[o >> 1] as i64 * 8;
        let base = regs[rv::M_BASE >> 1] as i64 * 8;
        let size = 0x80000 - base;
        let cur = self.addr as i64;
        let at = |o: i64| -> usize { (base + (cur - base + o).rem_euclid(size.max(2))) as usize & 0x7FFFE };
        let rd = |ram: &[u8], o: i64| i16::from_le_bytes([ram[at(o)], ram[at(o) + 1]]) as i32;
        let wr = |ram: &mut [u8], o: i64, v: i32| {
            if write {
                let a = at(o);
                ram[a..a + 2].copy_from_slice(&(sat(v) as i16).to_le_bytes());
            }
        };
        let (iir, wall) = (reg(rv::V_IIR), reg(rv::V_WALL));
        let mut out = [0i32; 2];
        for side in 0..2 {
            let other = 1 - side;
            let x = mul15(if side == 0 { input.0 } else { input.1 }, reg(rv::V_IN[side]));
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
            out[side] = sat(mul15(o, reg(rv::V_OUT[side])));
        }
        let next = (self.addr + 2) & 0x7FFFE;
        self.addr = next.max(base as u32);
        (out[0], out[1])
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
        }
    }
}

/// One envelope step at `level`: `(samples until the next step, level change)`.
/// Shared by ADSR and volume sweep (psx-spx "SPU Volume and ADSR Generator").
fn env_step(exp: bool, increase: bool, shift: i32, step: i32, level: i32) -> (u32, i32) {
    let mut cycles = 1u32 << (shift - 11).max(0);
    let mut delta = step << (11 - shift).max(0);
    if exp && increase && level > 0x6000 {
        cycles *= 4;
    }
    if exp && !increase {
        // An arithmetic shift (rounding down), as the hardware does: dividing
        // rounds toward zero, which stalls every exponential decrease once
        // the level drops below 0x1000 (voices then hold at 4095 forever).
        delta = (delta * level) >> 15;
    }
    (cycles, delta)
}

/// A volume register: fixed (bit 15 clear: 15-bit signed volume ×2) or a
/// sweep (bit 15 set) that ramps the current level every sample.
/// Sweep phase (bit 12, negative output) is not modelled.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Volume {
    raw: u16,
    /// Current level (what the current-volume registers read back).
    level: i32,
    wait: u32,
}

impl Volume {
    fn write(&mut self, v: u16) {
        self.raw = v;
        if v & 0x8000 == 0 {
            self.level = ((v << 1) as i16) as i32;
        }
        self.wait = 0;
    }

    #[inline]
    fn tick(&mut self) {
        if self.raw & 0x8000 == 0 {
            return;
        }
        if self.wait > 0 {
            self.wait -= 1;
            return;
        }
        let (exp, decrease) = (self.raw & 0x4000 != 0, self.raw & 0x2000 != 0);
        let r = (self.raw & 0x7F) as i32;
        let step = if decrease { -8 + (r & 3) } else { 7 - (r & 3) };
        let (cycles, delta) = env_step(exp, !decrease, r >> 2, step, self.level.max(0));
        self.wait = cycles - 1;
        self.level = (self.level.max(0) + delta).clamp(0, 0x7FFF);
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

    pub fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.out)
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
            _ => self.regs[(off >> 1) as usize & 0xFF],
        }
    }

    /// Returns true when this write raised the SPU interrupt.
    pub fn write16(&mut self, off: u32, v: u16) -> bool {
        self.regs[(off >> 1) as usize & 0xFF] = v;
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
                    _ => voice.repeat = v,
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
            _ => {}
        }
        false
    }

    fn check_irq(&mut self, addr: u32, len: u32) -> bool {
        let irq = (self.irq_addr as u32) << 3;
        if self.cnt & (1 << 6) != 0 && !self.irq_flag && irq >= addr && irq < addr + len {
            self.irq_flag = true;
            return true;
        }
        false
    }

    /// Manual transfer / DMA-4 write of one halfword at the transfer address.
    pub fn fifo_write(&mut self, v: u16) -> bool {
        let a = self.xfer_addr as usize & (RAM_SIZE - 1);
        self.ram[a..a + 2].copy_from_slice(&v.to_le_bytes());
        self.xfer_addr = (self.xfer_addr + 2) & (RAM_SIZE as u32 - 1);
        self.check_irq(a as u32, 2)
    }

    pub fn fifo_read(&mut self) -> u16 {
        let a = self.xfer_addr as usize & (RAM_SIZE - 1);
        self.xfer_addr = (self.xfer_addr + 2) & (RAM_SIZE as u32 - 1);
        u16::from_le_bytes([self.ram[a], self.ram[a + 1]])
    }

    fn key_on(&mut self, mask: u32) {
        for i in 0..24 {
            if mask & (1 << i) != 0 {
                let v = &mut self.voices[i];
                v.addr = (v.start as u32) << 3;
                v.counter = 0;
                v.hist = [0, 0];
                v.prev = 0;
                v.decoded = false;
                v.phase = Phase::Attack;
                v.level = 0;
                v.env_wait = 0;
                self.endx &= !(1 << i);
                self.keyed_on += 1;
            }
        }
    }

    fn key_off(&mut self, mask: u32) {
        for i in 0..24 {
            if mask & (1 << i) != 0 && self.voices[i].phase != Phase::Off {
                self.voices[i].phase = Phase::Release;
                self.voices[i].env_wait = 0;
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
        let v = &mut self.voices[i];
        let header = self.ram[a];
        let shift = (header & 0xF).min(12) as i32;
        let filter = ((header >> 4) & 7).min(4) as usize;
        v.flags = self.ram[a + 1];
        if v.flags & 4 != 0 {
            v.repeat = (a >> 3) as u16;
        }
        for n in 0..28 {
            let byte = self.ram[a + 2 + n / 2];
            let nib = if n & 1 == 0 { byte & 0xF } else { byte >> 4 };
            let raw = (((nib as i16) << 12) >> shift) as i32;
            let s = raw + (v.hist[0] * FILTER_POS[filter] + v.hist[1] * FILTER_NEG[filter] + 32) / 64;
            let s = s.clamp(-0x8000, 0x7FFF);
            v.hist[1] = v.hist[0];
            v.hist[0] = s;
            v.samples[n] = s as i16;
        }
        v.decoded = true;
        irq
    }

    fn envelope(v: &mut Voice) {
        if v.env_wait > 0 {
            v.env_wait -= 1;
            return;
        }
        let (exp, increase, shift, step, target): (bool, bool, i32, i32, Option<i32>) = match v.phase {
            Phase::Off => return,
            Phase::Attack => {
                let r = ((v.adsr_lo >> 8) & 0x7F) as i32;
                (v.adsr_lo & 0x8000 != 0, true, r >> 2, 7 - (r & 3), Some(0x7FFF))
            }
            Phase::Decay => {
                let sl = (((v.adsr_lo & 0xF) as i32) + 1) * 0x800;
                (true, false, ((v.adsr_lo >> 4) & 0xF) as i32, -8, Some(sl))
            }
            Phase::Sustain => {
                let r = ((v.adsr_hi >> 6) & 0x7F) as i32;
                let dec = v.adsr_hi & 0x4000 != 0;
                (v.adsr_hi & 0x8000 != 0, !dec, r >> 2, if dec { -8 + (r & 3) } else { 7 - (r & 3) }, None)
            }
            Phase::Release => (v.adsr_hi & 0x20 != 0, false, (v.adsr_hi & 0x1F) as i32, -8, Some(0)),
        };
        let (cycles, delta) = env_step(exp, increase, shift, step, v.level);
        v.env_wait = cycles - 1;
        v.level = (v.level + delta).clamp(0, 0x7FFF);
        match (v.phase, target) {
            (Phase::Attack, Some(t)) if v.level >= t => v.phase = Phase::Decay,
            (Phase::Decay, Some(t)) if v.level <= t => v.phase = Phase::Sustain,
            (Phase::Release, _) if v.level == 0 => v.phase = Phase::Off,
            _ => {}
        }
    }

    fn sample(&mut self) -> bool {
        let (mut l, mut r) = (0i32, 0i32);
        // Reverb input: voices with their EON bit set (1F801D98h/9Ah).
        let (mut rl, mut rr) = (0i32, 0i32);
        let eon = self.regs[0x198 >> 1] as u32 | (self.regs[0x19A >> 1] as u32) << 16;
        let mut irq = false;
        for i in 0..24 {
            // Sweeps run whether or not the voice is sounding.
            self.voices[i].vol_l.tick();
            self.voices[i].vol_r.tick();
            if self.voices[i].phase == Phase::Off {
                continue;
            }
            if !self.voices[i].decoded {
                irq |= self.decode_block(i);
            }
            let v = &mut self.voices[i];
            let idx = (v.counter >> 12) as usize;
            let frac = (v.counter & 0xFFF) as i32;
            let cur = v.samples[idx.min(27)] as i32;
            let prev = if idx == 0 { v.prev as i32 } else { v.samples[idx - 1] as i32 };
            let s = prev + (((cur - prev) * frac) >> 12);
            Self::envelope(v);
            let s = s * v.level / 0x8000;
            let (vl, vr) = (s * v.vol_l.level / 0x8000, s * v.vol_r.level / 0x8000);
            l += vl;
            r += vr;
            if eon & (1 << i) != 0 {
                rl += vl;
                rr += vr;
            }
            v.counter += (v.pitch as u32).min(0x4000);
            if v.counter >> 12 >= 28 {
                v.counter -= 28 << 12;
                v.prev = v.samples[27];
                // Block finished: loop end / repeat flags.
                if v.flags & 1 != 0 {
                    self.endx |= 1 << i;
                    if v.flags & 2 != 0 {
                        v.addr = (v.repeat as u32) << 3;
                    } else {
                        v.phase = Phase::Off;
                        v.level = 0;
                    }
                } else {
                    v.addr = v.addr.wrapping_add(16);
                }
                v.decoded = false;
            }
        }
        // CD audio input (SPUCNT bit 0) through the CD volume (0x1DB0/0x1DB2).
        if self.cnt & 1 != 0 {
            let (cl, cr) = self.cd_in;
            let (cl, cr) = (cl as i32 * (self.regs[0x1B0 >> 1] as i16 as i32) / 0x8000, cr as i32 * (self.regs[0x1B2 >> 1] as i16 as i32) / 0x8000);
            l += cl;
            r += cr;
            // SPUCNT bit 2: CD audio into the reverb.
            if self.cnt & 4 != 0 {
                rl += cl;
                rr += cr;
            }
        }
        self.cd_in = (0, 0);
        // Reverb at 22.05 kHz: average each input pair, hold the output for both.
        if self.reverb.odd {
            let (hl, hr) = self.reverb.held_in;
            let input = (sat((hl + rl) / 2), sat((hr + rr) / 2));
            self.reverb.out = self.reverb.step(&mut self.ram, &self.regs, self.cnt & 0x80 != 0, input);
        } else {
            self.reverb.held_in = (rl, rr);
        }
        self.reverb.odd = !self.reverb.odd;
        l += self.reverb.out.0;
        r += self.reverb.out.1;
        self.main_l.tick();
        self.main_r.tick();
        if self.capture {
            let on = self.cnt & 0x8000 != 0 && self.cnt & 0x4000 != 0;
            let mix = |x: i32, m: &Volume| if on { (x * m.level / 0x8000).clamp(-0x8000, 0x7FFF) as i16 } else { 0 };
            self.out.push(mix(l, &self.main_l));
            self.out.push(mix(r, &self.main_r));
        }
        irq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverb_delay_line_round_trips_through_sound_ram() {
        let mut ram = vec![0u8; RAM_SIZE];
        let mut regs = vec![0u16; 256];
        let set = |regs: &mut Vec<u16>, off: usize, v: u16| regs[off >> 1] = v;
        set(&mut regs, rv::M_BASE, 0xE000); // work area from 70000h
        set(&mut regs, rv::V_IN[0], 0x7FFF);
        set(&mut regs, rv::V_OUT[0], 0x7FFF);
        set(&mut regs, rv::V_IIR, 0x7FFF);
        set(&mut regs, rv::V_COMB[0], 0x7FFF);
        set(&mut regs, rv::M_SAME[0], 0x10); // written 64 steps ahead...
        set(&mut regs, rv::M_COMB12[0][0], 0); // ...of where comb 1 reads
        // Keep the other taps clear of that line. With vAPF = 0 each all-pass
        // stage is a pure delay of dAPF (8 bytes = 4 steps).
        set(&mut regs, rv::M_DIFF[0], 0x200);
        set(&mut regs, rv::M_APF1[0], 0x300);
        set(&mut regs, rv::M_APF2[0], 0x380);
        set(&mut regs, rv::D_APF1, 1);
        set(&mut regs, rv::D_APF2, 1);
        let mut r = Reverb { addr: 0x70000, ..Default::default() };
        let mut outs = Vec::new();
        for step in 0..200 {
            let input = if step == 0 { (0x4000, 0) } else { (0, 0) };
            outs.push(r.step(&mut ram, &regs, true, input).0);
        }
        let first = outs.iter().position(|&o| o != 0);
        assert_eq!(first, Some(72), "impulse arrives after 64 + 4 + 4 steps");
        assert!(outs[72] > 0x3000, "{}", outs[72]);
        // Nothing is written with the master enable off.
        let mut quiet = vec![0u8; RAM_SIZE];
        let mut r = Reverb { addr: 0x70000, ..Default::default() };
        r.step(&mut quiet, &regs, false, (0x4000, 0));
        assert!(quiet.iter().all(|&b| b == 0));
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
    fn exponential_release_reaches_silence() {
        // Release: exponential, rate 13 (Croc's boot voices hold at 4095 without the fix).
        let mut v = Voice { phase: Phase::Release, level: 0x7FFF, adsr_hi: 0x20 | 13, ..Default::default() };
        for _ in 0..44_100 * 4 {
            Spu::envelope(&mut v);
        }
        assert_eq!((v.phase, v.level), (Phase::Off, 0));
    }

    #[test]
    fn keyed_voice_plays_looped_square_wave() {
        let mut s = Spu { capture: true, ..Default::default() };
        // One ADPCM block at 0x1000: shift 0, filter 0, loop start+end+repeat, nibbles 7/-8.
        s.write16(0x1A6, 0x1000 >> 3);
        let mut block = [0u8; 16];
        block[0] = 0x00;
        block[1] = 0x07;
        for b in &mut block[2..] {
            *b = 0x87; // low nibble 7 then high nibble 8 (= -8)
        }
        for w in block.chunks(2) {
            s.write16(0x1A8, u16::from_le_bytes([w[0], w[1]]));
        }
        s.write16(0x006, 0x1000 >> 3); // voice 0 start
        s.write16(0x004, 0x1000); // pitch 1.0
        s.write16(0x000, 0x3FFF); // full volume
        s.write16(0x002, 0x3FFF);
        s.write16(0x008, 0x000F); // fastest linear attack, sustain level max
        s.write16(0x00A, 0x0000);
        s.write16(0x180, 0x3FFF);
        s.write16(0x182, 0x3FFF);
        s.write16(0x1AA, 0xC000); // enable, unmute
        s.write16(0x188, 1);
        s.tick(CYCLES_PER_SAMPLE * 200, &mut Default::default());
        let out = s.take_samples();
        assert_eq!(out.len(), 400);
        assert!(out.iter().any(|&x| x > 1000) && out.iter().any(|&x| x < -1000), "audible square wave");
        assert_eq!(s.read16(0x19C) & 1, 1, "ENDX set after loop end");
    }
}

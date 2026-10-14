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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Debug, Default)]
struct Voice {
    vol_l: u16,
    vol_r: u16,
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

pub struct Spu {
    pub ram: Vec<u8>,
    voices: Vec<Voice>,
    main_l: u16,
    main_r: u16,
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
    out: Vec<i16>,
    pub capture: bool,
    pub keyed_on: u64,
}

impl Default for Spu {
    fn default() -> Self {
        Spu {
            ram: vec![0; RAM_SIZE],
            voices: vec![Voice::default(); 24],
            main_l: 0,
            main_r: 0,
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
        }
    }
}

fn vol(v: u16) -> i32 {
    // Fixed mode: 15-bit signed volume ×2. Sweep mode (bit 15) not modelled: hold.
    if v & 0x8000 == 0 { (((v << 1) as i16) as i32).clamp(-0x8000, 0x7FFF) } else { 0x7FFF }
}

impl Spu {
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
                    0x0 => v.vol_l,
                    0x2 => v.vol_r,
                    0x4 => v.pitch,
                    0x6 => v.start,
                    0x8 => v.adsr_lo,
                    0xA => v.adsr_hi,
                    0xC => v.level as u16,
                    _ => v.repeat,
                }
            }
            0x180 => self.main_l,
            0x182 => self.main_r,
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
                    0x0 => voice.vol_l = v,
                    0x2 => voice.vol_r = v,
                    0x4 => voice.pitch = v,
                    0x6 => voice.start = v,
                    0x8 => voice.adsr_lo = v,
                    0xA => voice.adsr_hi = v,
                    0xC => voice.level = v as i16 as i32,
                    _ => voice.repeat = v,
                }
            }
            0x180 => self.main_l = v,
            0x182 => self.main_r = v,
            0x188 => self.key_on(v as u32),
            0x18A => self.key_on((v as u32) << 16),
            0x18C => self.key_off(v as u32),
            0x18E => self.key_off((v as u32) << 16),
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

    /// Advance by `cycles`; returns true if the SPU interrupt fired.
    pub fn tick(&mut self, cycles: u64) -> bool {
        self.cycles += cycles;
        let mut irq = false;
        while self.cycles >= CYCLES_PER_SAMPLE {
            self.cycles -= CYCLES_PER_SAMPLE;
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
        let mut cycles = 1u32 << (shift - 11).max(0);
        let mut delta = step << (11 - shift).max(0);
        if exp && increase && v.level > 0x6000 {
            cycles *= 4;
        }
        if exp && !increase {
            delta = delta * v.level / 0x8000;
        }
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
        let mut irq = false;
        for i in 0..24 {
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
            l += s * vol(v.vol_l) / 0x8000;
            r += s * vol(v.vol_r) / 0x8000;
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
        if self.capture {
            let on = self.cnt & 0x8000 != 0 && self.cnt & 0x4000 != 0;
            let mix = |x: i32, m: u16| if on { (x * vol(m) / 0x8000).clamp(-0x8000, 0x7FFF) as i16 } else { 0 };
            self.out.push(mix(l, self.main_l));
            self.out.push(mix(r, self.main_r));
        }
        irq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_voice_plays_looped_square_wave() {
        let mut s = Spu::default();
        s.capture = true;
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
        s.tick(CYCLES_PER_SAMPLE * 200);
        let out = s.take_samples();
        assert_eq!(out.len(), 400);
        assert!(out.iter().any(|&x| x > 1000) && out.iter().any(|&x| x < -1000), "audible square wave");
        assert_eq!(s.read16(0x19C) & 1, 1, "ENDX set after loop end");
    }
}

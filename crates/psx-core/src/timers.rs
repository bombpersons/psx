//! Root counters (psx-spx "Timers"), three 16-bit counters at 0x1F801100+0x10*n.
//!
//! Modelled: free-running counting from the system clock, dot clock (T0),
//! hblank (T1) or system clock / 8 (T2); reset at target or 0xFFFF; IRQ on
//! target and/or overflow with one-shot/repeat and pulse/toggle modes; the
//! "reached" flags that clear on read; mode writes resetting the counter.
//! Not modelled yet: the sync modes (pausing/resetting at hblank/vblank).

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Timer {
    pub counter: u32,
    pub mode: u32,
    pub target: u32,
    /// Fractional accumulator for sources slower than the system clock (16.16).
    frac: u64,
    /// One-shot IRQ already fired since the last mode write.
    fired: bool,
}

/// Clock rates relative to the 33.8688 MHz system clock, as 16.16 fractions.
#[derive(Clone, Copy, Debug)]
pub struct Rates {
    pub dotclock: u64,
    pub hblank: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Timers {
    pub t: [Timer; 3],
}

impl Timers {
    pub fn read(&mut self, n: usize, reg: u32) -> u32 {
        let t = &mut self.t[n];
        match reg {
            0 => t.counter,
            4 => {
                let v = t.mode;
                t.mode &= !(3 << 11); // reached flags clear on read
                v
            }
            8 => t.target,
            _ => 0,
        }
    }

    pub fn write(&mut self, n: usize, reg: u32, value: u32) {
        let t = &mut self.t[n];
        match reg {
            0 => t.counter = value & 0xFFFF,
            4 => {
                t.mode = (value & 0x3FF) | (1 << 10);
                t.counter = 0;
                t.frac = 0;
                t.fired = false;
            }
            8 => t.target = value & 0xFFFF,
            _ => {}
        }
    }

    /// Advance by `cycles` system clocks. Returns I_STAT bits to raise (4,5,6).
    pub fn tick(&mut self, cycles: u64, rates: Rates) -> u32 {
        let mut irq = 0;
        for (n, t) in self.t.iter_mut().enumerate() {
            let src = (t.mode >> 8) & 3;
            let ticks = match (n, src) {
                (0, 1 | 3) => step_frac(&mut t.frac, cycles, rates.dotclock),
                (1, 1 | 3) => step_frac(&mut t.frac, cycles, rates.hblank),
                (2, 2 | 3) => step_frac(&mut t.frac, cycles, 1 << 13), // 1/8
                _ => cycles,
            };
            if ticks == 0 {
                continue;
            }
            let target = t.target;
            let before = t.counter as u64;
            let mut after = before + ticks;
            let mut hit_target = false;
            let mut hit_ffff = false;
            if target > 0 && before < target as u64 && after >= target as u64 {
                hit_target = true;
                if t.mode & (1 << 3) != 0 {
                    after = (after - target as u64) % (target as u64 + 1);
                }
            }
            if after > 0xFFFF {
                hit_ffff = true;
                after &= 0xFFFF;
            }
            t.counter = after as u32;
            if hit_target {
                t.mode |= 1 << 11;
            }
            if hit_ffff {
                t.mode |= 1 << 12;
            }
            let wants =
                (hit_target && t.mode & (1 << 4) != 0) || (hit_ffff && t.mode & (1 << 5) != 0);
            if wants && (t.mode & (1 << 6) != 0 || !t.fired) {
                t.fired = true;
                if t.mode & (1 << 7) != 0 {
                    t.mode ^= 1 << 10; // toggle mode
                } else {
                    t.mode &= !(1 << 10); // pulse: request asserted (active low)
                }
                if t.mode & (1 << 10) == 0 {
                    irq |= 1 << (4 + n);
                }
                if t.mode & (1 << 7) == 0 {
                    t.mode |= 1 << 10; // short pulse returns high
                }
            }
        }
        irq
    }
}

fn step_frac(frac: &mut u64, cycles: u64, rate: u64) -> u64 {
    *frac += cycles * rate;
    let whole = *frac >> 16;
    *frac &= 0xFFFF;
    whole
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: Rates = Rates {
        dotclock: 1 << 16,
        hblank: 1 << 16,
    };

    #[test]
    fn target_reset_and_irq() {
        let mut t = Timers::default();
        t.write(2, 8, 100);
        t.write(2, 4, (1 << 3) | (1 << 4) | (1 << 6)); // reset at target, IRQ at target, repeat
        assert_eq!(t.tick(99, R), 0);
        assert_eq!(t.tick(1, R), 1 << 6);
        assert_eq!(t.read(2, 0), 0);
        assert_ne!(t.read(2, 4) & (1 << 11), 0, "reached target");
        assert_eq!(t.read(2, 4) & (1 << 11), 0, "cleared on read");
        assert_eq!(t.tick(101, R), 1 << 6, "repeat mode fires again");
    }

    #[test]
    fn sysclock_div8_and_overflow() {
        let mut t = Timers::default();
        t.write(2, 4, (2 << 8) | (1 << 5));
        assert_eq!(t.tick(8 * 0xFFFF, R), 0);
        assert_eq!(t.read(2, 0), 0xFFFF);
        assert_eq!(t.tick(8, R), 1 << 6);
        assert_eq!(t.read(2, 0), 0);
    }
}

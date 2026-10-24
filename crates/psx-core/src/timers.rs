//! Root counters, three 16-bit counters at 0x1F801100+0x10*n, ported from
//! Mednafen's `timer.cpp` (`crates/psx-oracle/mednafen/src/psx/timer.cpp`).
//!
//! Counters clocked by the system clock catch up lazily: at their own event,
//! when software reads or writes them, and when the GPU ends a line phase.
//! Timer 0 can count dots and timer 1 horizontal retraces, which the video
//! timing hands over as it runs (so they run only as far as the GPU has).
//! Timer 1's sync modes follow vblank, timer 0's mode 3 resets at the end
//! of each horizontal retrace, and timer 2 stops in sync modes 1 and 3.
//! As in Mednafen, mode bit 10 (the interrupt line) is never set and an
//! interrupt is a pulse on the interrupt controller.

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Timer {
    pub mode: u32,
    /// 16-bit, but wider here to see counting past the target.
    pub counter: u32,
    pub target: u32,
    div8: u32,
    irq_done: bool,
    /// Counting: 1, not counting: 0, timer 1 mode 7 waiting for vblank: -1.
    counting: i32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Timers {
    pub t: [Timer; 3],
    vblank: bool,
    hretrace: bool,
    last: u64,
    /// Interrupt controller bits (4, 5, 6) pulsed since the bus last took them.
    irq: u32,
}

impl Default for Timers {
    /// Power on: the GPU then reports vblank.
    fn default() -> Self {
        Timers { t: [Timer::default(); 3], vblank: true, hretrace: false, last: 0, irq: 0 }
    }
}

impl Timers {
    /// Interrupts pulsed since the last call.
    pub fn take_irq(&mut self) -> u32 {
        std::mem::take(&mut self.irq)
    }

    /// Clocks until a counter clocked by the system clock could interrupt
    /// (Mednafen's `CalcNextEvent`), at most 1024.
    fn next_event(&self) -> u64 {
        let mut next = 1024u32;
        for (i, t) in self.t.iter().enumerate() {
            if t.mode & 0x30 == 0 {
                continue;
            }
            if t.mode & 8 != 0 && t.counter == 0 && t.target == 0 && !t.irq_done {
                next = 1;
                continue;
            }
            if (i < 2 && t.mode & 0x100 != 0) || t.counting <= 0 || (i == 2 && t.mode & 1 != 0) {
                continue;
            }
            let target = if t.mode & 0x18 != 0 && t.counter < t.target { t.target } else { 0x10000 };
            let delta = target.wrapping_sub(t.counter);
            let clocks = if i == 2 && t.mode & 0x200 != 0 { (delta * 8).wrapping_sub(t.div8) } else { delta };
            next = next.min(clocks);
        }
        next as u64
    }

    fn pulse(&mut self, i: usize) {
        self.t[i].irq_done = true;
        self.irq |= 1 << (4 + i);
    }

    fn matched(&mut self, i: usize) -> bool {
        let t = &mut self.t[i];
        t.mode |= 0x800;
        if t.mode & 8 != 0 {
            t.counter %= t.target.max(1);
        }
        if t.mode & 0x10 != 0 && !t.irq_done {
            let exact = t.counter == 0 || t.counter == t.target;
            self.pulse(i);
            return exact;
        }
        false
    }

    fn overflowed(&mut self, i: usize) -> bool {
        let t = &mut self.t[i];
        t.mode |= 0x1000;
        t.counter &= 0xFFFF;
        if t.mode & 0x20 != 0 && !t.irq_done {
            let exact = t.counter == 0;
            self.pulse(i);
            return exact;
        }
        false
    }

    fn clock(&mut self, i: usize, mut clocks: u32) {
        let t = &mut self.t[i];
        if t.counting <= 0 {
            clocks = 0;
        }
        if i == 2 {
            t.div8 += clocks;
            let d8 = t.div8 >> 3;
            t.div8 &= 7;
            if t.mode & 0x200 != 0 {
                clocks = d8;
            }
            if t.mode & 1 != 0 {
                clocks = 0;
            }
        }
        if t.mode & 8 != 0 && t.target == 0 && t.counter == 0 {
            self.matched(i);
        } else if clocks != 0 {
            let before = t.counter;
            t.counter += clocks;
            if t.mode & 0x40 != 0 {
                t.irq_done = false;
            }
            let (counter, target) = (t.counter, t.target);
            let mut exact = false;
            if (before < target && counter >= target) || counter >= target + 0x10000 {
                exact |= self.matched(i);
            }
            if self.t[i].counter >= 0x10000 {
                exact |= self.overflowed(i);
            }
            if self.t[i].mode & 0x40 != 0 && !exact {
                self.t[i].irq_done = false;
            }
        }
    }

    /// Clock the counters on the system clock up to `t` (Mednafen's
    /// `TIMER_Update`). Returns the timers' next event.
    pub fn update(&mut self, t: u64) -> u64 {
        let clocks = t.saturating_sub(self.last) as u32;
        for i in 0..3 {
            if self.t[i].mode & 0x100 == 0 {
                self.clock(i, clocks);
            }
        }
        self.last = self.last.max(t);
        t + self.next_event()
    }

    pub fn set_vblank(&mut self, status: bool) {
        let was = self.vblank;
        let t1 = &mut self.t[1];
        match t1.mode & 7 {
            1 => t1.counting = !status as i32,
            3 | 5 => {
                if t1.mode & 7 == 5 {
                    t1.counting = status as i32;
                }
                if was && !status {
                    t1.counter = 0;
                    if t1.target == 0 {
                        self.matched(1);
                    }
                }
            }
            7 => {
                if t1.counting == -1 && !was && status {
                    t1.counting = 0;
                } else if t1.counting == 0 && was && !status {
                    t1.counting = 1;
                }
            }
            _ => {}
        }
        self.vblank = status;
    }

    pub fn set_hretrace(&mut self, status: bool) {
        if self.hretrace && !status && self.t[0].mode & 7 == 3 {
            self.t[0].counter = 0;
            if self.t[0].target == 0 {
                self.matched(0);
            }
        }
        self.hretrace = status;
    }

    pub fn add_dot_clocks(&mut self, n: u32) {
        if self.t[0].mode & 0x100 != 0 {
            self.clock(0, n);
        }
    }

    pub fn clock_hretrace(&mut self) {
        if self.t[1].mode & 0x100 != 0 {
            self.clock(1, 1);
        }
    }

    fn counting_start(&mut self, i: usize) {
        let vblank = self.vblank;
        let t = &mut self.t[i];
        t.counting = 1;
        if i == 1 {
            match t.mode & 7 {
                1 => t.counting = !vblank as i32,
                5 => t.counting = vblank as i32,
                7 => t.counting = -1,
                _ => {}
            }
        }
    }

    /// A read of register `a` (0x1F801100-0x1F80113F) at `t`.
    pub fn read(&mut self, t: u64, a: u32) -> u32 {
        let i = ((a >> 4) & 3) as usize;
        if i >= 3 {
            return 0;
        }
        self.update(t);
        let tm = &mut self.t[i];
        let v = match a & 0xC {
            0 => tm.counter,
            4 => {
                let v = tm.mode;
                tm.mode &= !0x1000;
                if tm.counter != tm.target {
                    tm.mode &= !0x800;
                }
                v
            }
            8 => tm.target,
            _ => 0,
        };
        (v & 0xFFFF) >> ((a & 3) * 8)
    }

    /// A write of `value` to register `a` at `t`. Returns the timers' next event.
    pub fn write(&mut self, t: u64, a: u32, value: u32) -> u64 {
        self.update(t);
        let i = ((a >> 4) & 3) as usize;
        if i >= 3 {
            return t + self.next_event();
        }
        let v = ((value as u16 as u32) << ((a & 3) * 8)) & 0xFFFF;
        let tm = &mut self.t[i];
        match a & 0xC {
            0 => {
                tm.irq_done = false;
                tm.counter = v;
            }
            4 => {
                tm.mode = (v & 0x3FF) | (tm.mode & 0x1C00);
                tm.irq_done = false;
                tm.counter = 0;
                self.counting_start(i);
            }
            8 => tm.target = v,
            _ => {}
        }
        if self.t[i].counter == self.t[i].target {
            self.matched(i);
        }
        t + self.next_event()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_reset_and_irq() {
        let mut t = Timers::default();
        t.write(0, 0x28, 100);
        // Reset at target, IRQ at target, repeat.
        assert_eq!(t.write(0, 0x24, (1 << 3) | (1 << 4) | (1 << 6)), 100);
        t.update(99);
        assert_eq!(t.take_irq(), 0);
        t.update(100);
        assert_eq!(t.take_irq(), 1 << 6);
        assert_eq!(t.read(100, 0x20), 0);
        assert_ne!(t.read(100, 0x24) & (1 << 11), 0, "reached target");
        t.update(101);
        assert_eq!(t.read(101, 0x24) & (1 << 11), 0, "cleared on read");
        t.update(200);
        assert_eq!(t.take_irq(), 1 << 6, "repeat mode fires again");
    }

    #[test]
    fn sysclock_div8_and_overflow() {
        let mut t = Timers::default();
        t.write(0, 0x24, (2 << 8) | (1 << 5));
        t.update(8 * 0xFFFF);
        assert_eq!(t.read(8 * 0xFFFF, 0x20), 0xFFFF);
        assert_eq!(t.take_irq(), 0);
        t.update(8 * 0x10000);
        assert_eq!(t.take_irq(), 1 << 6);
        assert_eq!(t.read(8 * 0x10000, 0x20), 0);
    }

    #[test]
    fn hblank_timer_counts_only_when_clocked() {
        let mut t = Timers::default();
        t.write(0, 0x14, 0x100);
        t.update(5000);
        assert_eq!(t.read(5000, 0x10), 0);
        t.clock_hretrace();
        t.clock_hretrace();
        assert_eq!(t.read(5000, 0x10), 2);
    }
}

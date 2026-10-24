//! Controller / memory-card serial port, ported from Mednafen's `frontio.cpp`.
//!
//! JOY_DATA/STAT/MODE/CTRL/BAUD with one-byte transmit and receive buffers.
//! Bytes are shifted a bit at a time, each bit taking the baud reload value
//! (scaled by the mode's factor) in system clocks, through the controller in
//! port 1 (see [`crate::pad`]) and the memory cards (see [`crate::memcard`]);
//! port 2 has no controller. A device with more to send pulses DSR (/ACK),
//! which raises the interrupt (I_STAT bit 7) when JOY_CTRL bit 12 enables it.
//! The port catches up at its own event and at register accesses.

use crate::memcard::{CardPort, MemCard};
use crate::pad::Pad;

/// PS1 digital pad buttons (bit set = pressed; transmitted inverted).
pub mod button {
    pub const SELECT: u16 = 1 << 0;
    pub const L3: u16 = 1 << 1;
    pub const R3: u16 = 1 << 2;
    pub const START: u16 = 1 << 3;
    pub const UP: u16 = 1 << 4;
    pub const RIGHT: u16 = 1 << 5;
    pub const DOWN: u16 = 1 << 6;
    pub const LEFT: u16 = 1 << 7;
    pub const L2: u16 = 1 << 8;
    pub const R2: u16 = 1 << 9;
    pub const L1: u16 = 1 << 10;
    pub const R1: u16 = 1 << 11;
    pub const TRIANGLE: u16 = 1 << 12;
    pub const CIRCLE: u16 = 1 << 13;
    pub const CROSS: u16 = 1 << 14;
    pub const SQUARE: u16 = 1 << 15;
}

/// Button bit for a PCSX-Redux-style name (CROSS, START, UP, ...).
pub fn button_from_name(name: &str) -> Option<u16> {
    use button::*;
    Some(match name.to_ascii_uppercase().as_str() {
        "SELECT" => SELECT,
        "L3" => L3,
        "R3" => R3,
        "START" => START,
        "UP" => UP,
        "RIGHT" => RIGHT,
        "DOWN" => DOWN,
        "LEFT" => LEFT,
        "L2" => L2,
        "R2" => R2,
        "L1" => L1,
        "R1" => R1,
        "TRIANGLE" => TRIANGLE,
        "CIRCLE" => CIRCLE,
        "CROSS" => CROSS,
        "SQUARE" => SQUARE,
        _ => return None,
    })
}

/// One exchanged byte: (time, JOY_CTRL, tx, rx, DSR pending).
pub type ByteLog = (u64, u16, u8, u8, bool);

const SCALE_SHIFT: [u32; 4] = [0, 0, 4, 6];

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Sio0 {
    last: u64,
    /// The port's next event (Mednafen's `PSX_EVENT_FIO`).
    pub next: u64,
    clock_divider: i32,
    rx_pending: bool,
    tx_pending: bool,
    rx_busy: bool,
    tx_busy: bool,
    rx_avail: bool,
    rx_buf: u8,
    tx_buf: u8,
    rx_bits: u32,
    tx_bits: u32,
    mode: u16,
    ctrl: u16,
    baud: u16,
    /// The interrupt line (JOY_STAT bit 9), held until JOY_CTRL bit 4.
    istatus: bool,
    /// Per device (pad port 1, pad port 2, card 1, card 2): clocks until DSR
    /// pulses, and when the pulse ends.
    dsr_delay: [i32; 4],
    dsr_until: [Option<u64>; 4],
    /// The interrupt line rose since the bus last looked.
    irq: bool,
    /// Pressed buttons on the port-1 pad.
    pub buttons: u16,
    /// Bytes written to JOY_DATA.
    pub transfers: u64,
    /// Memory cards in slots 1 and 2.
    pub cards: [Option<MemCard>; 2],
    card_ports: [CardPort; 2],
    /// The controller in port 1 (its buttons come from `buttons`).
    pub pad: Pad,
    /// When `Some`, every byte exchanged.
    #[serde(skip)]
    pub log: Option<Vec<ByteLog>>,
}

impl Sio0 {
    /// Whether the interrupt line rose since the last call.
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq)
    }

    fn assert_irq(&mut self) {
        if !self.istatus {
            self.irq = true;
        }
        self.istatus = true;
    }

    fn reload(&self) -> i32 {
        ((((self.baud as u32) << SCALE_SHIFT[(self.mode & 3) as usize]) & !1).max(0x20)) as i32
    }

    fn next_event(&self, t: u64, mut next: i32) -> u64 {
        if self.clock_divider > 0 && self.clock_divider < next {
            next = self.clock_divider;
        }
        for &d in &self.dsr_delay {
            if d > 0 && next > d {
                next = d;
            }
        }
        t + next as u64
    }

    /// Mednafen's `CheckStartStopPending`: start a pending transfer, or stop.
    fn start_stop(&mut self, t: u64, set_event: bool) {
        if (self.rx_pending && self.ctrl & 4 != 0) || (self.tx_pending && self.ctrl & 1 != 0) {
            if self.rx_pending {
                self.rx_pending = false;
                self.rx_busy = true;
                self.rx_avail = false;
                self.rx_buf = 0;
                self.rx_bits = 0;
            }
            if self.tx_pending {
                self.tx_pending = false;
                self.tx_busy = true;
                self.tx_bits = 0;
            }
            self.clock_divider = self.reload();
        }
        if self.ctrl & 5 == 0 {
            self.rx_busy = false;
            self.tx_busy = false;
        }
        if !self.rx_busy && !self.tx_busy {
            self.clock_divider = 0;
        }
        if set_event {
            self.next = self.next_event(t, 0x1000_0000);
        }
    }

    /// Bring the port to `t` (Mednafen's `FrontIO::Update`). Returns its next event.
    pub fn update(&mut self, t: u64) -> u64 {
        let clocks = t.saturating_sub(self.last) as i32;
        // Mednafen hands devices new input as each frame starts.
        self.pad.buttons = self.buttons;
        for i in 0..4 {
            if self.dsr_delay[i] > 0 {
                self.dsr_delay[i] -= clocks;
                if self.dsr_delay[i] <= 0 {
                    self.dsr_until[i] = Some((t as i64 + 32 + self.dsr_delay[i] as i64) as u64);
                    if self.ctrl & 0x1000 != 0 {
                        self.assert_irq();
                    }
                }
            }
        }
        let mut check = false;
        if self.clock_divider > 0 {
            self.clock_divider -= clocks;
            while self.clock_divider <= 0 && (self.rx_busy || self.tx_busy) {
                let mut txd = false;
                if self.tx_busy {
                    txd = (self.tx_buf >> self.tx_bits) & 1 != 0;
                    self.tx_bits = (self.tx_bits + 1) & 7;
                    if self.tx_bits == 0 {
                        check = true;
                        self.tx_busy = false;
                        if self.ctrl & 0x400 != 0 {
                            self.assert_irq();
                        }
                    }
                }
                let (rxd, d) = self.pad.clock(txd);
                self.dsr_delay[0] = d;
                self.dsr_delay[1] = 0;
                let mut rxd = rxd;
                for slot in 0..2 {
                    let (b, d) = match &mut self.cards[slot] {
                        Some(card) => self.card_ports[slot].clock(card, txd),
                        None => (true, 0),
                    };
                    rxd &= b;
                    self.dsr_delay[2 + slot] = d;
                }
                if self.rx_busy {
                    self.rx_buf = (self.rx_buf & !(1 << self.rx_bits)) | (rxd as u8) << self.rx_bits;
                    self.rx_bits = (self.rx_bits + 1) & 7;
                    if self.rx_bits == 0 {
                        check = true;
                        self.rx_busy = false;
                        self.rx_avail = true;
                        if let Some(log) = &mut self.log {
                            log.push((t, self.ctrl, self.tx_buf, self.rx_buf, self.dsr_delay.iter().any(|&d| d > 0)));
                        }
                        if self.ctrl & 0x800 != 0 {
                            self.assert_irq();
                        }
                    }
                }
                self.clock_divider += self.reload();
            }
        }
        self.last = t;
        if check {
            self.start_stop(t, false);
        }
        self.next_event(t, 0x1000_0000)
    }

    fn set_dtr(&mut self) {
        let sel = self.ctrl & 2 != 0;
        let port2 = self.ctrl & 0x2000 != 0;
        self.pad.set_dtr(sel && !port2);
        self.card_ports[0].set_dtr(sel && !port2);
        self.card_ports[1].set_dtr(sel && port2);
        for (i, on) in [(0, sel && !port2), (1, sel && port2)] {
            if !on {
                self.dsr_delay[i] = 0;
                self.dsr_delay[i + 2] = 0;
                self.dsr_until[i] = None;
                self.dsr_until[i + 2] = None;
            }
        }
    }

    /// A read of register `a` (0x1F801040-0x1F80104F) at `t`.
    pub fn read(&mut self, t: u64, a: u32) -> u32 {
        self.update(t);
        let v = match a & 0xE {
            0x0 | 0x2 => {
                let b = self.rx_buf as u32;
                let v = b * 0x0101_0101;
                self.rx_avail = false;
                self.rx_pending = true;
                self.rx_busy = false;
                self.start_stop(t, true);
                v >> ((a & 2) * 8)
            }
            0x4 => {
                let mut v = 0;
                if !self.tx_pending && !self.tx_busy {
                    v |= 1;
                }
                if self.rx_avail {
                    v |= 2;
                }
                if self.dsr_until.iter().any(|u| u.is_some_and(|u| t < u)) {
                    v |= 0x80;
                }
                if self.istatus {
                    v |= 0x200;
                }
                v
            }
            0x8 => self.mode as u32,
            0xA => self.ctrl as u32,
            0xE => self.baud as u32,
            _ => 0,
        };
        v >> ((a & 1) * 8)
    }

    /// A write of `v` to register `a` at `t`.
    pub fn write(&mut self, t: u64, a: u32, v: u32) {
        let v = v << ((a & 1) * 8);
        self.update(t);
        match a & 0xE {
            0x0 | 0x2 => {
                self.tx_buf = (v << ((a & 2) * 8)) as u8;
                self.tx_pending = true;
                self.tx_busy = false;
                self.transfers += 1;
            }
            0x8 => self.mode = (v & 0x13F) as u16,
            0xA => {
                self.ctrl = (v & 0x3F2F) as u16;
                if v & 0x10 != 0 {
                    self.istatus = false;
                }
                if v & 0x40 != 0 {
                    self.istatus = false;
                    self.clock_divider = 0;
                    self.rx_pending = false;
                    self.tx_pending = false;
                    self.rx_busy = false;
                    self.tx_busy = false;
                    self.rx_avail = false;
                    self.tx_buf = 0;
                    self.rx_buf = 0;
                    self.rx_bits = 0;
                    self.tx_bits = 0;
                    self.mode = 0;
                    self.ctrl = 0;
                    self.baud = 0;
                }
                self.set_dtr();
            }
            0xE => self.baud = v as u16,
            _ => {}
        }
        self.start_stop(t, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write JOY_DATA and wait for the byte to come back.
    fn exchange(s: &mut Sio0, t: &mut u64, tx: u8) -> (u8, bool) {
        s.write(*t, 0, tx as u32);
        while s.read(*t, 4) & 2 == 0 {
            *t = s.next;
            s.next = s.update(*t);
        }
        // DSR comes 0x40 clocks after the byte, if at all.
        *t += 0x60;
        s.next = s.update(*t);
        let irq = s.take_irq();
        s.write(*t, 0xA, 0x1013); // acknowledge
        (s.read(*t, 0) as u8, irq)
    }

    #[test]
    fn digital_pad_poll() {
        let mut s = Sio0 { buttons: button::CROSS | button::START, ..Default::default() };
        let mut t = 0;
        s.write(t, 0xE, 0x88);
        s.write(t, 0x8, 0x0D);
        s.write(t, 0xA, 0x1003); // TX enable, select, DSR IRQ enable
        s.read(t, 0); // a JOY_DATA read makes the receiver wait for the next byte
        let got: Vec<(u8, bool)> = [0x01u8, 0x42, 0, 0, 0].into_iter().map(|b| exchange(&mut s, &mut t, b)).collect();
        assert_eq!(got, vec![(0xFF, true), (0x41, true), (0x5A, true), (!0x08u8, true), (!0x40u8, false)]);
    }
}

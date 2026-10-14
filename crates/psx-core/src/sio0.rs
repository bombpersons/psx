//! Controller / memory-card serial port (psx-spx "Controllers and Memory Cards").
//!
//! Modelled: JOY_DATA/STAT/MODE/CTRL/BAUD, one-byte transfers with an RX
//! FIFO, /ACK interrupts (I_STAT bit 7) after a delay, a digital pad in
//! port 1 (ID 0x5A41) with configurable buttons, and memory cards in either
//! slot (see [`crate::memcard`]). Empty slots read 0xFF with no /ACK.

use std::collections::VecDeque;

use crate::memcard::{CardSession, MemCard};

/// Delay from a byte transfer to the device's /ACK interrupt (~1 byte at 250 kHz + ack).
const ACK_DELAY: u64 = 1_500;

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

/// The device answering the current transfer sequence.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
enum Target {
    #[default]
    None,
    Pad,
    /// Card in slot 0/1, before (None) and after its command byte.
    Card(usize, Option<CardSession>),
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Sio0 {
    rx: VecDeque<u8>,
    mode: u16,
    ctrl: u16,
    baud: u16,
    irq_flag: bool,
    ack_at: Option<u64>,
    /// Position in the current pad command (0 = expecting address).
    seq: u8,
    target: Target,
    /// Pressed buttons on the port-1 pad.
    pub buttons: u16,
    pub transfers: u64,
    /// Memory cards in slots 1 and 2.
    pub cards: [Option<MemCard>; 2],
    /// When `Some`, every byte exchanged: (time, JOY_CTRL, tx, rx, ack).
    #[serde(skip)]
    pub log: Option<Vec<(u64, u16, u8, u8, bool)>>,
}

impl Sio0 {
    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x0 => self.rx.pop_front().map_or(0xFF, |b| b as u32),
            0x4 => {
                let mut s = 0b101; // TX ready (bit 0) and TX finished (bit 2)
                if !self.rx.is_empty() {
                    s |= 1 << 1;
                }
                if self.irq_flag {
                    s |= 1 << 9;
                }
                s
            }
            0x8 => self.mode as u32,
            0xA => self.ctrl as u32,
            0xE => self.baud as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64) {
        match off {
            0x0 => self.transfer(v as u8, now),
            0x8 => self.mode = v as u16,
            0xA => {
                if v & (1 << 4) != 0 {
                    self.irq_flag = false; // acknowledge
                }
                if v & (1 << 6) != 0 {
                    let (cards, log) = (std::mem::take(&mut self.cards), self.log.take());
                    *self = Sio0 { buttons: self.buttons, transfers: self.transfers, cards, log, ..Default::default() };
                    return;
                }
                if v & 2 == 0 {
                    self.seq = 0; // /JOYn deselected: device resets
                    self.target = Target::None;
                }
                self.ctrl = v as u16 & !(1 << 4);
            }
            0xE => self.baud = v as u16,
            _ => {}
        }
    }

    fn transfer(&mut self, tx: u8, now: u64) {
        self.transfers += 1;
        let port2 = self.ctrl & (1 << 13) != 0;
        let selected = self.ctrl & 2 != 0;
        let slot = port2 as usize;
        let (rx, ack) = if !selected {
            (0xFF, false)
        } else if self.seq == 0 {
            // The address byte picks the device.
            match tx {
                0x01 if !port2 => {
                    self.target = Target::Pad;
                    (0xFF, true)
                }
                0x81 if self.cards[slot].is_some() => {
                    self.target = Target::Card(slot, None);
                    (0xFF, true)
                }
                _ => (0xFF, false),
            }
        } else {
            match &mut self.target {
                Target::Pad => {
                    let pressed = !self.buttons;
                    match (self.seq, tx) {
                        (1, 0x42) => (0x41, true),
                        (1, _) => (0xFF, false),
                        (2, _) => (0x5A, true),
                        (3, _) => (pressed as u8, true),
                        (4, _) => ((pressed >> 8) as u8, false),
                        _ => (0xFF, false),
                    }
                }
                Target::Card(slot, session) => match (self.cards[*slot].as_mut(), session) {
                    (Some(card), Some(s)) => s.exchange(card, tx),
                    (Some(card), session @ None) => {
                        let (s, flag, ok) = CardSession::start(card, tx);
                        *session = Some(s);
                        (flag, ok)
                    }
                    (None, _) => (0xFF, false),
                },
                Target::None => (0xFF, false),
            }
        };
        if let Some(log) = &mut self.log {
            log.push((now, self.ctrl, tx, rx, ack));
        }
        self.seq = if ack { self.seq.saturating_add(1) } else { 0 };
        if !ack {
            self.target = Target::None;
        }
        if self.rx.len() < 8 {
            self.rx.push_back(rx);
        }
        if ack && self.ctrl & (1 << 12) != 0 {
            self.ack_at = Some(now + ACK_DELAY);
        }
    }

    /// Returns true when an /ACK interrupt fires at `now`.
    pub fn tick(&mut self, now: u64) -> bool {
        match self.ack_at {
            Some(t) if now >= t => {
                self.ack_at = None;
                self.irq_flag = true;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digital_pad_poll() {
        let mut s = Sio0 { buttons: button::CROSS | button::START, ..Default::default() };
        s.write(0xA, 0x1003, 0); // TX enable, select, ACK IRQ enable
        let mut got = Vec::new();
        for (i, tx) in [0x01u8, 0x42, 0, 0, 0].into_iter().enumerate() {
            s.write(0x0, tx as u32, i as u64 * 10_000);
            got.push(s.read(0x0) as u8);
            let acked = s.tick(i as u64 * 10_000 + ACK_DELAY);
            assert_eq!(acked, i < 4, "byte {i}");
            s.write(0xA, 0x1013, 0);
        }
        assert_eq!(got, vec![0xFF, 0x41, 0x5A, !0x08u8, !0x40u8]);
    }
}

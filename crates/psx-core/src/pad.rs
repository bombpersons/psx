//! Controllers on SIO0, ported from Mednafen's `input/gamepad.cpp` and
//! `input/dualshock.cpp`: a digital pad (ID 5A41h), or a DualShock that
//! starts digital and can be switched to analog (ID 5A73h) through config
//! mode (ID 5AF3h). Rumble commands are accepted and the motor values
//! recorded; nothing else is done with them.
//!
//! Devices are clocked a bit at a time by the serial port. A reply byte is
//! fixed at the end of the byte before it, and a device that has more to send
//! pulses /ACK (DSR) a while after each byte.

/// Which controller is plugged into port 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PadKind {
    #[default]
    Digital,
    DualShock,
}

impl PadKind {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "digital" => Some(PadKind::Digital),
            "dualshock" | "analog" => Some(PadKind::DualShock),
            _ => None,
        }
    }
}

/// A device's end of the serial link: DTR (select), the bit position, the
/// byte being received and the bytes queued to send.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Link {
    dtr: bool,
    pub phase: i32,
    bitpos: u32,
    pub rx: u8,
    pub tx: [u8; 8],
    pub pos: usize,
    pub count: usize,
}

impl Link {
    /// Returns whether DTR rose (the device then restarts its command).
    pub fn set_dtr(&mut self, dtr: bool) -> bool {
        let rose = !self.dtr && dtr;
        if rose {
            self.phase = 0;
            self.bitpos = 0;
            self.pos = 0;
            self.count = 0;
        }
        self.dtr = dtr;
        rose
    }

    pub fn dtr(&self) -> bool {
        self.dtr
    }

    /// Shift one bit each way. Returns the bit sent and whether a byte was
    /// completed (`rx` then holds it, and the byte sent is consumed).
    pub fn shift(&mut self, txd: bool) -> (bool, bool) {
        let out = self.count == 0 || (self.tx[self.pos] >> self.bitpos) & 1 != 0;
        self.rx = (self.rx & !(1 << self.bitpos)) | (txd as u8) << self.bitpos;
        self.bitpos = (self.bitpos + 1) & 7;
        let done = self.bitpos == 0;
        if done && self.count > 0 {
            self.pos += 1;
            self.count -= 1;
        }
        (out, done)
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.tx[..bytes.len()].copy_from_slice(bytes);
        self.pos = 0;
        self.count = bytes.len();
    }

    /// The DSR pulse delay after this bit: `delay` once a byte is done and
    /// more are queued.
    pub fn dsr(&self, delay: i32) -> i32 {
        if self.bitpos == 0 && self.count > 0 { delay } else { 0 }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Pad {
    pub kind: PadKind,
    /// Pressed buttons (bit set = pressed; see [`crate::sio0::button`]).
    pub buttons: u16,
    /// Sticks as `[right x, right y, left x, left y]`, 0x80 = centred.
    pub sticks: [u8; 4],
    /// DualShock state: analog mode, config mode (Mednafen's
    /// `mad_munchkins`) and the analog mode lock.
    pub analog: bool,
    pub config: bool,
    pub locked: bool,
    /// Last motor values sent (small, large).
    pub motors: (u8, u8),
    da_rumble_compat: bool,
    rumble_magic: [u8; 6],
    command: u8,
    link: Link,
}

impl Default for Pad {
    fn default() -> Self {
        Pad {
            kind: PadKind::Digital,
            buttons: 0,
            sticks: [0x80; 4],
            analog: false,
            config: false,
            locked: false,
            motors: (0, 0),
            da_rumble_compat: true,
            rumble_magic: [0xFF; 6],
            command: 0,
            link: Link::default(),
        }
    }
}

impl Pad {
    pub(crate) fn set_dtr(&mut self, dtr: bool) {
        self.link.set_dtr(dtr);
    }

    /// One bit each way (Mednafen's `Clock`). Returns the bit sent and the
    /// DSR pulse delay.
    pub(crate) fn clock(&mut self, txd: bool) -> (bool, i32) {
        if !self.link.dtr() {
            return (true, 0);
        }
        let (out, done) = self.link.shift(txd);
        if done {
            match self.kind {
                PadKind::Digital => self.digital_byte(),
                PadKind::DualShock => self.dualshock_byte(),
            }
        }
        (out, self.link.dsr(0x40))
    }

    fn pressed(&self) -> [u8; 2] {
        [!self.buttons as u8, !(self.buttons >> 8) as u8]
    }

    fn digital_byte(&mut self) {
        let rx = self.link.rx;
        match self.link.phase {
            0 if rx != 0x01 => self.link.phase = -1,
            0 => {
                self.link.send(&[0x41]);
                self.link.phase = 1;
            }
            1 => {
                self.command = rx;
                self.link.phase = 2;
                if rx == 0x42 {
                    let [b0, b1] = self.pressed();
                    self.link.send(&[0x5A, b0, b1]);
                } else {
                    self.link.phase = -1;
                    self.link.tx[..3].copy_from_slice(&[0x5A, 0, 0]);
                    self.link.pos = 0;
                    self.link.count = 0;
                }
            }
            _ => {}
        }
    }

    fn dualshock_byte(&mut self) {
        let rx = self.link.rx;
        let [b0, b1] = self.pressed();
        let s = self.sticks;
        let phase = self.link.phase;
        let l = &mut self.link;
        match phase {
            0 if rx != 0x01 => l.phase = -1,
            0 if self.config => {
                l.send(&[0xF3]);
                l.phase = 101;
            }
            0 => {
                l.send(&[if self.analog { 0x73 } else { 0x41 }]);
                l.phase = 1;
            }
            1 => {
                self.command = rx;
                l.phase = 2;
                l.tx[0] = 0x5A;
                if rx == 0x42 {
                    l.send(&[0x5A]);
                    l.phase = 0x4200;
                } else if rx == 0x43 {
                    if self.analog {
                        l.send(&[0x5A, b0, b1, s[0], s[1], s[2], s[3]]);
                    } else {
                        l.send(&[0x5A, b0, b1]);
                    }
                } else {
                    l.phase = -1;
                    l.tx[1] = 0;
                    l.tx[2] = 0;
                    l.pos = 0;
                    l.count = 0;
                }
            }
            2 => {
                if self.command == 0x43 && l.pos == 2 && rx == 0x01 {
                    self.config = true;
                    if self.da_rumble_compat {
                        self.motors = (0, 0);
                        self.da_rumble_compat = false;
                    }
                    l.phase = -1;
                }
            }
            101 => {
                self.command = rx;
                if (0x40..=0x4F).contains(&rx) {
                    l.send(&[0x5A]);
                    l.phase = (rx as i32) << 8;
                } else {
                    l.count = 0;
                    l.phase = -1;
                }
            }
            0x4200 => {
                if self.analog || self.config {
                    l.send(&[b0, b1, s[0], s[1], s[2], s[3]]);
                } else if self.rumble_magic[2] & 0xFE == 0 {
                    l.send(&[b0, b1, 0, 0]);
                } else {
                    l.send(&[b0, b1]);
                }
                l.phase += 1;
            }
            0x4201 => {
                let m = self.rumble_magic;
                if self.da_rumble_compat || (m[0] == 0 && m[2] != 0 && m[3] != 0 && m[4] != 0 && m[5] != 0) {
                    self.motors.0 = rx;
                }
                l.phase += 1;
            }
            0x4202 => {
                let m = self.rumble_magic;
                if self.da_rumble_compat || m[1] == 1 {
                    self.motors.1 = rx;
                } else if m[1] == 0 && m[2] != 0 && m[3] != 0 && m[4] != 0 && m[5] != 0 {
                    self.motors.0 = rx;
                }
                l.phase += 1;
            }
            0x4203 => {
                if !self.da_rumble_compat && self.rumble_magic[1] == 0 && self.rumble_magic[2] == 1 {
                    self.motors.1 = rx;
                }
                l.phase += 1;
            }
            // Config mode commands: the byte after 5Ah must be 00h.
            0x4000 | 0x4100 | 0x4300 | 0x4400 | 0x4500 | 0x4600 | 0x4700 | 0x4800 | 0x4900 | 0x4A00 | 0x4B00
            | 0x4C00 | 0x4D00 | 0x4E00 | 0x4F00 => {
                if rx == 0 {
                    l.send(&[match phase {
                        0x4500 => 0x01,
                        0x4D00 => self.rumble_magic[0],
                        _ => 0,
                    }]);
                    l.phase += 1;
                } else {
                    l.phase = -1;
                }
            }
            0x4301 => {
                if rx == 0 {
                    self.config = false;
                }
                l.send(&[0; 5]);
                l.phase = -1;
            }
            0x4401 => {
                l.send(&[0; 5]);
                l.phase += 1;
                match rx {
                    0 => self.analog = false,
                    1 => self.analog = true,
                    _ => {}
                }
            }
            0x4402 => {
                match rx {
                    2 => self.locked = false,
                    3 => self.locked = true,
                    _ => {}
                }
                l.phase = -1;
            }
            0x4501 => {
                l.send(&[0x02, self.analog as u8, 0x02, 0x01, 0x00]);
                l.phase = -1;
            }
            0x4601 => {
                l.send(&match rx {
                    0 => [0, 1, 2, 0, 0x0A],
                    1 => [0, 1, 1, 1, 0x14],
                    _ => [0; 5],
                });
                l.phase = -1;
            }
            0x4701 => {
                l.send(&if rx == 0 { [0, 2, 0, 1, 0] } else { [0; 5] });
                l.phase = -1;
            }
            0x4801 => {
                l.send(&match rx {
                    0 => [0, 0, 0, 1, self.motors.0],
                    1 => [0, 0, 0, 1, self.motors.1],
                    _ => [0; 5],
                });
                l.phase = -1;
            }
            0x4C01 => {
                l.send(&match rx {
                    0 => [0, 0, 4, 0, 0],
                    1 => [0, 0, 7, 0, 0],
                    _ => [0; 5],
                });
                l.phase = -1;
            }
            0x4D01..=0x4D06 => {
                let i = (phase - 0x4D01) as usize;
                if i < 5 {
                    l.send(&[self.rumble_magic[1 + i]]);
                    l.phase += 1;
                } else {
                    l.phase = -1;
                }
                self.rumble_magic[i] = rx;
            }
            0x4001 | 0x4101 | 0x4901 | 0x4A01 | 0x4B01 | 0x4E01 | 0x4F01 => {
                l.send(&[0; 5]);
                l.phase = -1;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One byte each way, a bit at a time; returns the byte received and
    /// whether DSR followed it.
    pub(crate) fn byte(clock: &mut dyn FnMut(bool) -> (bool, i32), tx: u8) -> (u8, bool) {
        let (mut rx, mut dsr) = (0, 0);
        for bit in 0..8 {
            let (b, d) = clock((tx >> bit) & 1 != 0);
            rx |= (b as u8) << bit;
            dsr = d;
        }
        (rx, dsr > 0)
    }

    fn xfer(p: &mut Pad, bytes: &[u8]) -> Vec<u8> {
        p.set_dtr(true);
        let mut rx = Vec::new();
        for (k, &b) in [0x01].iter().chain(bytes).enumerate() {
            let (r, ack) = byte(&mut |t| p.clock(t), b);
            rx.push(r);
            if !ack {
                assert_eq!(k, bytes.len(), "transfer ended early at byte {k}");
            }
        }
        p.set_dtr(false);
        rx
    }

    #[test]
    fn digital_poll() {
        let mut p = Pad { buttons: crate::sio0::button::CROSS, ..Default::default() };
        assert_eq!(xfer(&mut p, &[0x42, 0, 0, 0]), vec![0xFF, 0x41, 0x5A, 0xFF, !0x40u8]);
    }

    #[test]
    fn dualshock_switches_to_analog_and_locks() {
        let mut p = Pad { kind: PadKind::DualShock, sticks: [0x10, 0x20, 0x30, 0x40], ..Default::default() };
        assert_eq!(xfer(&mut p, &[0x42, 0, 0, 0])[1], 0x41, "starts digital");
        xfer(&mut p, &[0x43, 0, 1]); // enter config
        assert!(p.config);
        assert_eq!(xfer(&mut p, &[0x45, 0, 0, 0, 0, 0, 0, 0]), vec![0xFF, 0xF3, 0x5A, 1, 2, 0, 2, 1, 0]);
        xfer(&mut p, &[0x44, 0, 1, 3, 0, 0, 0, 0]); // analog, locked
        assert_eq!(xfer(&mut p, &[0x43, 0, 0, 0, 0, 0, 0, 0])[3..], [0; 6]);
        assert!(p.analog && p.locked && !p.config);
        assert_eq!(
            xfer(&mut p, &[0x42, 0, 0, 0, 0, 0, 0, 0]),
            vec![0xFF, 0x73, 0x5A, 0xFF, 0xFF, 0x10, 0x20, 0x30, 0x40]
        );
    }

    #[test]
    fn digital_pad_rejects_config() {
        let mut p = Pad::default();
        p.set_dtr(true);
        assert_eq!(byte(&mut |t| p.clock(t), 0x01), (0xFF, true));
        assert_eq!(byte(&mut |t| p.clock(t), 0x43), (0x41, false));
    }
}

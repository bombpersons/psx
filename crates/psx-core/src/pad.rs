//! Controllers on SIO0 (psx-spx "Controllers and Memory Cards" and
//! "Controllers - Configuration Commands"): a digital pad (ID 5A41h), or a
//! DualShock that starts digital and can be switched to analog (ID 5A73h)
//! through config mode (ID 5AF3h). Rumble commands are accepted and the
//! motor values recorded; nothing else is done with them.

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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Pad {
    pub kind: PadKind,
    /// Pressed buttons (bit set = pressed; see [`crate::sio0::button`]).
    pub buttons: u16,
    /// Sticks as `[right x, right y, left x, left y]`, 0x80 = centred.
    pub sticks: [u8; 4],
    /// DualShock state.
    pub analog: bool,
    pub config: bool,
    pub locked: bool,
    rumble_map: [u8; 6],
    /// Last motor values sent with a poll (small, large).
    pub motors: (u8, u8),
    /// Current command and its parameter bytes.
    command: u8,
    params: [u8; 8],
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
            rumble_map: [0xFF; 6],
            motors: (0, 0),
            command: 0,
            params: [0; 8],
        }
    }
}

impl Pad {
    fn id(&self) -> u8 {
        if self.config {
            0xF3
        } else if self.analog {
            0x73
        } else {
            0x41
        }
    }

    /// Data bytes after the 5Ah of the current command, given the parameter bytes so far.
    fn reply(&self, i: usize) -> u8 {
        let pressed = !self.buttons;
        let poll = |i: usize| -> u8 {
            match i {
                0 => pressed as u8,
                1 => (pressed >> 8) as u8,
                n => self.sticks.get(n - 2).copied().unwrap_or(0),
            }
        };
        if !self.config {
            return poll(i);
        }
        // Config-mode replies per psx-spx; the variable ones depend on the
        // first parameter byte, which arrives with reply byte 0.
        let table: [u8; 6] = match (self.command, self.params[0]) {
            // Forced analog response, even in digital mode.
            (0x42, _) => return poll(i),
            // Type 01h = PSX DualShock/analog pad (03h would be a DualShock 2).
            (0x45, _) => [0x01, 0x02, self.analog as u8, 0x02, 0x01, 0x00],
            (0x46, 0) => [0, 0, 1, 2, 0, 0x0A],
            (0x46, 1) => [0, 0, 1, 1, 1, 0x14],
            (0x47, _) => [0, 0, 2, 0, 1, 0],
            (0x48, 0 | 1) => [0, 0, 0, 0, 1, 0],
            (0x4C, 0) => [0, 0, 0, 4, 0, 0],
            (0x4C, 1) => [0, 0, 0, 7, 0, 0],
            (0x4D, _) => self.rumble_map,
            // 43h in config mode, and everything else: zeros.
            _ => [0; 6],
        };
        table.get(i).copied().unwrap_or(0)
    }

    /// Data bytes in the current reply (after 5Ah).
    fn reply_len(&self) -> usize {
        if self.config || self.analog { 6 } else { 2 }
    }

    /// Byte `seq` (1 = command byte) of a transfer that began with 01h.
    /// Returns `(reply, ack)`; no ack ends the transfer.
    pub fn exchange(&mut self, seq: u8, tx: u8) -> (u8, bool) {
        match seq {
            1 => {
                let ok = match self.kind {
                    PadKind::Digital => tx == 0x42,
                    PadKind::DualShock => tx == 0x42 || tx == 0x43 || (self.config && (0x40..=0x4F).contains(&tx)),
                };
                if !ok {
                    return (0xFF, false);
                }
                self.command = tx;
                self.params = [0; 8];
                (self.id(), true)
            }
            2 => (0x5A, true),
            n => {
                let i = n as usize - 3;
                if i < self.params.len() {
                    self.params[i] = tx;
                }
                let r = self.reply(i);
                let last = i + 1 >= self.reply_len();
                if last {
                    self.finish();
                }
                (r, !last)
            }
        }
    }

    /// Apply a completed command's parameters.
    fn finish(&mut self) {
        let p = self.params;
        match (self.command, self.config) {
            (0x42, _) if self.kind == PadKind::DualShock => self.motors = (p[0], p[1]),
            (0x43, _) => self.config = p[0] == 1,
            (0x44, true) => {
                self.analog = p[0] == 1;
                self.locked = p[1] == 3;
            }
            (0x4D, true) => self.rumble_map.copy_from_slice(&p[..6]),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xfer(p: &mut Pad, bytes: &[u8]) -> Vec<u8> {
        let mut rx = vec![0xFF]; // address byte 01h
        for (k, &b) in bytes.iter().enumerate() {
            let (r, ack) = p.exchange(k as u8 + 1, b);
            rx.push(r);
            if !ack {
                assert_eq!(k + 1, bytes.len(), "transfer ended early at byte {k}");
            }
        }
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
        xfer(&mut p, &[0x43, 0, 1, 0]); // enter config (digital-length reply)
        assert!(p.config);
        assert_eq!(xfer(&mut p, &[0x45, 0, 0, 0, 0, 0, 0, 0]), vec![0xFF, 0xF3, 0x5A, 1, 2, 0, 2, 1, 0]);
        xfer(&mut p, &[0x44, 0, 1, 3, 0, 0, 0, 0]); // analog, locked
        // Exit config: zeros in config mode, not button data.
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
        assert_eq!(p.exchange(1, 0x43), (0xFF, false));
    }
}

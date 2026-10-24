//! PlayStation memory card (psx-spx "Memory Card Data Format"): 128 KiB as
//! 1024 sectors ("frames") of 128 bytes. Its protocol is Mednafen's
//! (`input/memcard.cpp`): Read (52h) and Write (57h); Get ID (53h) isn't
//! answered.

pub const SIZE: usize = 128 * 1024;
pub const SECTOR: usize = 128;

/// FLAG bit 3: set at power-on/insertion ("directory not read yet"),
/// cleared by the first successful write.
const FLAG_NEW: u8 = 0x08;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MemCard {
    pub data: Vec<u8>,
    pub flag: u8,
    /// Sectors written since insertion.
    pub writes: u64,
}

fn checksum(frame: &[u8]) -> u8 {
    frame[..127].iter().fold(0, |a, b| a ^ b)
}

impl MemCard {
    /// A card image (`.mcd`/`.mcr` raw 128 KiB); shorter images are padded.
    pub fn from_image(mut data: Vec<u8>) -> Self {
        data.resize(SIZE, 0);
        MemCard { data, flag: FLAG_NEW, writes: 0 }
    }

    /// A freshly formatted, empty card.
    pub fn formatted() -> Self {
        let mut data = vec![0u8; SIZE];
        let mut frame = |n: usize, fill: &dyn Fn(&mut [u8])| {
            let f = &mut data[n * SECTOR..(n + 1) * SECTOR];
            fill(f);
            f[127] = checksum(f);
        };
        let header = |f: &mut [u8]| f[..2].copy_from_slice(b"MC");
        frame(0, &header);
        for n in 1..16 {
            // Free directory entry: state A0h, no next block.
            frame(n, &|f: &mut [u8]| {
                f[0] = 0xA0;
                f[8..10].copy_from_slice(&[0xFF, 0xFF]);
            });
        }
        for n in 16..36 {
            // Unused broken-sector list entry.
            frame(n, &|f: &mut [u8]| {
                f[..4].copy_from_slice(&[0xFF; 4]);
                f[8..10].copy_from_slice(&[0xFF, 0xFF]);
            });
        }
        frame(63, &header); // write-test frame
        MemCard { data, flag: FLAG_NEW, writes: 0 }
    }

    pub fn sector(&self, n: u16) -> &[u8] {
        &self.data[n as usize * SECTOR..(n as usize + 1) * SECTOR]
    }

    pub(crate) fn write_sector(&mut self, n: u16, bytes: &[u8]) {
        self.data[n as usize * SECTOR..(n as usize + 1) * SECTOR].copy_from_slice(bytes);
        self.flag &= !FLAG_NEW;
        self.writes += 1;
    }
}

/// One save in the directory, from its first block.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SaveInfo {
    /// Directory slot (1-15) = data block number of the first block.
    pub block: usize,
    /// Product code and name, e.g. `BESLES-00000NAME`.
    pub filename: String,
    pub size: u32,
    /// Blocks used, following the directory's next-block links.
    pub blocks: Vec<usize>,
    /// Title from the save's header frame (Shift-JIS, common characters converted).
    pub title: String,
    /// Icon frames (1-3) from the header's icon flag.
    pub icon_frames: u8,
}

/// Shift-JIS to text for the characters save titles use: ASCII, full-width
/// letters, digits and common punctuation; anything else becomes `?`.
pub fn sjis_title(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != 0 {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        let Some(&c) = bytes.get(i + 1) else { break };
        i += 2;
        out.push(match (b, c) {
            (0x81, 0x40) => ' ',
            (0x81, 0x43) => ',',
            (0x81, 0x44) => '.',
            (0x81, 0x46) => ':',
            (0x81, 0x48) => '?',
            (0x81, 0x49) => '!',
            (0x81, 0x5B) | (0x81, 0x5D) => '-',
            (0x81, 0x5E) => '/',
            (0x81, 0x69) => '(',
            (0x81, 0x6A) => ')',
            (0x81, 0x7B) => '+',
            (0x81, 0x93) => '%',
            (0x81, 0x95) => '&',
            (0x81, 0x96) => '*',
            (0x82, 0x4F..=0x58) => (b'0' + c - 0x4F) as char,
            (0x82, 0x60..=0x79) => (b'A' + c - 0x60) as char,
            (0x82, 0x81..=0x9A) => (b'a' + c - 0x81) as char,
            _ => '?',
        });
    }
    out.trim_end().to_string()
}

impl MemCard {
    /// Saves listed in the directory (frames 1-15), first blocks only.
    pub fn saves(&self) -> Vec<SaveInfo> {
        let mut out = Vec::new();
        for slot in 1..16 {
            let d = self.sector(slot as u16);
            if d[0] != 0x51 {
                continue; // not the first block of a save in use
            }
            let mut blocks = vec![slot];
            let mut next = u16::from_le_bytes([d[8], d[9]]);
            while next != 0xFFFF && blocks.len() < 15 {
                let b = next as usize + 1;
                blocks.push(b);
                next = u16::from_le_bytes([self.sector(b as u16)[8], self.sector(b as u16)[9]]);
            }
            let header = self.sector(slot as u16 * 64);
            let filename = d[10..30].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
            out.push(SaveInfo {
                block: slot,
                filename,
                size: u32::from_le_bytes(d[4..8].try_into().unwrap()),
                blocks,
                title: if &header[..2] == b"SC" { sjis_title(&header[4..68]) } else { String::new() },
                icon_frames: if &header[..2] == b"SC" { header[2] & 0x0F } else { 0 },
            });
        }
        out
    }
}

/// A card's end of the serial link (Mednafen's `InputDevice_Memcard`).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CardPort {
    link: crate::pad::Link,
    command: u8,
    addr: u16,
    calced_xor: u8,
    write_xor: u8,
    rw: Vec<u8>,
}

impl CardPort {
    pub fn set_dtr(&mut self, dtr: bool) {
        self.link.set_dtr(dtr);
    }

    /// One bit each way. Returns the bit sent and the DSR pulse delay.
    pub fn clock(&mut self, card: &mut MemCard, txd: bool) -> (bool, i32) {
        if !self.link.dtr() {
            return (true, 0);
        }
        let (out, done) = self.link.shift(txd);
        if done {
            self.byte(card);
        }
        (out, self.link.dsr(0x100))
    }

    fn byte(&mut self, card: &mut MemCard) {
        let rx = self.link.rx;
        let l = &mut self.link;
        let phase = l.phase;
        match phase {
            0 if rx != 0x81 => l.phase = -1,
            0 => {
                l.send(&[card.flag & FLAG_NEW]);
                l.phase = 1;
            }
            1 => {
                self.command = rx;
                if rx == b'R' || rx == b'W' {
                    l.phase = 2;
                    l.send(&[0x5A]);
                } else {
                    l.phase = -1;
                    l.tx[0] = 0;
                    l.count = 0;
                }
            }
            2 => {
                l.send(&[0x5D]);
                l.phase = 3;
            }
            3 => {
                l.send(&[0]);
                l.phase = if self.command == b'R' { 1000 } else { 2000 };
            }
            // Read: the address (echoed), 5Ch 5Dh, the address, 128 bytes, XOR, 47h.
            1000 => {
                self.addr = (rx as u16) << 8;
                l.send(&[rx]);
                l.phase += 1;
            }
            1001 => {
                self.addr |= rx as u16;
                l.send(b"\\");
                l.phase += 1;
            }
            1002 => {
                if self.addr as usize >= SIZE / SECTOR {
                    self.addr = 0xFFFF;
                }
                self.calced_xor = 0;
                l.send(b"]");
                l.phase += 1;
            }
            1003 => {
                let b = (self.addr >> 8) as u8;
                self.calced_xor ^= b;
                l.send(&[b]);
                l.phase += 1;
            }
            1004 => {
                let b = self.addr as u8;
                self.calced_xor ^= b;
                l.send(&[b]);
                l.phase = if self.addr == 0xFFFF { -1 } else { 1024 };
            }
            1024..=1151 => {
                let b = card.sector(self.addr)[(phase - 1024) as usize];
                self.calced_xor ^= b;
                l.send(&[b]);
                l.phase += 1;
            }
            1152 => {
                l.send(&[self.calced_xor]);
                l.phase += 1;
            }
            1153 => {
                l.send(b"G");
                l.phase = -1;
            }
            // Write: the address, 128 bytes and the XOR (each echoed), 5Ch 5Dh, the end status.
            2000 => {
                self.calced_xor = rx;
                self.addr = (rx as u16) << 8;
                l.send(&[rx]);
                l.phase += 1;
            }
            2001 => {
                self.calced_xor ^= rx;
                self.addr |= rx as u16;
                self.rw = vec![0; SECTOR];
                l.send(&[rx]);
                l.phase = 2048;
            }
            2048..=2175 => {
                self.calced_xor ^= rx;
                self.rw[(phase - 2048) as usize] = rx;
                l.send(&[rx]);
                l.phase += 1;
            }
            2176 => {
                self.write_xor = rx;
                l.send(b"\\");
                l.phase += 1;
            }
            2177 => {
                l.send(b"]");
                l.phase += 1;
            }
            2178 => {
                let status = if self.calced_xor != self.write_xor {
                    b'N'
                } else if self.addr as usize >= SIZE / SECTOR {
                    0xFF
                } else {
                    card.write_sector(self.addr, &self.rw);
                    b'G'
                };
                l.send(&[status]);
                l.phase = -1;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command after 81h; returns every byte received and checks DSR
    /// follows all but the last.
    fn run(card: &mut MemCard, cmd: u8, tx: &[u8]) -> Vec<u8> {
        let mut port = CardPort::default();
        port.set_dtr(true);
        let mut rx = Vec::new();
        for (i, &b) in [0x81, cmd].iter().chain(tx).enumerate() {
            let (r, ack) = crate::pad::tests::byte(&mut |t| port.clock(card, t), b);
            rx.push(r);
            assert_eq!(ack, i < tx.len() + 1, "ack at byte {i}");
        }
        rx[1..].to_vec()
    }

    #[test]
    fn formatted_card_has_valid_frames() {
        let c = MemCard::formatted();
        assert_eq!(&c.sector(0)[..2], b"MC");
        assert_eq!(c.sector(0)[127], b'M' ^ b'C');
        assert_eq!(c.sector(1)[0], 0xA0);
        assert_eq!(c.sector(1)[127], checksum(c.sector(1)));
    }

    #[test]
    fn write_then_read_round_trips_and_clears_new_flag() {
        let mut c = MemCard::formatted();
        let data: Vec<u8> = (0..128).map(|i| i as u8 ^ 0x5A).collect();
        let sum = data.iter().fold(0x01 ^ 0x23, |a, b| a ^ b);
        let mut tx = vec![0, 0, 0x01, 0x23];
        tx.extend(&data);
        tx.extend([sum, 0, 0, 0]);
        let rx = run(&mut c, 0x57, &tx);
        assert_eq!(rx[0], 0x08, "new-card flag before the first write");
        assert_eq!(&rx[rx.len() - 3..], &[0x5C, 0x5D, 0x47]);
        assert_eq!(c.sector(0x123), &data[..]);

        let mut tx = vec![0, 0, 0x01, 0x23];
        tx.extend([0u8; 4 + 128 + 2]);
        let rx = run(&mut c, 0x52, &tx);
        assert_eq!(rx[0], 0x00, "flag cleared");
        assert_eq!(&rx[1..3], &[0x5A, 0x5D]);
        assert_eq!(&rx[5..9], &[0x5C, 0x5D, 0x01, 0x23]);
        assert_eq!(&rx[9..137], &data[..]);
        assert_eq!(rx[137], sum);
        assert_eq!(rx[138], 0x47);
    }

    #[test]
    fn bad_checksum_is_rejected() {
        let mut c = MemCard::formatted();
        let mut tx = vec![0, 0, 0x00, 0x40];
        tx.extend([0xAA; 128]);
        tx.extend([0x00, 0, 0, 0]); // wrong checksum
        let rx = run(&mut c, 0x57, &tx);
        assert_eq!(*rx.last().unwrap(), 0x4E);
        assert_eq!(c.sector(0x40), &[0u8; 128][..]);
    }

    #[test]
    fn titles_and_directory() {
        assert_eq!(sjis_title(&[0x82, 0x73, 0x82, 0x8F, 0x82, 0x8D, 0x82, 0x82, 0x81, 0x40, 0x82, 0x51, 0]), "Tomb 2");
        let mut c = MemCard::formatted();
        c.data[128] = 0x51;
        c.data[128 + 4..128 + 8].copy_from_slice(&8192u32.to_le_bytes());
        c.data[128 + 10..128 + 16].copy_from_slice(b"BESLES");
        c.data[64 * SECTOR..64 * SECTOR + 3].copy_from_slice(b"SC\x11");
        c.data[64 * SECTOR + 4..64 * SECTOR + 8].copy_from_slice(b"Test");
        let s = c.saves();
        assert_eq!(
            (s.len(), s[0].filename.as_str(), s[0].title.as_str(), s[0].blocks.clone()),
            (1, "BESLES", "Test", vec![1])
        );
    }
}

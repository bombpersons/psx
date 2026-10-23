//! PlayStation memory card (psx-spx "Memory Card Read/Write Commands" and
//! "Memory Card Data Format"): 128 KiB as 1024 sectors ("frames") of 128
//! bytes, answering Read (52h), Write (57h) and Get ID (53h) over SIO0.

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

    fn write_sector(&mut self, n: u16, bytes: &[u8]) {
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

/// Progress through one card command.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CardSession {
    command: u8,
    /// Bytes exchanged after the command byte.
    k: u32,
    addr: u16,
    sum: u8,
    last: u8,
    buf: Vec<u8>,
}

impl CardSession {
    /// The command byte (second byte after 81h). Returns the reply and
    /// whether the card acknowledges.
    pub fn start(card: &MemCard, command: u8) -> (Self, u8, bool) {
        let ok = matches!(command, 0x52 | 0x57 | 0x53);
        (CardSession { command, ..Default::default() }, card.flag, ok)
    }

    /// One further byte. Returns `(reply, ack)`; no ack ends the command.
    pub fn exchange(&mut self, card: &mut MemCard, tx: u8) -> (u8, bool) {
        let k = self.k;
        self.k += 1;
        match (self.command, k) {
            (_, 0) => (0x5A, true),
            (_, 1) => (0x5D, true),
            // Get ID: 5C 5D 04 00 00 80.
            (0x53, 2) => (0x5C, true),
            (0x53, 3) => (0x5D, true),
            (0x53, 4) => (0x04, true),
            (0x53, 5 | 6) => (0x00, true),
            (0x53, _) => (0x80, false),
            // Address (both commands): MSB, LSB.
            (_, 2) => {
                self.addr = (tx as u16) << 8;
                self.last = tx;
                (0x00, true)
            }
            (_, 3) => {
                self.addr |= tx as u16;
                self.sum = (self.addr >> 8) as u8 ^ tx;
                let r = self.last;
                self.last = tx;
                (r, true)
            }
            // Read: 5C 5D MSB LSB, 128 data, checksum, 47h.
            (0x52, 4) => (0x5C, true),
            (0x52, 5) => (0x5D, true),
            (0x52, 6) if self.addr > 0x3FF => (0xFF, true),
            (0x52, 7) if self.addr > 0x3FF => (0xFF, false),
            (0x52, 6) => ((self.addr >> 8) as u8, true),
            (0x52, 7) => (self.addr as u8, true),
            (0x52, 8..=135) => {
                let b = card.sector(self.addr)[(k - 8) as usize];
                self.sum ^= b;
                (b, true)
            }
            (0x52, 136) => (self.sum, true),
            (0x52, _) => (0x47, false),
            // Write: 128 data, checksum (each echoed one byte late), 5C 5D, end status.
            (0x57, 4..=131) => {
                self.buf.push(tx);
                self.sum ^= tx;
                let r = self.last;
                self.last = tx;
                (r, true)
            }
            (0x57, 132) => {
                let good = tx == self.sum;
                self.sum = if good { 0x47 } else { 0x4E };
                let r = self.last;
                (r, true)
            }
            (0x57, 133) => (0x5C, true),
            (0x57, 134) => (0x5D, true),
            (0x57, _) => {
                if self.addr > 0x3FF {
                    return (0xFF, false);
                }
                if self.sum == 0x47 {
                    let buf = std::mem::take(&mut self.buf);
                    card.write_sector(self.addr, &buf);
                }
                (self.sum, false)
            }
            _ => (0xFF, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(card: &mut MemCard, cmd: u8, tx: &[u8]) -> Vec<u8> {
        let (mut s, flag, ok) = CardSession::start(card, cmd);
        assert!(ok);
        let mut rx = vec![flag];
        for (i, &b) in tx.iter().enumerate() {
            let (r, ack) = s.exchange(card, b);
            rx.push(r);
            assert_eq!(ack, i + 1 < tx.len(), "ack at byte {i}");
        }
        rx
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

    #[test]
    fn get_id() {
        let mut c = MemCard::formatted();
        let rx = run(&mut c, 0x53, &[0; 8]);
        assert_eq!(&rx[1..], &[0x5A, 0x5D, 0x5C, 0x5D, 0x04, 0x00, 0x00, 0x80]);
    }
}

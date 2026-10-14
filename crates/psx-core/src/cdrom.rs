//! CD-ROM controller (psx-spx "CDROM Controller I/O Ports" and "CDROM Commands").
//!
//! Modelled: index register; parameter, response and data FIFOs; interrupt
//! enable/flag with acknowledge; queued responses delivered one at a time
//! after the previous is acknowledged; the common command set; sector reads
//! at single/double speed in 0x800/0x924-byte modes; XA audio sectors withheld
//! from the data path when XA-ADPCM is enabled; DMA channel 3 reads.
//!
//! Timings are approximations (documented per constant), not measured.
//! Not modelled: CD-DA/XA audio output, subchannel Q beyond Getloc, the
//! sound-map registers, seek-time physics.

use std::collections::VecDeque;

use psx_media::cue::TrackMode;
use psx_media::Disc;

/// Delay from command to its first (INT3) response, in system clocks (~0.75 ms).
const ACK_DELAY: u64 = 25_000;
/// Delay of a second (INT2) response for short operations (~3 ms).
const COMPLETE_DELAY: u64 = 100_000;
/// Init / GetID / ReadTOC completion (~15 ms; real drives can be slower).
const SLOW_COMPLETE_DELAY: u64 = 500_000;
/// Seek before the first sector of a read (~3 ms; no distance model).
const SEEK_DELAY: u64 = 100_000;
const SECTOR_SINGLE: u64 = 33_868_800 / 75;
const SECTOR_DOUBLE: u64 = 33_868_800 / 150;

/// Controller firmware date/version returned by Test(20h): 99/02/01 C3, as
/// reported by PCSX-Redux for SCPH-7502-era hardware.
const TEST_VERSION: [u8; 4] = [0x99, 0x02, 0x01, 0xC3];

mod stat {
    pub const ERROR: u8 = 0x01;
    pub const MOTOR: u8 = 0x02;
    pub const ID_ERROR: u8 = 0x08;
    pub const SHELL_OPEN: u8 = 0x10;
    pub const READ: u8 = 0x20;
    pub const SEEK: u8 = 0x40;
    pub const PLAY: u8 = 0x80;
}

#[derive(Clone, Debug)]
struct Response {
    at: u64,
    int: u8,
    bytes: Vec<u8>,
}

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

fn unbcd(v: u8) -> u32 {
    (v >> 4) as u32 * 10 + (v & 15) as u32
}

/// LBA (no lead-in) to absolute MSF bytes.
fn msf(lba: u32) -> [u8; 3] {
    let a = lba + 150;
    [bcd(a / 4500), bcd((a / 75) % 60), bcd(a % 75)]
}

pub struct Cdrom {
    index: u8,
    params: VecDeque<u8>,
    response: VecDeque<u8>,
    ie: u8,
    iflag: u8,
    queue: VecDeque<Response>,
    busy_until: u64,
    pub disc: Option<Disc>,
    region: [u8; 4],
    status: u8,
    mode: u8,
    setloc: Option<u32>,
    /// Next sector the head will deliver.
    pos: u32,
    reading: bool,
    next_sector_at: u64,
    /// Last delivered sector, raw 2352 bytes.
    sector: Vec<u8>,
    data: Vec<u8>,
    data_pos: usize,
    filter: (u8, u8),
    /// Sectors delivered as INT1 since power-on.
    pub sectors_delivered: u64,
    /// Commands the controller received that are not implemented.
    pub unknown_commands: Vec<u8>,
}

impl Default for Cdrom {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Cdrom {
    pub fn new(disc: Option<Disc>) -> Self {
        let region = disc.as_ref().map(region_of).unwrap_or(*b"SCEE");
        let status = if disc.is_some() { stat::MOTOR } else { stat::SHELL_OPEN };
        Cdrom {
            index: 0,
            params: VecDeque::new(),
            response: VecDeque::new(),
            ie: 0,
            iflag: 0,
            queue: VecDeque::new(),
            busy_until: 0,
            disc,
            region,
            status,
            mode: 0,
            setloc: None,
            pos: 0,
            reading: false,
            next_sector_at: 0,
            sector: Vec::new(),
            data: Vec::new(),
            data_pos: 0,
            filter: (0, 0),
            sectors_delivered: 0,
            unknown_commands: Vec::new(),
        }
    }

    /// Interrupt output (I_STAT bit 2).
    pub fn irq(&self) -> bool {
        self.iflag & self.ie & 0x1F != 0
    }

    pub fn read(&mut self, reg: u32, now: u64) -> u8 {
        match (reg, self.index) {
            (0, _) => {
                let mut s = self.index;
                s |= ((self.params.is_empty()) as u8) << 3;
                s |= ((self.params.len() < 16) as u8) << 4;
                s |= ((!self.response.is_empty()) as u8) << 5;
                s |= ((self.data_pos < self.data.len()) as u8) << 6;
                s |= ((now < self.busy_until) as u8) << 7;
                s
            }
            (1, _) => self.response.pop_front().unwrap_or(0),
            (2, _) => self.read_data_byte(),
            (3, 0 | 2) => self.ie | 0xE0,
            (3, _) => self.iflag | 0xE0,
            _ => 0,
        }
    }

    pub fn write(&mut self, reg: u32, v: u8, now: u64) {
        match (reg, self.index) {
            (0, _) => self.index = v & 3,
            (1, 0) => self.command(v, now),
            (2, 0) => {
                if self.params.len() < 16 {
                    self.params.push_back(v)
                }
            }
            (2, 1) => self.ie = v & 0x1F,
            (3, 0) => {
                if v & 0x80 != 0 {
                    // Want data: load the last delivered sector into the data FIFO.
                    if self.data_pos >= self.data.len() {
                        self.data = self.fifo_payload();
                        self.data_pos = 0;
                    }
                } else {
                    self.data.clear();
                    self.data_pos = 0;
                }
            }
            (3, 1) => {
                self.iflag &= !(v & 0x1F);
                if v & 0x40 != 0 {
                    self.params.clear();
                }
                if self.iflag == 0 {
                    self.response.clear();
                }
            }
            _ => {} // sound map and volume registers
        }
    }

    fn read_data_byte(&mut self) -> u8 {
        let b = self.data.get(self.data_pos).copied().unwrap_or(0);
        if self.data_pos < self.data.len() {
            self.data_pos += 1;
        }
        b
    }

    /// DMA channel 3: next word from the data FIFO.
    pub fn dma_word(&mut self) -> u32 {
        let b = [self.read_data_byte(), self.read_data_byte(), self.read_data_byte(), self.read_data_byte()];
        u32::from_le_bytes(b)
    }

    fn fifo_payload(&self) -> Vec<u8> {
        if self.sector.len() < 2352 {
            return Vec::new();
        }
        if self.mode & 0x20 != 0 {
            self.sector[12..12 + 0x924].to_vec()
        } else if self.sector[15] == 1 {
            self.sector[16..16 + 0x800].to_vec()
        } else {
            self.sector[24..24 + 0x800].to_vec()
        }
    }

    fn respond(&mut self, at: u64, int: u8, bytes: Vec<u8>) {
        self.queue.push_back(Response { at, int, bytes });
    }

    fn command(&mut self, cmd: u8, now: u64) {
        let p: Vec<u8> = self.params.drain(..).collect();
        let ack = now + ACK_DELAY;
        self.busy_until = ack;
        let has_disc = self.disc.is_some();
        match cmd {
            0x01 => {
                let s = self.status;
                self.respond(ack, 3, vec![s]);
                if has_disc {
                    self.status &= !stat::SHELL_OPEN;
                }
            }
            0x02 if p.len() >= 3 => {
                let lba = (unbcd(p[0]) * 60 + unbcd(p[1])) * 75 + unbcd(p[2]);
                self.setloc = Some(lba.saturating_sub(150));
                self.respond(ack, 3, vec![self.status]);
            }
            0x03 => {
                self.reading = false;
                self.status = (self.status & !(stat::READ | stat::SEEK)) | stat::PLAY | stat::MOTOR;
                self.respond(ack, 3, vec![self.status]);
            }
            0x06 | 0x1B => {
                if !has_disc {
                    self.respond(ack, 5, vec![self.status | stat::ERROR, 0x80]);
                    return;
                }
                if let Some(l) = self.setloc.take() {
                    self.pos = l;
                }
                self.status = (self.status & !stat::PLAY) | stat::MOTOR | stat::READ;
                self.respond(ack, 3, vec![self.status]);
                self.reading = true;
                self.next_sector_at = ack + SEEK_DELAY + self.sector_period();
            }
            0x07 => {
                self.status |= stat::MOTOR;
                self.respond(ack, 3, vec![self.status]);
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x08 => {
                self.reading = false;
                self.status &= !(stat::READ | stat::PLAY | stat::SEEK);
                self.respond(ack, 3, vec![self.status]);
                self.status &= !stat::MOTOR;
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x09 => {
                self.respond(ack, 3, vec![self.status]);
                self.reading = false;
                self.status &= !(stat::READ | stat::PLAY | stat::SEEK);
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x0A => {
                self.reading = false;
                self.mode = 0;
                self.status = (self.status & stat::SHELL_OPEN) | if has_disc { stat::MOTOR } else { 0 };
                self.respond(ack, 3, vec![self.status]);
                self.respond(ack + SLOW_COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x0B | 0x0C => self.respond(ack, 3, vec![self.status]),
            0x0D if p.len() >= 2 => {
                self.filter = (p[0], p[1]);
                self.respond(ack, 3, vec![self.status]);
            }
            0x0E if !p.is_empty() => {
                self.mode = p[0];
                self.respond(ack, 3, vec![self.status]);
            }
            0x0F => self.respond(ack, 3, vec![self.status, self.mode, 0, self.filter.0, self.filter.1]),
            0x10 => {
                let s = &self.sector;
                let v = if s.len() >= 20 { s[12..20].to_vec() } else { vec![0; 8] };
                self.respond(ack, 3, v);
            }
            0x11 => {
                let lba = self.pos.saturating_sub(1);
                let (track, start) = self.track_of(lba);
                let rel = msf(lba.saturating_sub(start).saturating_sub(150));
                let abs = msf(lba);
                self.respond(ack, 3, vec![bcd(track), 1, rel[0], rel[1], rel[2], abs[0], abs[1], abs[2]]);
            }
            0x13 => {
                let last = self.disc.as_ref().map_or(1, |d| d.tracks.len() as u32);
                self.respond(ack, 3, vec![self.status, 1, bcd(last)]);
            }
            0x14 => {
                let t = p.first().map_or(0, |&v| unbcd(v));
                let lba = match &self.disc {
                    Some(d) if t == 0 => d.end_lba(),
                    Some(d) => d.tracks.iter().find(|x| x.number as u32 == t).map_or(0, |x| x.start_lba),
                    None => 0,
                };
                let m = msf(lba);
                self.respond(ack, 3, vec![self.status, m[0], m[1]]);
            }
            0x15 | 0x16 => {
                self.reading = false;
                if let Some(l) = self.setloc.take() {
                    self.pos = l;
                }
                self.status = (self.status & !(stat::READ | stat::PLAY)) | stat::MOTOR;
                self.respond(ack, 3, vec![self.status | stat::SEEK]);
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x19 => match p.first() {
                Some(0x20) => self.respond(ack, 3, TEST_VERSION.to_vec()),
                _ => self.respond(ack, 3, vec![self.status]),
            },
            0x1A => {
                self.respond(ack, 3, vec![self.status]);
                if has_disc {
                    let mut v = vec![stat::MOTOR, 0x00, 0x20, 0x00];
                    v.extend_from_slice(&self.region);
                    self.respond(ack + SLOW_COMPLETE_DELAY, 2, v);
                } else {
                    self.respond(ack + SLOW_COMPLETE_DELAY, 5, vec![stat::ID_ERROR, 0x40, 0, 0, 0, 0, 0, 0]);
                }
            }
            0x1E => {
                self.respond(ack, 3, vec![self.status]);
                self.respond(ack + SLOW_COMPLETE_DELAY, 2, vec![self.status]);
            }
            _ => {
                self.unknown_commands.push(cmd);
                self.respond(ack, 5, vec![self.status | stat::ERROR, 0x40]);
            }
        }
    }

    fn sector_period(&self) -> u64 {
        if self.mode & 0x80 != 0 {
            SECTOR_DOUBLE
        } else {
            SECTOR_SINGLE
        }
    }

    fn track_of(&self, lba: u32) -> (u32, u32) {
        self.disc
            .as_ref()
            .and_then(|d| d.tracks.iter().rev().find(|t| t.start_lba <= lba))
            .map_or((1, 0), |t| (t.number as u32, t.start_lba))
    }

    /// Advance to `now`; returns true when the interrupt line is asserted.
    pub fn tick(&mut self, now: u64) -> bool {
        if self.reading && now >= self.next_sector_at {
            self.next_sector_at += self.sector_period();
            self.deliver_sector(now);
        }
        if self.iflag == 0 {
            if let Some(r) = self.queue.front() {
                if now >= r.at {
                    let r = self.queue.pop_front().unwrap();
                    self.iflag = r.int;
                    self.response = r.bytes.into();
                }
            }
        }
        self.irq()
    }

    fn deliver_sector(&mut self, now: u64) {
        let lba = self.pos;
        self.pos += 1;
        let Some(disc) = &self.disc else { return };
        let raw = match disc.read_raw(lba) {
            Ok((raw, mode)) if mode != TrackMode::Audio && raw.len() == 2352 => raw,
            _ => return,
        };
        // XA-ADPCM enabled: real-time audio sectors go to the SPU, not the data FIFO.
        let submode = raw[18];
        if self.mode & 0x40 != 0 && submode & 0x04 != 0 && raw[15] == 2 {
            return;
        }
        self.sector = raw;
        self.sectors_delivered += 1;
        // A newer sector replaces an undelivered INT1.
        self.queue.retain(|r| r.int != 1);
        let s = self.status;
        self.respond(now, 1, vec![s]);
    }
}

/// Region letters for GetID from the licence text in sector 4.
fn region_of(d: &Disc) -> [u8; 4] {
    let text = d.read_data(4).map(|s| s.data).unwrap_or_default();
    let t = String::from_utf8_lossy(&text);
    if t.contains("Europe") {
        *b"SCEE"
    } else if t.contains("America") {
        *b"SCEA"
    } else {
        *b"SCEI"
    }
}

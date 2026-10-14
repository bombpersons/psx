//! CD-ROM controller (psx-spx "CDROM Controller I/O Ports" and "CDROM Commands").
//!
//! Modelled: index register; parameter, response and data FIFOs; interrupt
//! enable/flag with acknowledge; queued responses delivered one at a time
//! after the previous is acknowledged; the common command set; sector reads
//! at single/double speed in 0x800/0x924-byte modes; DMA channel 3 reads;
//! CD-DA playback and XA-ADPCM sectors (with Setfilter) decoded to the SPU's
//! CD input through Mute/Demute and the CD volume matrix.
//!
//! Timings are approximations (documented per constant), not measured.
//! Not modelled: XA from RAM (sound map data out), XA emphasis and the
//! hardware's zig-zag resampler, subchannel Q beyond Getloc, seek-time physics.

use std::collections::VecDeque;

use psx_media::cue::TrackMode;
use psx_media::Disc;

/// Delay from command to its first (INT3) response, in system clocks (~0.75 ms).
const ACK_DELAY: u64 = 25_000;
/// Delay of a second (INT2) response for short operations (~3 ms).
const COMPLETE_DELAY: u64 = 100_000;
/// Init / GetID / ReadTOC completion (~15 ms; real drives can be slower).
const SLOW_COMPLETE_DELAY: u64 = 500_000;
/// Seek model (PCSX-Redux's approximation): 3 µs per sector of distance plus
/// 1 ms before a read or 167 ms for an explicit seek, plus 650 ms after a
/// speed change. Games rely on these windows to finish setup before data flows.
const CYCLES_PER_US: u64 = 34; // 33.8688 MHz
const SEEK_PER_SECTOR: u64 = 3 * CYCLES_PER_US;
const SEEK_BASE_READ: u64 = 1_000 * CYCLES_PER_US;
const SEEK_BASE_EXPLICIT: u64 = 167_000 * CYCLES_PER_US;
const SPEED_CHANGE_PENALTY: u64 = 650_000 * CYCLES_PER_US;
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    rel_msf(lba + 150)
}

/// A sector count as BCD MSF without the lead-in (track-relative times).
fn rel_msf(sectors: u32) -> [u8; 3] {
    [bcd(sectors / 4500), bcd((sectors / 75) % 60), bcd(sectors % 75)]
}

/// One entry of [`Cdrom::recent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentCommand {
    pub time: u64,
    pub command: u8,
    pub params: Vec<u8>,
    /// INT types queued in reply.
    pub ints: Vec<u8>,
    pub repeats: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cdrom {
    index: u8,
    params: VecDeque<u8>,
    response: VecDeque<u8>,
    ie: u8,
    iflag: u8,
    queue: VecDeque<Response>,
    busy_until: u64,
    /// Not saved: a restored machine gets the disc reinserted by the caller.
    #[serde(skip)]
    pub disc: Option<Disc>,
    region: [u8; 4],
    status: u8,
    mode: u8,
    setloc: Option<u32>,
    /// Next sector the head will deliver.
    pos: u32,
    /// Where the head is now (for seek distance).
    head: u32,
    /// Position Getloc reports: the seek target after a seek, else the last sector read/played.
    loc: u32,
    speed_changed: bool,
    reading: bool,
    /// CD-DA playback in progress (head advancing through an audio track).
    playing: bool,
    /// Track being played, and sectors since the last position report.
    play_track: u8,
    report_count: u32,
    /// Decoded CD-DA stereo samples waiting for the SPU's CD input.
    pub audio: VecDeque<(i16, i16)>,
    next_sector_at: u64,
    /// Last delivered sector, raw 2352 bytes, and its LBA.
    sector: Vec<u8>,
    sector_lba: u32,
    /// LBA of the sector currently in the data FIFO.
    fifo_lba: u32,
    /// Every DMA3 transfer: (lba of FIFO data, FIFO offset, bytes). Enabled by `log_dma`.
    #[serde(skip)]
    pub dma_log: Vec<(u32, u32, u32)>,
    /// Commands as (time, command, params) when `log_dma` is set.
    #[serde(skip)]
    pub cmd_log: Vec<(u64, u8, Vec<u8>)>,
    /// The last 64 distinct commands. Consecutive identical commands (e.g.
    /// GetlocP polling) are counted, not stored.
    #[serde(skip)]
    pub recent: VecDeque<RecentCommand>,
    /// Interrupt events as (time, what, detail) when `log_dma` is set.
    #[serde(skip)]
    pub irq_log: Vec<(u64, &'static str, u32)>,
    #[serde(skip)]
    pub log_dma: bool,
    data: Vec<u8>,
    data_pos: usize,
    filter: (u8, u8),
    /// Sectors delivered as INT1 since power-on.
    pub sectors_delivered: u64,
    /// XA-ADPCM decoder, and XA sectors decoded since power-on.
    xa: crate::xa::XaDecoder,
    pub xa_sectors: u64,
    /// Mute/Demute commands: silence CD-DA and XA output.
    muted: bool,
    /// Sound map register bit 0: silence XA only.
    xa_muted: bool,
    /// CD-to-SPU volume matrix `[L->L, L->R, R->R, R->L]` (0x80 = 100%):
    /// being written, and as applied (bit 5 of the apply register).
    vol_pending: [u8; 4],
    vol: [u8; 4],
    /// Commands the controller received that are not implemented.
    #[serde(skip)]
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
            head: 0,
            loc: 0,
            speed_changed: false,
            reading: false,
            playing: false,
            play_track: 0,
            report_count: 0,
            audio: VecDeque::new(),
            next_sector_at: 0,
            sector: Vec::new(),
            sector_lba: 0,
            fifo_lba: 0,
            dma_log: Vec::new(),
            cmd_log: Vec::new(),
            recent: VecDeque::new(),
            irq_log: Vec::new(),
            log_dma: false,
            data: Vec::new(),
            data_pos: 0,
            filter: (0, 0),
            sectors_delivered: 0,
            xa: Default::default(),
            xa_sectors: 0,
            muted: false,
            xa_muted: false,
            vol_pending: [0x80, 0, 0x80, 0],
            vol: [0x80, 0, 0x80, 0],
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
                        self.fifo_lba = self.sector_lba;
                    }
                    if self.log_dma {
                        self.irq_log.push((now, "want-data", self.fifo_lba));
                    }
                } else {
                    self.data.clear();
                    self.data_pos = 0;
                }
            }
            (3, 1) => {
                if self.log_dma {
                    self.irq_log.push((now, "ack", v as u32));
                }
                self.iflag &= !(v & 0x1F);
                if v & 0x40 != 0 {
                    self.params.clear();
                }
                if self.iflag == 0 {
                    self.response.clear();
                }
            }
            (2, 2) => self.vol_pending[0] = v,
            (3, 2) => self.vol_pending[1] = v,
            (1, 3) => self.vol_pending[2] = v,
            (2, 3) => self.vol_pending[3] = v,
            (3, 3) => {
                self.xa_muted = v & 1 != 0;
                if v & 0x20 != 0 {
                    self.vol = self.vol_pending;
                }
            }
            _ => {} // sound map data out / coding info (XA from RAM): not modelled
        }
    }

    fn read_data_byte(&mut self) -> u8 {
        let b = self.data.get(self.data_pos).copied().unwrap_or(0);
        if self.data_pos < self.data.len() {
            self.data_pos += 1;
        }
        b
    }

    /// Record one DMA3 transfer of `bytes` starting at the current FIFO position.
    pub fn note_dma(&mut self, bytes: u32) {
        if self.log_dma {
            self.dma_log.push((self.fifo_lba, self.data_pos as u32, bytes.min(self.data.len().saturating_sub(self.data_pos) as u32)));
        }
    }

    /// (LBA in the FIFO, current FIFO offset, sector payload size).
    pub fn fifo_state(&self) -> (u32, u32, u32) {
        (self.fifo_lba, self.data_pos as u32, if self.mode & 0x20 != 0 { 2340 } else { 2048 })
    }

    /// Bytes left in the data FIFO.
    pub fn fifo_remaining(&self) -> u32 {
        self.data.len().saturating_sub(self.data_pos) as u32
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
        let queued_before = self.queue.len();
        let params: Vec<u8> = self.params.iter().copied().collect();
        self.command_inner(cmd, now);
        let ints: Vec<u8> = self.queue.iter().skip(queued_before).map(|r| r.int).collect();
        if let Some(last) = self.recent.back_mut() {
            if last.command == cmd && last.params == params && last.ints == ints {
                last.repeats += 1;
                return;
            }
        }
        if self.recent.len() >= 64 {
            self.recent.pop_front();
        }
        self.recent.push_back(RecentCommand { time: now, command: cmd, params, ints, repeats: 1 });
    }

    fn command_inner(&mut self, cmd: u8, now: u64) {
        let p: Vec<u8> = self.params.drain(..).collect();
        if self.log_dma {
            self.cmd_log.push((now, cmd, p.clone()));
        }
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
                // Play: from the given track (BCD), else from Setloc / the current position.
                self.reading = false;
                if let Some(&t) = p.first().filter(|&&t| t != 0) {
                    let track = unbcd(t);
                    if let Some(st) = self.disc.as_ref().and_then(|d| d.tracks.iter().find(|x| x.number as u32 == track)).map(|x| x.start_lba) {
                        self.pos = st;
                    }
                } else if let Some(l) = self.setloc.take() {
                    self.pos = l;
                }
                self.play_track = self.track_of(self.pos).0 as u8;
                self.report_count = 0;
                self.status = (self.status & !(stat::READ | stat::SEEK)) | stat::PLAY | stat::MOTOR;
                self.respond(ack, 3, vec![self.status]);
                self.playing = true;
                let seek = self.seek_cycles(SEEK_BASE_READ);
                self.next_sector_at = ack + seek + SECTOR_SINGLE;
            }
            0x06 | 0x1B => {
                if !has_disc {
                    self.respond(ack, 5, vec![self.status | stat::ERROR, 0x80]);
                    return;
                }
                if let Some(l) = self.setloc.take() {
                    self.pos = l;
                }
                self.playing = false;
                self.status = (self.status & !stat::PLAY) | stat::MOTOR | stat::READ;
                self.respond(ack, 3, vec![self.status]);
                self.reading = true;
                let seek = self.seek_cycles(SEEK_BASE_READ);
                self.next_sector_at = ack + seek + self.sector_period();
            }
            0x07 => {
                self.status |= stat::MOTOR;
                self.respond(ack, 3, vec![self.status]);
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x08 => {
                self.reading = false;
                self.playing = false;
                self.status &= !(stat::READ | stat::PLAY | stat::SEEK);
                self.respond(ack, 3, vec![self.status]);
                self.status &= !stat::MOTOR;
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x09 => {
                self.respond(ack, 3, vec![self.status]);
                self.reading = false;
                self.playing = false;
                self.status &= !(stat::READ | stat::PLAY | stat::SEEK);
                self.respond(ack + COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x0A => {
                self.reading = false;
                self.playing = false;
                self.mode = 0;
                self.status = (self.status & stat::SHELL_OPEN) | if has_disc { stat::MOTOR } else { 0 };
                self.respond(ack, 3, vec![self.status]);
                self.respond(ack + SLOW_COMPLETE_DELAY, 2, vec![self.status]);
            }
            0x0B | 0x0C => {
                self.muted = cmd == 0x0B;
                self.respond(ack, 3, vec![self.status]);
            }
            0x0D if p.len() >= 2 => {
                self.filter = (p[0], p[1]);
                self.respond(ack, 3, vec![self.status]);
            }
            0x0E if !p.is_empty() => {
                if (self.mode ^ p[0]) & 0x80 != 0 {
                    self.speed_changed = true;
                }
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
                let (track, index, rel, abs) = self.position(self.loc);
                self.respond(ack, 3, vec![track, index, rel[0], rel[1], rel[2], abs[0], abs[1], abs[2]]);
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
                self.playing = false;
                if let Some(l) = self.setloc.take() {
                    self.pos = l;
                }
                self.status = (self.status & !(stat::READ | stat::PLAY)) | stat::MOTOR;
                self.respond(ack, 3, vec![self.status | stat::SEEK]);
                let seek = self.seek_cycles(SEEK_BASE_EXPLICIT);
                self.loc = self.pos;
                self.respond(ack + seek, 2, vec![self.status]);
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

    /// Seek time from the head to `pos`; moves the head and consumes a pending speed change.
    fn seek_cycles(&mut self, base: u64) -> u64 {
        let distance = self.head.abs_diff(self.pos) as u64;
        let mut c = distance * SEEK_PER_SECTOR + base;
        if std::mem::take(&mut self.speed_changed) {
            c += SPEED_CHANGE_PENALTY;
        }
        self.head = self.pos;
        c
    }

    fn sector_period(&self) -> u64 {
        if self.mode & 0x80 != 0 {
            SECTOR_DOUBLE
        } else {
            SECTOR_SINGLE
        }
    }

    /// Track containing `lba` (pregaps belong to the following track) and its INDEX 01 LBA.
    fn track_of(&self, lba: u32) -> (u32, u32) {
        self.disc
            .as_ref()
            .and_then(|d| d.tracks.iter().rev().find(|t| t.first_lba <= lba))
            .map_or((1, 0), |t| (t.number as u32, t.start_lba))
    }

    /// Subchannel-Q style position: (track, index, relative MSF, absolute MSF).
    /// In a pregap (index 0) the relative time counts down to INDEX 01.
    fn position(&self, lba: u32) -> (u8, u8, [u8; 3], [u8; 3]) {
        let (track, start) = self.track_of(lba);
        let (index, rel) = if lba < start { (0, rel_msf(start - lba)) } else { (1, rel_msf(lba - start)) };
        (bcd(track), index, rel, msf(lba))
    }

    /// Advance to `now`; returns true when the interrupt line is asserted.
    pub fn tick(&mut self, now: u64) -> bool {
        if self.reading && now >= self.next_sector_at {
            self.next_sector_at += self.sector_period();
            self.deliver_sector(now);
        }
        if self.playing && now >= self.next_sector_at {
            self.next_sector_at += SECTOR_SINGLE;
            self.play_sector(now);
        }
        if self.iflag == 0 {
            if let Some(r) = self.queue.front() {
                if now >= r.at {
                    let r = self.queue.pop_front().unwrap();
                    if self.log_dma {
                        self.irq_log.push((now, "deliver", (r.int as u32) << 8 | r.bytes.first().copied().unwrap_or(0) as u32));
                    }
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
        self.head = self.pos;
        self.loc = lba;
        let Some(disc) = &self.disc else { return };
        let raw = match disc.read_raw(lba) {
            Ok((raw, mode)) if mode != TrackMode::Audio && raw.len() == 2352 => raw,
            _ => return,
        };
        // XA-ADPCM enabled: real-time audio sectors go to the SPU, not the data FIFO.
        let submode = raw[18];
        if self.mode & 0x40 != 0 && submode & 0x04 != 0 && raw[15] == 2 {
            // Filter (mode bit 3): only the Setfilter file/channel is played.
            let wanted = self.mode & 0x08 == 0 || (raw[16], raw[17]) == self.filter;
            if wanted {
                self.play_xa(&raw);
            }
            if self.log_dma {
                self.irq_log.push((now, if wanted { "xa" } else { "xa-filtered" }, lba));
            }
            return;
        }
        if self.log_dma {
            self.irq_log.push((now, "sector", lba));
        }
        self.sector = raw;
        self.sector_lba = lba;
        self.sectors_delivered += 1;
        // A newer sector replaces an undelivered INT1.
        self.queue.retain(|r| r.int != 1);
        let s = self.status;
        self.respond(now, 1, vec![s]);
    }
}

impl Cdrom {
    /// One CD-DA sector at the head: queue its PCM, report position, stop at track end.
    fn play_sector(&mut self, now: u64) {
        let lba = self.pos;
        let (track, _) = self.track_of(lba);
        let end = self.disc.as_ref().and_then(|d| d.tracks.iter().find(|t| t.number as u32 == track)).map(|t| t.first_lba + t.sectors);
        if track as u8 != self.play_track || end.is_some_and(|e| lba >= e) {
            // End of the track: with autopause (mode bit 1) stop and raise INT4.
            if self.mode & 0x02 != 0 {
                self.playing = false;
                self.status &= !stat::PLAY;
                self.respond(now, 4, vec![self.status]);
                return;
            }
            self.play_track = track as u8;
        }
        if let Some(Ok((raw, mode))) = self.disc.as_ref().map(|d| d.read_raw(lba)) {
            if mode == TrackMode::Audio && raw.len() == 2352 {
                // Keep at most ~0.5 s buffered so a stalled consumer cannot grow it without bound.
                let frames: Vec<(i16, i16)> =
                    raw.as_chunks::<4>().0.iter().map(|f| (i16::from_le_bytes([f[0], f[1]]), i16::from_le_bytes([f[2], f[3]]))).collect();
                self.push_audio(&frames, false);
            }
        }
        self.loc = lba;
        self.pos += 1;
        self.head = self.pos;
        // Report mode (bit 2): a position report every 10 sectors.
        if self.mode & 0x04 != 0 {
            self.report_count += 1;
            if self.report_count.is_multiple_of(10) {
                let (t, index, rel, _) = self.position(lba);
                let s = self.status;
                self.queue.retain(|r| r.int != 1);
                self.respond(now, 1, vec![s, t, index, rel[0], rel[1], rel[2], 0, 0]);
            }
        }
    }
}

impl Cdrom {
    /// Decode an XA-ADPCM sector to the SPU's CD input.
    fn play_xa(&mut self, raw: &[u8]) {
        let coding = crate::xa::Coding::from_byte(raw[19]);
        let native = self.xa.decode(raw, coding);
        let mut out = VecDeque::with_capacity(native.len() * 7 / 3 + 2);
        self.xa.resample(&native, coding.rate, &mut out);
        let out: Vec<(i16, i16)> = out.into();
        self.xa_sectors += 1;
        self.push_audio(&out, true);
    }

    /// Queue 44.1 kHz frames through mute and the volume matrix.
    fn push_audio(&mut self, frames: &[(i16, i16)], xa: bool) {
        // Keep at most ~1 s buffered so a stalled consumer cannot grow it without bound.
        if self.audio.len() >= 44_100 {
            return;
        }
        let silent = self.muted || (xa && self.xa_muted);
        let [ll, lr, rr, rl] = self.vol.map(|v| v as i32);
        for &(l, r) in frames {
            let (l, r) = (l as i32, r as i32);
            let out = if silent {
                (0, 0)
            } else {
                (((l * ll + r * rl) >> 7).clamp(-0x8000, 0x7FFF) as i16, ((r * rr + l * lr) >> 7).clamp(-0x8000, 0x7FFF) as i16)
            };
            self.audio.push_back(out);
        }
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

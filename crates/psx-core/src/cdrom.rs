//! CD-ROM controller: a port of Mednafen's `PS_CDC` (`psx/cdc.cpp`), which
//! psx takes as ground truth, with Mednafen's disc image access
//! (`cdrom/CDAccess_Image.cpp`) for sector and subchannel Q synthesis.
//!
//! The controller runs on its own counters, which [`Cdrom::update`] brings to
//! a timestamp in chunks ending at each counter's expiry, as Mednafen does:
//! - a command's phases: 10500-13500 clocks (random) plus 1815 per parameter
//!   until it is received, 8500 more until it runs, then the command's own
//!   delay before its second response;
//! - the head (`PSRCounter`): seeks (Mednafen's `CalcSeekTime`, random part
//!   included), then one sector per 1/75 s (1/150 s at double speed) through a
//!   two-sector pipeline, so data reaches the buffer two sectors after the
//!   head reads it;
//! - a 2000-clock hold after each interrupt before new results may replace
//!   the old ones (asynchronous results wait for it);
//! - a one second spin-up after a disc is inserted;
//! - the SPU's sample clock: the SPU runs inside the controller's update and
//!   pulls CD audio (CD-DA, and XA-ADPCM resampled to 44.1 kHz with
//!   Mednafen's filter) one sample at a time.
//!
//! Random delays come from Mednafen's PRNG, which only the controller uses,
//! with its power-on seed, so a run is deterministic and matches Mednafen's.
//!
//! Not ported: Mednafen's error correction of damaged sectors (images are
//! clean) and the P/Q parity of synthesized Mode 1 sectors (pregaps and
//! lead-out of Mode 1 discs; their user data and EDC are produced).

use std::collections::VecDeque;

use psx_media::Disc;
use psx_media::cue::TrackMode;

use crate::spu::Spu;

// Drive states (Mednafen's DS_*).
const DS_STANDBY: i32 = -2;
const DS_PAUSED: i32 = -1;
const DS_STOPPED: i32 = 0;
const DS_SEEKING: i32 = 1;
const DS_SEEKING_LOGICAL: i32 = 2;
const DS_SEEKING_LOGICAL2: i32 = 3;
const DS_PLAYING: i32 = 4;
const DS_READING: i32 = 5;

const IRQ_DATA_READY: u8 = 1;
const IRQ_COMPLETE: u8 = 2;
const IRQ_ACKNOWLEDGE: u8 = 3;
const IRQ_DATA_END: u8 = 4;
const IRQ_DISC_ERROR: u8 = 5;

const ERR_BAD_ARGVAL: u8 = 0x10;
const ERR_BAD_NUMARGS: u8 = 0x20;
const ERR_BAD_COMMAND: u8 = 0x40;
const ERR_NOT_READY: u8 = 0x80;

const MODE_SPEED: u8 = 0x80;
const MODE_STRSND: u8 = 0x40;
const MODE_SF: u8 = 0x08;
const MODE_REPORT: u8 = 0x04;
const MODE_AUTOPAUSE: u8 = 0x02;
const MODE_CDDA: u8 = 0x01;

const CLOCK: i64 = 33_868_800;
const DMA_BUFFER_SIZE: usize = 4096;
const AUDIO_BUFFER_SIZE: usize = 0x1000;

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

fn unbcd(v: u8) -> i32 {
    (v >> 4) as i32 * 10 + (v & 15) as i32
}

fn bcd_valid(v: u8) -> bool {
    ((v as u32 + 0x66) ^ v as u32) & 0x110 == 0
}

/// Absolute sector address (LBA + 150) as BCD M:S:F.
fn aba_msf(aba: u32) -> [u8; 3] {
    [bcd(aba / 75 / 60), bcd((aba / 75) % 60), bcd(aba % 75)]
}

/// Mednafen's `MDFN_PseudoRNG` (based on JKISS), reset at power-on.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Prng {
    x: u32,
    y: u32,
    z: u32,
    c: u32,
    lcgo: u64,
}

impl Default for Prng {
    fn default() -> Self {
        Prng { x: 123456789, y: 987654321, z: 43219876, c: 6543217, lcgo: 0xDEAD_BEEF_CAFE_BABE }
    }
}

impl Prng {
    fn next(&mut self) -> u32 {
        self.x = 314527869u32.wrapping_mul(self.x).wrapping_add(1234567);
        self.y ^= self.y << 5;
        self.y ^= self.y >> 7;
        self.y ^= self.y << 22;
        let t = 4294584393u64.wrapping_mul(self.z as u64).wrapping_add(self.c as u64);
        self.c = (t >> 32) as u32;
        self.z = t as u32;
        self.lcgo = 19073486328125u64.wrapping_mul(self.lcgo).wrapping_add(1);
        self.x.wrapping_add(self.y).wrapping_add(self.z) ^ (self.lcgo >> 16) as u32
    }

    fn range(&mut self, min: u32, max: u32) -> u32 {
        let range_m1 = max - min;
        let mut mask = range_m1;
        mask |= mask >> 1;
        mask |= mask >> 2;
        mask |= mask >> 4;
        mask |= mask >> 8;
        mask |= mask >> 16;
        loop {
            let t = self.next() & mask;
            if t <= range_m1 {
                return min + t;
            }
        }
    }
}

/// One entry of [`Cdrom::recent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentCommand {
    pub time: u64,
    /// Actual vblank at command issue, when supplied by the machine bus.
    pub frame: Option<u64>,
    pub last_time: u64,
    pub last_frame: Option<u64>,
    pub command: u8,
    pub params: Vec<u8>,
    /// INT types the command itself raised (asynchronous sector and report
    /// interrupts are not counted).
    pub ints: Vec<u8>,
    pub repeats: u32,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct AudioBuffer {
    /// `[0]` left, `[1]` right.
    samples: [Vec<i16>; 2],
    size: u32,
    /// 7 or 14 for CD-DA (1x or 2x), 3 or 6 for XA (18.9 or 37.8 kHz).
    freq: u32,
    read_pos: u32,
}

impl AudioBuffer {
    fn new() -> Self {
        AudioBuffer { samples: [vec![0; AUDIO_BUFFER_SIZE], vec![0; AUDIO_BUFFER_SIZE]], ..Default::default() }
    }

    fn read(&mut self) -> [i32; 2] {
        let i = self.read_pos as usize & (AUDIO_BUFFER_SIZE - 1);
        self.read_pos += 1;
        [self.samples[0][i] as i32, self.samples[1][i] as i32]
    }
}

/// The disc's layout as Mednafen's image access computes it.
struct Layout {
    first_track: u8,
    last_track: u8,
    /// Mednafen `DISC_TYPE_*`: 0x20 (CD-XA) with any Mode 2 track, else 0.
    disc_type: u8,
    /// Lead-out LBA (end of the last track).
    leadout: i32,
    tracks: Vec<TrackLayout>,
}

#[derive(Clone, Copy)]
struct TrackLayout {
    number: u8,
    mode: TrackMode,
    /// INDEX 01.
    lba: i32,
    /// First sector of the track's range (pregap included; -150 for track 1).
    start: i32,
    /// First stored sector and the end of the stored sectors.
    stored: i32,
    end: i32,
}

impl TrackLayout {
    fn data(&self) -> bool {
        self.mode != TrackMode::Audio
    }
}

fn layout(d: &Disc) -> Layout {
    let mut tracks: Vec<TrackLayout> = d
        .tracks
        .iter()
        .map(|t| TrackLayout {
            number: t.number,
            mode: t.mode,
            lba: t.start_lba as i32,
            start: t.first_lba as i32 - t.unstored_pregap as i32,
            stored: t.first_lba as i32,
            end: (t.first_lba + t.sectors) as i32,
        })
        .collect();
    tracks.sort_by_key(|t| t.number);
    if let Some(t) = tracks.first_mut() {
        t.start -= 150;
    }
    let disc_type = if tracks.iter().any(|t| t.mode == TrackMode::Mode2Raw) { 0x20 } else { 0 };
    Layout {
        first_track: tracks.first().map_or(1, |t| t.number),
        last_track: tracks.last().map_or(1, |t| t.number),
        disc_type,
        leadout: d.end_lba() as i32,
        tracks,
    }
}

impl Layout {
    fn track_index(&self, lba: i32) -> Option<usize> {
        self.tracks.iter().position(|t| lba >= t.start && lba < t.end)
    }

    fn track_lba(&self, number: i32) -> i32 {
        if number == 100 {
            return self.leadout;
        }
        self.tracks.iter().find(|t| t.number as i32 == number).map_or(0, |t| t.lba)
    }
}

fn subq_crc(q: &mut [u8; 12]) {
    let c = psx_media::subq::crc(q[..10].try_into().unwrap());
    q[10] = (c >> 8) as u8;
    q[11] = c as u8;
}

fn subq_crc_ok(q: &[u8; 12]) -> bool {
    psx_media::subq::crc(q[..10].try_into().unwrap()) == u16::from_be_bytes([q[10], q[11]])
}

/// Mednafen `MakeSubPQ`: the Q subchannel of a sector inside a track, after
/// any LibCrypt replacement (whose CRC is bad, as the drive sees it).
fn make_subq(d: &Disc, l: &Layout, ti: usize, lba: i32) -> [u8; 12] {
    let t = &l.tracks[ti];
    let rel = if lba < t.lba { t.lba - 1 - lba } else { lba - t.lba };
    let mut control = if t.data() { 4 } else { 0 };
    // More than 2 s before a data track that follows an audio track: encoded as audio.
    if lba - t.lba < -150 && t.data() && ti > 0 && !l.tracks[ti - 1].data() {
        control = 0;
    }
    let mut q = [0u8; 12];
    q[0] = 1 | control << 4;
    q[1] = bcd(t.number as u32);
    q[2] = if lba >= t.lba { 1 } else { 0 };
    q[3..6].copy_from_slice(&aba_msf(rel as u32));
    q[7..10].copy_from_slice(&aba_msf((lba + 150) as u32));
    subq_crc(&mut q);
    if lba >= 0 {
        if let Some(o) = d.subq.get(&(lba as u32)) {
            let mut q10: [u8; 10] = q[..10].try_into().unwrap();
            o.apply(&mut q10);
            q[..10].copy_from_slice(&q10);
            subq_crc(&mut q);
            if !o.crc_valid() {
                q[10] ^= 0xFF;
                q[11] ^= 0xFF;
            }
        }
    }
    q
}

fn leadout_subq(l: &Layout, lba: i32) -> [u8; 12] {
    let control = if l.tracks.last().is_some_and(|t| t.data()) { 4 } else { 0 };
    let mut q = [0u8; 12];
    q[0] = 1 | control << 4;
    q[1] = 0xAA;
    q[2] = 0x01;
    q[3..6].copy_from_slice(&aba_msf((lba - l.leadout) as u32));
    q[7..10].copy_from_slice(&aba_msf((lba + 150) as u32));
    subq_crc(&mut q);
    q
}

/// CD-ROM EDC: CRC-32 with polynomial 8001801Bh, reflected.
fn edc(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xD801_8001 } else { c >> 1 };
        }
    }
    c
}

const SYNC: [u8; 12] = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];

fn set_header(buf: &mut [u8], aba: u32, mode: u8) {
    buf[..12].copy_from_slice(&SYNC);
    buf[12..15].copy_from_slice(&aba_msf(aba));
    buf[15] = mode;
}

/// Mednafen `encode_mode1_sector` (user data already in place), without P/Q parity.
fn encode_mode1(buf: &mut [u8], aba: u32) {
    set_header(buf, aba, 1);
    let e = edc(&buf[..2064]);
    buf[2064..2068].copy_from_slice(&e.to_le_bytes());
    buf[2068..2076].fill(0);
}

/// Mednafen `encode_mode2_form2_sector`.
fn encode_mode2_form2(buf: &mut [u8], aba: u32) {
    let e = edc(&buf[16..2348]);
    buf[2348..2352].copy_from_slice(&e.to_le_bytes());
    set_header(buf, aba, 2);
}

/// A gap or lead-out sector of a data track (Mednafen synthesizes these).
fn synth_data_sector(buf: &mut [u8], mode: TrackMode, lba: i32) {
    let aba = (lba + 150) as u32;
    match mode {
        TrackMode::Audio => {}
        TrackMode::Mode1Raw | TrackMode::Mode1Cooked => encode_mode1(buf, aba),
        TrackMode::Mode2Raw => {
            buf[12 + 6] = 0x20;
            buf[12 + 10] = 0x20;
            encode_mode2_form2(buf, aba);
        }
    }
}

/// Mednafen `Read_Raw_Sector`: the 2352 bytes and the Q subchannel of any
/// sector from the start of track 1's pregap through the lead-out.
fn read_raw_sector(d: &Disc, l: &Layout, lba: i32) -> (Vec<u8>, [u8; 12]) {
    let mut buf = vec![0u8; 2352];
    if lba >= l.leadout {
        let q = leadout_subq(l, lba);
        if q[0] & 0x40 != 0 {
            let mode = l.tracks.last().map_or(TrackMode::Mode1Raw, |t| t.mode);
            synth_data_sector(&mut buf, mode, lba);
        }
        return (buf, q);
    }
    let Some(ti) = l.track_index(lba) else { return (buf, [0; 12]) };
    let q = make_subq(d, l, ti, lba);
    let t = l.tracks[ti];
    if lba < t.stored || lba >= t.end {
        let mut et = t;
        if lba - t.lba < -150 && t.data() && ti > 0 && !l.tracks[ti - 1].data() {
            et = l.tracks[ti - 1];
        }
        synth_data_sector(&mut buf, et.mode, lba);
        return (buf, q);
    }
    match d.read_raw(lba as u32) {
        Ok((raw, TrackMode::Mode1Cooked)) => {
            let n = raw.len().min(2048);
            buf[16..16 + n].copy_from_slice(&raw[..n]);
            encode_mode1(&mut buf, (lba + 150) as u32);
        }
        Ok((raw, _)) => {
            let n = raw.len().min(2352);
            buf[..n].copy_from_slice(&raw[..n]);
        }
        Err(_) => {}
    }
    (buf, q)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// The disc's SCEx string as Mednafen's `CalcDiscSCEx` decides it (SYSTEM.CNF's
/// boot file prefix, else the licence text in the system area, else
/// `default`, the console's), or None when it isn't a PlayStation disc.
pub fn disc_scex(d: &Disc, default: [u8; 4]) -> Option<[u8; 4]> {
    let first = d.read_data(4).ok()?;
    if first.form == psx_media::disc::SectorForm::Mode1 {
        return None;
    }
    let mut sa = Vec::with_capacity(2048 * 8);
    for lba in 4..12 {
        let mut s = d.read_data(lba).map(|s| s.data).unwrap_or_default();
        s.resize(2048, 0);
        sa.extend_from_slice(&s[..2048]);
    }
    const LOGO_CRC: u32 = 0x0069_C087;
    let region = match region_by_system_cnf(d).or_else(|| region_by_sa(&sa)) {
        Some(r) => r,
        None => {
            let licensed = sa[10..].starts_with(b"Licensed  by");
            if !licensed && crc32(&sa[0x800..0x800 + 0x3278]) != LOGO_CRC {
                let pvd = d.read_data(16).map(|s| s.data).unwrap_or_default();
                if !pvd.starts_with(b"\x01CD001") || pvd.get(8..19) != Some(b"PLAYSTATION") {
                    return None;
                }
            }
            default
        }
    };
    // A Japanese console is strict about the system area; else take America's.
    if region == *b"SCEI" {
        let mut tv = [b' '; 0x41];
        tv[10..20].copy_from_slice(b"Licensed  ");
        tv[20..22].copy_from_slice(b"by");
        tv[32..64].copy_from_slice(b"Sony Computer Entertainment Inc.");
        let mut matches = |end: u8| {
            tv[0x40] = end;
            sa[..0x41] == tv
        };
        if !(matches(0) || matches(10)) || crc32(&sa[0x800..0x800 + 0x3278]) != LOGO_CRC {
            return Some(*b"SCEA");
        }
    }
    Some(region)
}

/// Mednafen `CalcRegion_By_SYSTEMCNF`.
fn region_by_system_cnf(d: &Disc) -> Option<[u8; 4]> {
    let pvd = (16..32).map_while(|l| d.read_data(l).ok()).map(|s| s.data).find(|s| s.first() == Some(&1))?;
    let le32 = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let (root, root_len) = (le32(&pvd, 0x9E), le32(&pvd, 0xA6));
    if root_len >= 10 * 1024 * 1024 {
        return None;
    }
    let mut dir = Vec::new();
    for l in root..root + root_len.div_ceil(2048) {
        dir.extend_from_slice(&d.read_data(l).ok()?.data);
    }
    let mut at = 0;
    while at < root_len as usize && at < dir.len() {
        let len = dir[at] as usize;
        if len == 0 {
            break;
        }
        let dr = &dir[at..(at + len).min(dir.len())];
        if dr.len() > 0x21 + 12 && dr[0x20] == 12 && &dr[0x21..0x21 + 12] == b"SYSTEM.CNF;1" {
            let mut fb = d.read_data(le32(dr, 2)).ok()?.data;
            fb.truncate(2048);
            let text = &fb[..fb.iter().position(|&b| b == 0).unwrap_or(fb.len())];
            let p = text.windows(4).position(|w| w == b"BOOT")?;
            let mut s = &text[p + 4..];
            let skip_ws = |s: &mut &[u8]| {
                while s.first().is_some_and(|&c| c == b' ' || c == b'\t') {
                    *s = &s[1..];
                }
            };
            skip_ws(&mut s);
            s = s.strip_prefix(b"=")?;
            skip_ws(&mut s);
            if s.len() < 6 || !s[..6].eq_ignore_ascii_case(b"cdrom:") {
                return None;
            }
            s = &s[6..];
            while s.first() == Some(&b'\\') {
                s = &s[1..];
            }
            let end = s.iter().position(|&c| c == b'_' || c == b'.' || c == b';').unwrap_or(s.len());
            let id = &s[..end];
            let up = |i: usize| id[i].to_ascii_uppercase();
            if id.len() == 4 && up(0) == b'S' && matches!(up(1), b'C' | b'L' | b'I') {
                return match up(2) {
                    b'E' => Some(*b"SCEE"),
                    b'U' => Some(*b"SCEA"),
                    b'K' | b'B' | b'P' => Some(*b"SCEI"),
                    _ => None,
                };
            }
            return None;
        }
        at += len;
    }
    None
}

/// Mednafen `CalcRegion_By_SA`: the licence text at the start of sector 4.
fn region_by_sa(sa: &[u8]) -> Option<[u8; 4]> {
    let f: String =
        sa[..0x48].iter().filter(|&&b| b > 0x20 && b < 0x80).map(|&b| (b as char).to_ascii_lowercase()).collect();
    if !f.contains("licensedby") {
        return None;
    }
    if f.contains("america") {
        Some(*b"SCEA")
    } else if f.contains("europe") {
        Some(*b"SCEE")
    } else if f.contains("japan") || f.contains("sonycomputerentertainmentinc.") {
        Some(*b"SCEI")
    } else {
        None
    }
}

/// The console's SCEx string from the BIOS version string's region letter,
/// as psx-oracle sets Mednafen's default region.
pub fn bios_scex(bios: &[u8]) -> [u8; 4] {
    let text = bios.get(0x7FF32..0x7FF32 + 40).unwrap_or_default();
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    match text.starts_with(b"System ROM Version").then(|| text[..end].last()).flatten() {
        Some(b'A') => *b"SCEA",
        Some(b'E') => *b"SCEE",
        _ => *b"SCEI",
    }
}

/// Mednafen's CD-XA ADPCM resampling filter, by phase (of 7).
const CDADPCM_IMPULSE: [[i16; 25]; 7] = [
    [
        0, -5, 17, -35, 70, -23, -68, 347, -839, 2062, -4681, 15367, 21472, -5882, 2810, -1352, 635, -235, 26, 43, -35,
        16, -8, 2, 0,
    ],
    [
        0, -2, 10, -34, 65, -84, 52, 9, -266, 1024, -2680, 9036, 26516, -6016, 3021, -1571, 848, -365, 107, 10, -16,
        17, -8, 3, -1,
    ],
    [
        -2, 0, 3, -19, 60, -75, 162, -227, 306, -67, -615, 3229, 29883, -4532, 2488, -1471, 882, -424, 166, -27, 5, 6,
        -8, 3, -1,
    ],
    [
        -1, 3, -2, -5, 31, -74, 179, -402, 689, -926, 1272, -1446, 31033, -1446, 1272, -926, 689, -402, 179, -74, 31,
        -5, -2, 3, -1,
    ],
    [
        -1, 3, -8, 6, 5, -27, 166, -424, 882, -1471, 2488, -4532, 29883, 3229, -615, -67, 306, -227, 162, -75, 60, -19,
        3, 0, -2,
    ],
    [
        -1, 3, -8, 17, -16, 10, 107, -365, 848, -1571, 3021, -6016, 26516, 9036, -2680, 1024, -266, 9, 52, -84, 65,
        -34, 10, -2, 0,
    ],
    [
        0, 2, -8, 16, -35, 43, 26, -235, 635, -1352, 2810, -5882, 21472, 15367, -4681, 2062, -839, 347, -68, -23, 70,
        -35, 17, -5, 0,
    ],
];

/// Mednafen `DecodeXAADPCM`: 28 samples after two of history in `out[0..2]`.
fn decode_xa_adpcm(input: &[u8; 28], out: &mut [i16; 30], shift: u32, weight: usize) {
    const WEIGHTS: [[i32; 2]; 5] = [[0, 0], [60, 0], [115, -52], [98, -55], [122, -60]];
    let w = WEIGHTS.get(weight).copied().unwrap_or([0, 0]);
    for i in 0..28 {
        let mut s = ((input[i] as i32) << 8) as i16 as i32;
        s >>= shift;
        s += ((out[i + 1] as i32 * w[0]) >> 6) + ((out[i] as i32 * w[1]) >> 6);
        out[i + 2] = s.clamp(-32768, 32767) as i16;
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cdrom {
    /// Not saved: a restored machine gets the disc reinserted by the caller.
    #[serde(skip)]
    pub disc: Option<Disc>,
    /// GetID's SCEx string, or None for a disc that isn't a PlayStation disc.
    disc_id: Option<[u8; 4]>,
    disc_changed: bool,
    disc_startup_delay: i32,

    audio: AudioBuffer,
    /// `[source][output]`: L->L, L->R, R->L, R->R.
    pending_decode_volume: [[u8; 2]; 2],
    decode_volume: [[u8; 2]; 2],
    adpcm_resamp_buf: [Vec<i16>; 2],
    adpcm_resamp_pos: u8,
    adpcm_resamp_phase: u8,

    reg_selector: u8,
    args_buf: [u8; 16],
    args_wp: u8,
    args_rp: u8,
    args_receive_latch: u8,
    args_receive_buf: [u8; 32],
    args_receive_in: u8,

    results_buffer: [u8; 16],
    results_in: u8,
    results_wp: u8,
    results_rp: u8,

    dma_buffer: VecDeque<u8>,
    sb: Vec<u8>,
    sb_in: u32,
    sector_pipe: [Vec<u8>; 2],
    sector_pipe_pos: u8,
    sector_pipe_in: u8,

    subq_buf: [u8; 12],
    subq_buf_safe: [u8; 12],
    subq_checksum_ok: bool,
    header_buf_valid: bool,
    header_buf: [u8; 12],

    irq_buffer: u8,
    irq_out_test_mask: u8,
    ready_receive_counter: i32,

    filter_file: u8,
    filter_chan: u8,

    pending_command: u8,
    pending_command_phase: i32,
    pending_command_counter: i32,

    spu_counter: i32,

    mode: u8,
    drive_status: i32,
    status_after_seek: i32,
    forward: bool,
    backward: bool,
    muted: bool,
    play_track_match: i32,
    psr_counter: i32,
    hold_logical_pos: bool,
    cur_sector: i32,
    sectors_read: u32,

    async_irq_pending: u8,
    async_results_pending: [u8; 16],
    async_results_pending_count: u8,

    seek_target: i32,
    seek_retry_counter: u32,
    seek_finished: i32,

    lastts: u64,

    command_loc: i32,
    command_loc_dirty: bool,
    xa_previous: [[i16; 2]; 2],
    report_last_f: u8,
    report_startup_delay: i32,

    rng: Prng,

    /// LBA of the sector in the sector buffer and in the data FIFO, and the
    /// bytes put in the FIFO (analysis).
    sb_lba: i32,
    fifo_lba: u32,
    fifo_fill: u32,
    /// Time of the update or write in progress (analysis).
    #[serde(skip)]
    now: u64,
    #[serde(skip)]
    frame: Option<u64>,
    /// A command's own responses are being written (for [`Cdrom::recent`]).
    #[serde(skip)]
    in_command: bool,
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
    /// Sectors delivered as INT1 since power-on.
    pub sectors_delivered: u64,
    /// XA sectors decoded since power-on.
    pub xa_sectors: u64,
    /// Commands the controller received that are not implemented.
    #[serde(skip)]
    pub unknown_commands: Vec<u8>,
}

impl Default for Cdrom {
    fn default() -> Self {
        Self::new(None, *b"SCEI")
    }
}

impl Cdrom {
    /// Power on (Mednafen `Power`), then insert `disc` (`SetDisc`); `region`
    /// is the console's SCEx string, for discs whose region isn't detected.
    pub fn new(disc: Option<Disc>, region: [u8; 4]) -> Self {
        let mut c = Cdrom {
            disc: None,
            disc_id: None,
            disc_changed: true,
            disc_startup_delay: 0,
            audio: AudioBuffer::new(),
            pending_decode_volume: [[0x80, 0], [0, 0x80]],
            decode_volume: [[0x80, 0], [0, 0x80]],
            adpcm_resamp_buf: [vec![0; 64], vec![0; 64]],
            adpcm_resamp_pos: 0,
            adpcm_resamp_phase: 0,
            reg_selector: 0,
            args_buf: [0; 16],
            args_wp: 0,
            args_rp: 0,
            args_receive_latch: 0,
            args_receive_buf: [0; 32],
            args_receive_in: 0,
            results_buffer: [0; 16],
            results_in: 0,
            results_wp: 0,
            results_rp: 0,
            dma_buffer: VecDeque::with_capacity(DMA_BUFFER_SIZE),
            sb: vec![0; 2340],
            sb_in: 0,
            sector_pipe: [vec![0; 2352], vec![0; 2352]],
            sector_pipe_pos: 0,
            sector_pipe_in: 0,
            subq_buf: [0; 12],
            subq_buf_safe: [0; 12],
            subq_checksum_ok: false,
            header_buf_valid: false,
            header_buf: [0; 12],
            irq_buffer: 0,
            irq_out_test_mask: 0,
            ready_receive_counter: 0,
            filter_file: 0,
            filter_chan: 0,
            pending_command: 0,
            pending_command_phase: 0,
            pending_command_counter: 0,
            spu_counter: crate::spu::CYCLES_PER_SAMPLE as i32,
            mode: 0x20,
            drive_status: DS_STOPPED,
            status_after_seek: DS_STOPPED,
            forward: false,
            backward: false,
            muted: false,
            play_track_match: 0,
            psr_counter: 0,
            hold_logical_pos: false,
            cur_sector: 0,
            sectors_read: 0,
            async_irq_pending: 0,
            async_results_pending: [0; 16],
            async_results_pending_count: 0,
            seek_target: 0,
            seek_retry_counter: 0,
            seek_finished: -1,
            lastts: 0,
            command_loc: 0,
            command_loc_dirty: true,
            xa_previous: [[0; 2]; 2],
            report_last_f: 0,
            report_startup_delay: 0,
            rng: Prng::default(),
            sb_lba: 0,
            fifo_lba: 0,
            fifo_fill: 0,
            now: 0,
            frame: None,
            in_command: false,
            dma_log: Vec::new(),
            cmd_log: Vec::new(),
            recent: VecDeque::new(),
            irq_log: Vec::new(),
            log_dma: false,
            sectors_delivered: 0,
            xa_sectors: 0,
            unknown_commands: Vec::new(),
        };
        c.soft_reset();
        if let Some(d) = disc {
            c.header_buf_valid = false;
            c.disc_startup_delay = CLOCK as i32;
            c.disc_changed = true;
            c.disc_id = disc_scex(&d, region);
            c.disc = Some(d);
        }
        c
    }

    /// The time the controller has been brought up to.
    pub fn time(&self) -> u64 {
        self.lastts
    }

    /// Start counting from `now` (a drive inserted into a running machine).
    pub fn set_time(&mut self, now: u64) {
        self.lastts = now;
    }

    fn soft_reset(&mut self) {
        self.clear_audio_buffers();
        self.report_last_f = 0;
        self.report_startup_delay = 0;
        self.pending_decode_volume = [[0x80, 0], [0, 0x80]];
        self.decode_volume = self.pending_decode_volume;
        self.reg_selector = 0;
        self.args_buf = [0; 16];
        self.args_wp = 0;
        self.args_rp = 0;
        self.args_receive_latch = 0;
        self.args_receive_buf = [0; 32];
        self.args_receive_in = 0;
        self.results_buffer = [0; 16];
        self.results_wp = 0;
        self.results_rp = 0;
        self.results_in = 0;
        self.ready_receive_counter = 0;
        self.irq_buffer = 0;
        self.irq_out_test_mask = 0;
        self.dma_buffer.clear();
        self.sb_in = 0;
        self.sector_pipe_pos = 0;
        self.sector_pipe_in = 0;
        self.sectors_read = 0;
        self.subq_buf = [0; 12];
        self.subq_buf_safe = [0; 12];
        self.subq_checksum_ok = false;
        self.header_buf = [0; 12];
        self.filter_file = 0;
        self.filter_chan = 0;
        self.pending_command = 0;
        self.pending_command_phase = 0;
        self.pending_command_counter = 0;
        self.mode = 0x20;
        self.header_buf_valid = false;
        self.seek_finished = -1;
        self.drive_status = DS_STOPPED;
        self.clear_aip();
        self.status_after_seek = DS_STOPPED;
        self.seek_retry_counter = 0;
        self.forward = false;
        self.backward = false;
        self.muted = false;
        self.play_track_match = 0;
        self.psr_counter = 0;
        self.cur_sector = 0;
        self.seek_target = 0;
        self.command_loc = 0;
        self.command_loc_dirty = true;
        self.disc_changed = true;
    }

    /// Interrupt output (I_STAT bit 2).
    pub fn irq(&self) -> bool {
        self.irq_buffer & self.irq_out_test_mask & 0x1F != 0
    }

    /// When the controller next needs an update (Mednafen `CalcNextEvent`).
    pub fn next_event(&self) -> u64 {
        let mut next = self.spu_counter;
        if self.psr_counter > 0 && next > self.psr_counter {
            next = self.psr_counter;
        }
        if self.pending_command_counter > 0 && next > self.pending_command_counter {
            next = self.pending_command_counter;
        }
        if self.irq_buffer & 0xF == 0 && self.ready_receive_counter > 0 && next > self.ready_receive_counter {
            next = self.ready_receive_counter;
        }
        if self.disc_startup_delay > 0 && next > self.disc_startup_delay {
            next = self.disc_startup_delay;
        }
        self.lastts + next.max(1) as u64
    }

    fn sector_period(&self) -> i32 {
        (CLOCK / (75 * if self.mode & MODE_SPEED != 0 { 2 } else { 1 })) as i32
    }

    fn write_irq(&mut self, v: u8) {
        self.ready_receive_counter = 2000;
        self.irq_buffer = (self.irq_buffer & 0x10) | v;
        if self.log_dma {
            let first = if self.results_in > 0 { self.results_buffer[self.results_rp as usize] } else { 0 };
            self.irq_log.push((self.now, "deliver", (v as u32) << 8 | first as u32));
        }
        if self.in_command {
            if let Some(r) = self.recent.back_mut() {
                r.ints.push(v);
            }
        }
    }

    fn begin_results(&mut self) {
        self.results_in = 0;
        self.results_wp = 0;
        self.results_rp = 0;
        self.results_buffer = [0; 16];
    }

    fn write_result(&mut self, v: u8) {
        self.results_buffer[self.results_wp as usize] = v;
        self.results_wp = (self.results_wp + 1) & 0xF;
        self.results_in = (self.results_in + 1) & 0x1F;
    }

    fn read_result(&mut self) -> u8 {
        let r = self.results_buffer[self.results_rp as usize];
        self.results_rp = (self.results_rp + 1) & 0xF;
        self.results_in = self.results_in.wrapping_sub(1) & 0x1F;
        r
    }

    fn make_status(&mut self, cmd_error: bool) -> u8 {
        let mut r = 0;
        match self.drive_status {
            DS_PLAYING => r |= 0x80,
            DS_READING => r |= 0x20,
            DS_SEEKING | DS_SEEKING_LOGICAL | DS_SEEKING_LOGICAL2 => r |= 0x40,
            _ => {}
        }
        if self.disc.is_none() || self.disc_changed {
            r |= 0x10;
        }
        if self.drive_status != DS_STOPPED {
            r |= 0x02;
        }
        if cmd_error {
            r |= 0x01;
        }
        self.disc_changed = false;
        r
    }

    /// Mednafen `DecodeSubQ`: keep a position Q, and the last one whose CRC is good.
    fn decode_subq(&mut self, q: &[u8; 12]) -> bool {
        if q[0] & 0xF == 1 {
            self.subq_buf = *q;
            self.subq_checksum_ok = subq_crc_ok(q);
            if self.subq_checksum_ok {
                self.subq_buf_safe = *q;
                return true;
            }
        }
        false
    }

    fn clear_audio_buffers(&mut self) {
        self.audio.size = 0;
        self.audio.freq = 0;
        self.audio.read_pos = 0;
        for s in &mut self.audio.samples {
            s.fill(0);
        }
        self.xa_previous = [[0; 2]; 2];
        for b in &mut self.adpcm_resamp_buf {
            b.fill(0);
        }
        self.adpcm_resamp_phase = 0;
        self.adpcm_resamp_pos = 0;
    }

    fn clear_aip(&mut self) {
        self.async_results_pending_count = 0;
        self.async_irq_pending = 0;
    }

    fn check_aip(&mut self) {
        if self.async_irq_pending != 0 && self.ready_receive_counter <= 0 {
            self.begin_results();
            for i in 0..self.async_results_pending_count as usize {
                let v = self.async_results_pending[i];
                self.write_result(v);
            }
            let irq = self.async_irq_pending;
            self.write_irq(irq);
            self.clear_aip();
        }
    }

    fn set_aip(&mut self, irq: u8, results: &[u8]) {
        self.clear_aip();
        self.async_results_pending_count = results.len() as u8;
        self.async_results_pending[..results.len()].copy_from_slice(results);
        self.async_irq_pending = irq;
        self.check_aip();
    }

    /// One CD audio sample for the SPU (Mednafen `GetCDAudio`).
    fn cd_audio(&mut self) -> (i16, i16) {
        let freq = if self.audio.read_pos < self.audio.size { self.audio.freq } else { 0 };
        if freq == 0 {
            return (0, 0);
        }
        let mut s = [0i32; 2];
        if freq == 7 || freq == 14 {
            s = self.audio.read();
            if freq == 14 {
                s = self.audio.read();
            }
        } else {
            for (i, out) in s.iter_mut().enumerate() {
                let imp = &CDADPCM_IMPULSE[self.adpcm_resamp_phase as usize];
                let base = (self.adpcm_resamp_pos as usize + 32 - 25) & 0x1F;
                let wf = &self.adpcm_resamp_buf[i][base..base + 25];
                let acc: i32 = imp.iter().zip(wf).map(|(&a, &b)| a as i32 * b as i32).sum();
                *out = (acc >> 15).clamp(-32768, 32767);
            }
            self.adpcm_resamp_phase += freq as u8;
            if self.adpcm_resamp_phase >= 7 {
                self.adpcm_resamp_phase -= 7;
                let raw = self.audio.read();
                let p = self.adpcm_resamp_pos as usize;
                for (buf, &v) in self.adpcm_resamp_buf.iter_mut().zip(&raw) {
                    buf[p] = v as i16;
                    buf[p + 32] = v as i16;
                }
                self.adpcm_resamp_pos = (self.adpcm_resamp_pos + 1) & 0x1F;
            }
        }
        // Volume after resampling (ApplyVolume).
        let v = self.decode_volume.map(|r| r.map(|x| x as i32));
        let l = ((s[0] * v[0][0]) >> 7) + ((s[1] * v[1][0]) >> 7);
        let r = ((s[0] * v[0][1]) >> 7) + ((s[1] * v[1][1]) >> 7);
        if self.muted {
            return (0, 0);
        }
        (l.clamp(-32768, 32767) as i16, r.clamp(-32768, 32767) as i16)
    }

    fn xa_test(&self, buf: &[u8]) -> bool {
        let sh = &buf[16..24];
        if self.mode & MODE_STRSND == 0 || sh[2] & 0x04 == 0 {
            return false;
        }
        !(self.mode & MODE_SF != 0 && (sh[0] != self.filter_file || sh[1] != self.filter_chan))
    }

    /// Mednafen `XA_ProcessSector`: decode a whole XA sector into the audio buffer.
    fn xa_process_sector(&mut self, buf: &[u8]) {
        let coding = buf[16 + 3];
        let shift8 = if coding & 0x10 != 0 { 0 } else { 1 };
        let stereo = coding & 0x01 != 0;
        self.audio.read_pos = 0;
        self.audio.size = 18 * (4 << shift8) * 28;
        if stereo {
            self.audio.size >>= 1;
        }
        self.audio.freq = if coding & 0x04 != 0 { 3 } else { 6 };
        for group in 0..18 {
            let sg = &buf[24 + group * 128..24 + (group + 1) * 128];
            for unit in 0..(4usize << shift8) {
                let param = sg[(unit & 3) | ((unit & 4) << 1)];
                let param_copy = sg[4 | (unit & 3) | ((unit & 4) << 1)];
                let mut ib = [0u8; 28];
                for (i, b) in ib.iter_mut().enumerate() {
                    let mut t = sg[16 + i * 4 + (unit >> shift8)];
                    if shift8 != 0 {
                        t = if unit & 1 != 0 { t } else { t << 4 } & 0xF0;
                    }
                    *b = t;
                }
                let ocn = unit & 1 != 0 && stereo;
                let mut ob = [0i16; 30];
                ob[0] = self.xa_previous[ocn as usize][0];
                ob[1] = self.xa_previous[ocn as usize][1];
                decode_xa_adpcm(&ib, &mut ob, (param & 0xF) as u32, (param >> 4) as usize);
                self.xa_previous[ocn as usize] = [ob[28], ob[29]];
                if param != param_copy {
                    ob = [0; 30];
                }
                for s in 0..28 {
                    if stereo {
                        let i = group * (2 << shift8) * 28 + (unit >> 1) * 28 + s;
                        self.audio.samples[ocn as usize][i] = ob[2 + s];
                    } else {
                        let i = group * (4 << shift8) * 28 + unit * 28 + s;
                        self.audio.samples[0][i] = ob[2 + s];
                        self.audio.samples[1][i] = ob[2 + s];
                    }
                }
            }
        }
    }

    fn enbufferize_cdda_sector(&mut self, buf: &[u8]) {
        self.audio.freq = 7 * if self.mode & MODE_SPEED != 0 { 2 } else { 1 };
        self.audio.size = 588;
        let data = self.subq_buf_safe[0] & 0x40 != 0;
        for i in 0..588 {
            let (l, r) = if data {
                (0, 0)
            } else {
                (i16::from_le_bytes([buf[i * 4], buf[i * 4 + 1]]), i16::from_le_bytes([buf[i * 4 + 2], buf[i * 4 + 3]]))
            };
            self.audio.samples[0][i] = l;
            self.audio.samples[1][i] = r;
        }
        self.audio.read_pos = 0;
    }

    fn stop_pipe(&mut self) {
        self.sector_pipe_pos = 0;
        self.sector_pipe_in = 0;
        self.sectors_read = 0;
    }

    /// Mednafen `HandlePlayRead`: the head reads the sector under it.
    fn handle_play_read(&mut self) {
        let Some(d) = self.disc.as_ref() else { return };
        let l = layout(d);
        if self.cur_sector >= l.leadout + 300 && self.cur_sector >= 75 * 60 * 75 - 150 {
            self.seek_finished = -1;
            self.drive_status = DS_STOPPED;
            self.stop_pipe();
            return;
        }
        let (read_buf, q) = read_raw_sector(d, &l, self.cur_sector);
        self.decode_subq(&q);

        let safe = self.subq_buf_safe;
        if safe[1] == 0xAA
            && (self.drive_status == DS_PLAYING
                || (self.drive_status == DS_READING && safe[0] & 0x40 == 0 && self.mode & MODE_CDDA != 0))
        {
            self.header_buf_valid = false;
            self.seek_target = self.cur_sector;
            self.drive_status = DS_PAUSED;
            self.stop_pipe();
            let s = self.make_status(false);
            self.set_aip(IRQ_DATA_END, &[s]);
            return;
        }

        if self.drive_status == DS_PLAYING {
            if self.mode & MODE_AUTOPAUSE != 0 {
                if self.play_track_match == -1 && self.subq_checksum_ok {
                    self.play_track_match = safe[1] as i32;
                }
                if self.play_track_match != -1 && safe[1] as i32 != self.play_track_match {
                    let s = self.make_status(false);
                    self.set_aip(IRQ_DATA_END, &[s]);
                    self.seek_target = self.cur_sector;
                    self.drive_status = DS_PAUSED;
                    self.stop_pipe();
                    self.psr_counter = 0;
                    return;
                }
            }
            if self.mode & MODE_REPORT != 0
                && self.report_startup_delay <= 0
                && ((safe[9] >> 4) != self.report_last_f || self.forward || self.backward)
                && self.subq_checksum_ok
            {
                let chselect = (safe[8] & 1) as usize;
                let mut level: u16 = 0;
                for i in 0..588 {
                    let o = i * 4 + chselect * 2;
                    let v = i16::from_le_bytes([read_buf[o], read_buf[o + 1]]) as i32;
                    level = level.max(v.abs().min(32767) as u16);
                }
                level |= (chselect as u16) << 15;
                let mut tr = [0u8; 8];
                tr[0] = self.make_status(false);
                tr[1] = safe[1];
                tr[2] = safe[2];
                if safe[9] & 0x10 != 0 {
                    tr[3] = safe[3];
                    tr[4] = safe[4] | 0x80;
                    tr[5] = safe[5];
                } else {
                    tr[3] = safe[7];
                    tr[4] = safe[8];
                    tr[5] = safe[9];
                }
                tr[6] = level as u8;
                tr[7] = (level >> 8) as u8;
                self.set_aip(IRQ_DATA_READY, &tr);
            }
            self.report_last_f = safe[9] >> 4;
        }

        if self.sector_pipe_in >= 2 {
            let pos = self.sector_pipe_pos as usize;
            let buf = std::mem::take(&mut self.sector_pipe[pos]);
            self.sector_pipe_in -= 1;
            self.pipe_sector(&buf, safe);
            self.sector_pipe[pos] = buf;
        }

        let pos = self.sector_pipe_pos as usize;
        self.sector_pipe[pos].copy_from_slice(&read_buf);
        self.sector_pipe_pos = ((pos + 1) % 2) as u8;
        self.sector_pipe_in += 1;

        self.psr_counter += self.sector_period();

        if self.drive_status == DS_PLAYING {
            if self.forward {
                self.cur_sector += 12;
            } else if self.backward {
                self.cur_sector = (self.cur_sector - 12).max(0);
            } else {
                self.cur_sector += 1;
            }
        } else {
            self.cur_sector += 1;
        }

        if self.drive_status == DS_PAUSED || self.drive_status == DS_STANDBY {
            if self.cur_sector >= self.seek_target + if self.hold_logical_pos { 2 } else { 0 } {
                self.cur_sector = (self.cur_sector - 9).max(-150);
            }
        } else {
            self.sectors_read += 1;
        }
    }

    /// The sector leaving the pipeline; `safe` is the Q of the sector just read.
    fn pipe_sector(&mut self, buf: &[u8], safe: [u8; 12]) {
        if safe[0] & 0x40 != 0 {
            let reading = self.drive_status == DS_READING;
            if self.drive_status == DS_SEEKING_LOGICAL2
                || reading
                || (self.hold_logical_pos && (self.drive_status == DS_PAUSED || self.drive_status == DS_STANDBY))
            {
                self.header_buf.copy_from_slice(&buf[12..24]);
                self.header_buf_valid = true;
                if self.drive_status == DS_SEEKING_LOGICAL2 {
                    let h = self.header_buf;
                    let lba = (unbcd(h[0]) * 60 + unbcd(h[1])) * 75 + unbcd(h[2]) - 150;
                    if lba == self.seek_target {
                        self.drive_status = self.status_after_seek;
                        self.seek_finished = 1;
                        self.report_startup_delay = 24_000_000;
                    } else if self.seek_retry_counter == 0 {
                        self.hold_logical_pos = false;
                        self.drive_status = DS_STANDBY;
                        self.seek_finished = -1;
                    } else {
                        self.seek_retry_counter -= 1;
                    }
                }
            }
            if self.drive_status == DS_READING {
                let lba = (unbcd(buf[12]) * 60 + unbcd(buf[13])) * 75 + unbcd(buf[14]) - 150;
                if self.mode & MODE_STRSND != 0 && buf[12 + 3] == 2 && buf[12 + 6] & 0x64 == 0x64 {
                    if self.xa_test(buf) {
                        let wanted = self.audio.read_pos >= self.audio.size;
                        if wanted {
                            self.xa_process_sector(buf);
                            self.xa_sectors += 1;
                        }
                        if self.log_dma {
                            self.irq_log.push((self.now, if wanted { "xa" } else { "xa-skipped" }, lba as u32));
                        }
                    } else if self.log_dma {
                        self.irq_log.push((self.now, "xa-filtered", lba as u32));
                    }
                } else {
                    let (mut offs, mut size) = if self.mode & 0x20 != 0 { (0, 2340) } else { (12, 2048) };
                    if self.mode & 0x10 != 0 {
                        offs = 12;
                        size = 2328;
                    }
                    self.sb[..size].copy_from_slice(&buf[12 + offs..12 + offs + size]);
                    self.sb_in = size as u32;
                    self.sb_lba = lba;
                    self.sectors_delivered += 1;
                    if self.log_dma {
                        self.irq_log.push((self.now, "sector", lba as u32));
                    }
                    let s = self.make_status(false);
                    self.set_aip(IRQ_DATA_READY, &[s]);
                }
            }
        } else {
            if self.drive_status == DS_SEEKING_LOGICAL2 {
                if self.mode & MODE_CDDA != 0 {
                    self.drive_status = self.status_after_seek;
                    self.seek_finished = 1;
                    self.report_startup_delay = 24_000_000;
                } else if self.seek_retry_counter == 0 {
                    self.hold_logical_pos = false;
                    self.drive_status = DS_STANDBY;
                    self.seek_finished = -1;
                } else {
                    self.seek_retry_counter -= 1;
                }
            }
            if ((self.drive_status == DS_READING && self.mode & MODE_CDDA != 0) || self.drive_status == DS_PLAYING)
                && self.audio.read_pos >= self.audio.size
            {
                self.enbufferize_cdda_sector(buf);
            }
        }
    }

    /// Bring the controller (and the SPU, which it clocks) to `t`
    /// (Mednafen `Update`); returns true when the SPU interrupt fired.
    pub fn update(&mut self, t: u64, spu: &mut Spu) -> bool {
        let mut clocks = t.saturating_sub(self.lastts) as i64;
        let mut spu_irq = false;
        self.now = self.lastts;

        if self.disc.is_none() {
            if self.drive_status != DS_STOPPED || (self.pending_command_counter > 0 && self.pending_command_phase >= 2)
            {
                let s = (self.make_status(false) & !0xE0) | 0x04;
                self.set_aip(IRQ_DISC_ERROR, &[s, 0x08]);
                self.stop_pipe();
                self.header_buf_valid = false;
                self.seek_finished = -1;
                self.drive_status = DS_STOPPED;
                self.psr_counter = 0;
                if self.pending_command_phase >= 2 {
                    self.pending_command = 0;
                    self.pending_command_counter = 0;
                    self.pending_command_phase = 0;
                }
            }
            self.disc_startup_delay = 0;
        }

        while clocks > 0 {
            let mut chunk = clocks.min(i32::MAX as i64) as i32;
            if self.psr_counter > 0 && chunk > self.psr_counter {
                chunk = self.psr_counter;
            }
            if self.pending_command_counter > 0 && chunk > self.pending_command_counter {
                chunk = self.pending_command_counter;
            }
            if chunk > self.spu_counter {
                chunk = self.spu_counter;
            }
            if self.disc_startup_delay > 0 {
                if chunk > self.disc_startup_delay {
                    chunk = self.disc_startup_delay;
                }
                self.disc_startup_delay -= chunk;
                if self.disc_startup_delay <= 0 {
                    self.seek_target = self.cur_sector;
                    self.hold_logical_pos = false;
                    self.drive_status = DS_PAUSED;
                }
            }
            if self.irq_buffer & 0xF == 0 {
                if self.ready_receive_counter > 0 && chunk > self.ready_receive_counter {
                    chunk = self.ready_receive_counter;
                }
                if self.ready_receive_counter > 0 {
                    self.ready_receive_counter -= chunk;
                }
            }
            self.now += chunk as u64;

            self.check_aip();

            if self.psr_counter > 0 {
                self.psr_counter -= chunk;
                if self.report_startup_delay > 0 {
                    self.report_startup_delay -= chunk;
                }
                if self.psr_counter <= 0 {
                    if self.drive_status == DS_SEEKING {
                        self.cur_sector = self.seek_target;
                        self.hold_logical_pos = false;
                        self.drive_status = self.status_after_seek;
                        self.seek_finished = 1;
                        self.report_startup_delay = 24_000_000;
                        if self.drive_status == DS_PAUSED || self.drive_status == DS_STANDBY {
                            self.cur_sector = (self.cur_sector - 9).max(-150);
                        }
                        self.psr_counter = self.sector_period();
                    } else if self.drive_status == DS_SEEKING_LOGICAL {
                        self.cur_sector = self.seek_target;
                        self.hold_logical_pos = true;
                        self.drive_status = DS_SEEKING_LOGICAL2;
                    }
                    if matches!(
                        self.drive_status,
                        DS_PAUSED | DS_STANDBY | DS_READING | DS_PLAYING | DS_SEEKING_LOGICAL2
                    ) {
                        self.handle_play_read();
                    }
                }
            }

            if self.pending_command_counter > 0 {
                self.pending_command_counter -= chunk;
                if self.pending_command_counter <= 0 && self.ready_receive_counter > 0 {
                    self.pending_command_counter = self.ready_receive_counter;
                } else if self.pending_command_counter <= 0 {
                    let next = self.command_phase();
                    if next == 0 {
                        self.pending_command_counter = 0;
                        self.fold_recent();
                    } else {
                        self.pending_command_counter += next;
                    }
                }
            }

            spu_irq |= spu.tick_with(chunk as u64, || self.cd_audio());
            self.spu_counter = spu.clocks_to_sample() as i32;
            clocks -= chunk as i64;
        }
        self.lastts = t;
        spu_irq
    }

    /// The pending command's next phase; returns the time to the one after.
    fn command_phase(&mut self) -> i32 {
        match self.pending_command_phase {
            -1 | 0 => {
                if self.pending_command_phase == 0 && self.args_receive_in < 32 {
                    self.args_receive_buf[self.args_receive_in as usize] = self.args_receive_latch;
                    self.args_receive_in += 1;
                }
                if self.args_rp != self.args_wp {
                    self.args_receive_latch = self.args_buf[(self.args_rp & 0xF) as usize];
                    self.args_rp = (self.args_rp + 1) & 0x1F;
                    if self.pending_command_phase == -1 {
                        self.pending_command_phase = 0;
                    }
                    1815
                } else {
                    self.pending_command_phase += if self.pending_command_phase == -1 { 2 } else { 1 };
                    8500
                }
            }
            1 => {
                let cmd = self.pending_command;
                let n = self.args_receive_in;
                self.in_command = true;
                self.begin_results();
                let next = match command_args(cmd) {
                    None => {
                        self.unknown_commands.push(cmd);
                        let s = self.make_status(true);
                        self.write_result(s);
                        self.write_result(ERR_BAD_COMMAND);
                        self.write_irq(IRQ_DISC_ERROR);
                        0
                    }
                    Some((min, max)) if n < min || n > max => {
                        let s = self.make_status(true);
                        self.write_result(s);
                        self.write_result(ERR_BAD_NUMARGS);
                        self.write_irq(IRQ_DISC_ERROR);
                        0
                    }
                    Some(_) => {
                        let args = self.args_receive_buf;
                        let next = self.run_command(cmd, n as usize, &args);
                        self.pending_command_phase = 2;
                        next
                    }
                };
                self.args_receive_in = 0;
                self.in_command = false;
                next
            }
            _ => {
                self.in_command = true;
                self.begin_results();
                let next = self.run_command_part2(self.pending_command);
                self.in_command = false;
                next
            }
        }
    }

    pub fn read(&mut self, reg: u32) -> u8 {
        match reg & 3 {
            0 => {
                let mut r = self.reg_selector & 3;
                if self.args_wp == self.args_rp {
                    r |= 0x08;
                }
                if self.args_wp.wrapping_sub(self.args_rp) & 0x10 == 0 {
                    r |= 0x10;
                }
                if self.results_in != 0 {
                    r |= 0x20;
                }
                if !self.dma_buffer.is_empty() {
                    r |= 0x40;
                }
                if self.pending_command_counter > 0 && self.pending_command_phase <= 1 {
                    r |= 0x80;
                }
                r
            }
            1 => self.read_result(),
            2 => self.read_data_byte(),
            _ => {
                if self.reg_selector & 1 != 0 {
                    0xE0 | self.irq_buffer
                } else {
                    0xFF
                }
            }
        }
    }

    /// A register write at `now` (Mednafen `Write`: other than the index
    /// register, the controller is brought up to date first). Returns true
    /// when the SPU interrupt fired during that update.
    pub fn write(&mut self, reg: u32, v: u8, now: u64, frame: Option<u64>, spu: &mut Spu) -> bool {
        let a = reg & 3;
        if a == 0 {
            self.reg_selector = v & 3;
            return false;
        }
        let spu_irq = self.update(now, spu);
        self.now = now;
        self.frame = frame;
        match (self.reg_selector & 3) as u32 * 3 + (a - 1) {
            0x00 => {
                self.note_command(v, now, frame);
                self.pending_command_counter = 10500 + self.rng.range(0, 3000) as i32 + 1815;
                self.pending_command = v;
                self.pending_command_phase = -1;
                self.args_receive_in = 0;
            }
            0x01 => {
                self.args_buf[(self.args_wp & 0xF) as usize] = v;
                self.args_wp = (self.args_wp + 1) & 0x1F;
            }
            0x02 => {
                if v & 0x80 != 0 {
                    if self.dma_buffer.is_empty() {
                        if self.sb_in == 0 {
                            self.dma_buffer.extend(&self.sb[..2340]);
                            self.dma_buffer.resize(DMA_BUFFER_SIZE, 0);
                        } else {
                            self.dma_buffer.extend(&self.sb[..self.sb_in as usize]);
                            self.sb_in = 0;
                        }
                        self.fifo_lba = self.sb_lba.max(0) as u32;
                        self.fifo_fill = self.dma_buffer.len() as u32;
                    }
                    if self.log_dma {
                        self.irq_log.push((now, "want-data", self.fifo_lba));
                    }
                } else if v & 0x40 != 0 {
                    for _ in 0..4 {
                        self.dma_buffer.pop_front();
                    }
                } else {
                    self.dma_buffer.clear();
                }
                if v & 0x20 != 0 {
                    self.irq_buffer |= 0x10;
                }
            }
            0x04 => self.irq_out_test_mask = v,
            0x05 => {
                if self.log_dma {
                    self.irq_log.push((now, "ack", v as u32));
                }
                self.irq_buffer &= !v;
                if v & 0x80 != 0 {
                    self.soft_reset();
                }
                if v & 0x40 != 0 {
                    self.args_wp = 0;
                    self.args_rp = 0;
                }
            }
            0x07 => self.pending_decode_volume[0][0] = v,
            0x08 => self.pending_decode_volume[0][1] = v,
            0x09 => self.pending_decode_volume[1][1] = v,
            0x0A => self.pending_decode_volume[1][0] = v,
            0x0B if v & 0x20 != 0 => self.decode_volume = self.pending_decode_volume,
            _ => {} // sound map data out and coding info (XA from RAM): Mednafen ignores them too
        }
        spu_irq
    }

    /// [`Cdrom::recent`] and `cmd_log` for a command written now.
    fn note_command(&mut self, cmd: u8, now: u64, frame: Option<u64>) {
        let mut params = Vec::new();
        let mut rp = self.args_rp;
        while rp != self.args_wp {
            params.push(self.args_buf[(rp & 0xF) as usize]);
            rp = (rp + 1) & 0x1F;
        }
        if self.log_dma {
            self.cmd_log.push((now, cmd, params.clone()));
        }
        self.fold_recent();
        if self.recent.len() >= 64 {
            self.recent.pop_front();
        }
        self.recent.push_back(RecentCommand {
            time: now,
            frame,
            last_time: now,
            last_frame: frame,
            command: cmd,
            params,
            ints: Vec::new(),
            repeats: 1,
        });
    }

    /// Fold the last command into the one before it when they match.
    fn fold_recent(&mut self) {
        let n = self.recent.len();
        if n >= 2 {
            let (a, b) = (&self.recent[n - 2], &self.recent[n - 1]);
            if a.command == b.command && a.params == b.params && a.ints == b.ints {
                let b = self.recent.pop_back().unwrap();
                let a = self.recent.back_mut().unwrap();
                a.repeats += b.repeats;
                a.last_time = b.last_time;
                a.last_frame = b.last_frame;
            }
        }
    }

    fn read_data_byte(&mut self) -> u8 {
        self.dma_buffer.pop_front().unwrap_or(0)
    }

    /// Record one DMA3 transfer of `bytes` starting at the current FIFO position.
    pub fn note_dma(&mut self, bytes: u32) {
        if self.log_dma {
            let (lba, off, _) = self.fifo_state();
            self.dma_log.push((lba, off, bytes.min(self.fifo_remaining())));
        }
    }

    /// (LBA in the FIFO, current FIFO offset, sector payload size).
    pub fn fifo_state(&self) -> (u32, u32, u32) {
        let size = if self.mode & 0x10 != 0 {
            2328
        } else if self.mode & 0x20 != 0 {
            2340
        } else {
            2048
        };
        (self.fifo_lba, self.fifo_fill.saturating_sub(self.dma_buffer.len() as u32), size)
    }

    /// Bytes left in the data FIFO.
    pub fn fifo_remaining(&self) -> u32 {
        self.dma_buffer.len() as u32
    }

    /// DMA channel 3: next word from the data FIFO.
    pub fn dma_word(&mut self) -> u32 {
        let mut w = 0;
        for i in 0..4 {
            if let Some(b) = self.dma_buffer.pop_front() {
                w |= (b as u32) << (8 * i);
            }
        }
        w
    }

    fn check_disc_present(&mut self) -> bool {
        if self.disc.is_none() || self.disc_startup_delay > 0 {
            let s = self.make_status(true);
            self.write_result(s);
            self.write_result(ERR_NOT_READY);
            self.write_irq(IRQ_DISC_ERROR);
            return false;
        }
        true
    }

    fn ack(&mut self) {
        let s = self.make_status(false);
        self.write_result(s);
        self.write_irq(IRQ_ACKNOWLEDGE);
    }

    fn error(&mut self, code: u8) {
        let s = self.make_status(true);
        self.write_result(s);
        self.write_result(code);
        self.write_irq(IRQ_DISC_ERROR);
    }

    fn complete(&mut self) {
        let s = self.make_status(false);
        self.write_result(s);
        self.write_irq(IRQ_COMPLETE);
    }

    /// Mednafen `CalcSeekTime`.
    fn seek_time(&mut self, mut initial: i32, target: i32, motor_on: bool, paused: bool) -> i32 {
        let mut ret: i64 = 0;
        if !motor_on {
            initial = 0;
            ret += CLOCK;
        }
        let diff = (initial - target).abs() as i64;
        ret += (diff * CLOCK * 1000 / (72 * 60 * 75) / 1000).max(20000);
        let speed = if self.mode & MODE_SPEED != 0 { 1 } else { 2 };
        if diff >= 2250 {
            ret += CLOCK * 300 / 1000;
        } else if paused {
            ret += 1_237_952 * speed;
        } else if (3..12).contains(&diff) {
            ret += self.sector_period() as i64 * 4;
        }
        ret += self.rng.range(0, 25000) as i64;
        ret as i32
    }

    fn seek_time_now(&mut self, target: i32) -> i32 {
        let (cur, ds) = (self.cur_sector, self.drive_status);
        self.seek_time(cur, target, ds != DS_STOPPED, ds == DS_PAUSED)
    }

    /// Mednafen `PreSeekHack`: the head is put at the target at once, with its Q.
    fn pre_seek_hack(&mut self, target: i32) {
        self.cur_sector = target;
        self.seek_retry_counter = 128;
        let Some(d) = self.disc.take() else { return };
        let l = layout(&d);
        for lba in (target..).take(32) {
            let q = match l.track_index(lba) {
                _ if lba >= l.leadout => leadout_subq(&l, lba),
                Some(ti) => make_subq(&d, &l, ti, lba),
                None => [0; 12],
            };
            if self.decode_subq(&q) {
                break;
            }
        }
        self.disc = Some(d);
    }

    fn start_seek(&mut self, psr: i32, status: i32, after: i32) {
        self.psr_counter = psr;
        self.header_buf_valid = false;
        let target = self.seek_target;
        self.pre_seek_hack(target);
        self.seek_finished = 0;
        self.drive_status = status;
        self.status_after_seek = after;
    }

    fn read_base(&mut self) {
        if !self.check_disc_present() {
            return;
        }
        if self.disc_id.is_none() {
            self.error(ERR_BAD_COMMAND);
            return;
        }
        self.ack();
        if (self.drive_status == DS_SEEKING_LOGICAL || self.drive_status == DS_SEEKING_LOGICAL2)
            && self.seek_target == self.command_loc
            && self.status_after_seek == DS_READING
        {
            self.command_loc_dirty = false;
            return;
        }
        if self.command_loc_dirty || self.drive_status != DS_READING {
            // The data FIFO is kept (Mednafen: CTR's course selection).
            self.clear_aip();
            self.clear_audio_buffers();
            self.sb_in = 0;
            self.stop_pipe();
            if self.command_loc_dirty {
                self.seek_target = self.command_loc;
            } else if self.drive_status != DS_PAUSED && self.drive_status != DS_STANDBY {
                self.seek_target = self.cur_sector;
            }
            let target = self.seek_target;
            let psr = self.sector_period() + self.seek_time_now(target);
            self.start_seek(psr, DS_SEEKING_LOGICAL, DS_READING);
        }
        self.command_loc_dirty = false;
    }

    fn run_command(&mut self, cmd: u8, n: usize, args: &[u8; 32]) -> i32 {
        match cmd {
            0x01 => {
                self.ack();
                0
            }
            0x02 => {
                let bad = |v: u8, max: u8| v & 0xF > 9 || v > max;
                if bad(args[0], 0x99) || bad(args[1], 0x59) || bad(args[2], 0x74) {
                    self.error(ERR_BAD_ARGVAL);
                    return 0;
                }
                self.command_loc = unbcd(args[2]) + 75 * unbcd(args[1]) + 75 * 60 * unbcd(args[0]) - 150;
                self.command_loc_dirty = true;
                self.ack();
                0
            }
            0x03 => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.clear_aip();
                self.ack();
                self.forward = false;
                self.backward = false;
                if n > 0 && args[0] != 0 {
                    let l = layout(self.disc.as_ref().unwrap());
                    let track = unbcd(args[0]).clamp(l.first_track as i32, l.last_track as i32);
                    self.clear_audio_buffers();
                    self.stop_pipe();
                    self.play_track_match = track;
                    self.seek_target = l.track_lba(track);
                    let target = self.seek_target;
                    let psr = self.seek_time_now(target);
                    self.start_seek(psr, DS_SEEKING, DS_PLAYING);
                } else if self.command_loc_dirty || self.drive_status != DS_PLAYING {
                    self.clear_audio_buffers();
                    self.stop_pipe();
                    self.seek_target = if self.command_loc_dirty { self.command_loc } else { self.cur_sector };
                    self.play_track_match = -1;
                    let target = self.seek_target;
                    let psr = self.seek_time_now(target);
                    self.start_seek(psr, DS_SEEKING, DS_PLAYING);
                }
                self.command_loc_dirty = false;
                0
            }
            0x04 | 0x05 => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.ack();
                self.forward = cmd == 0x04;
                self.backward = cmd == 0x05;
                0
            }
            0x06 | 0x1B => {
                self.read_base();
                0
            }
            0x07 => {
                if !self.check_disc_present() {
                    return 0;
                }
                if self.drive_status != DS_STOPPED {
                    self.error(0x20);
                    return 0;
                }
                self.ack();
                self.clear_audio_buffers();
                self.clear_aip();
                self.stop_pipe();
                self.seek_target = self.cur_sector;
                self.hold_logical_pos = false;
                self.drive_status = DS_STANDBY;
                (CLOCK * 100 / 1000) as i32
            }
            0x08 => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.ack();
                if self.drive_status == DS_STOPPED {
                    5000
                } else {
                    self.clear_audio_buffers();
                    self.clear_aip();
                    self.stop_pipe();
                    self.drive_status = DS_STOPPED;
                    self.header_buf_valid = false;
                    33868
                }
            }
            0x09 => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.ack();
                if self.drive_status == DS_PAUSED || self.drive_status == DS_STOPPED {
                    5000
                } else {
                    self.cur_sector -= self.sectors_read.min(4) as i32;
                    self.sectors_read = 0;
                    self.sector_pipe_pos = 0;
                    self.sector_pipe_in = 0;
                    self.clear_aip();
                    self.seek_target = self.cur_sector;
                    self.drive_status = DS_PAUSED;
                    self.psr_counter = self.sector_period();
                    let speed = if self.mode & MODE_SPEED != 0 { 1 } else { 2 };
                    ((1_124_584 + self.cur_sector as i64 * 42596 / (75 * 60)) * speed
                        + self.rng.range(0, 100_000) as i64) as i32
                }
            }
            0x0A => {
                self.ack();
                self.muted = false;
                self.mode = 0x20;
                self.command_loc = 0;
                if self.disc.is_some() && self.disc_startup_delay <= 0 {
                    if (self.drive_status == DS_SEEKING_LOGICAL || self.drive_status == DS_SEEKING)
                        && self.status_after_seek == DS_PAUSED
                        && self.seek_target == 0
                    {
                        256
                    } else {
                        self.clear_audio_buffers();
                        self.sb_in = 0;
                        self.stop_pipe();
                        self.seek_target = 0;
                        // GCC evaluates std::max's seek time before the random delay.
                        let seek = self.seek_time_now(0);
                        self.psr_counter = (self.rng.range(0, 3_250_000) as i32).max(seek);
                        self.pre_seek_hack(0);
                        self.seek_finished = 0;
                        self.drive_status = if self.hold_logical_pos { DS_SEEKING_LOGICAL } else { DS_SEEKING };
                        self.status_after_seek = DS_PAUSED;
                        self.clear_aip();
                        4_100_000
                    }
                } else {
                    self.seek_finished = 1;
                    70000
                }
            }
            0x0B | 0x0C => {
                self.muted = cmd == 0x0B;
                self.ack();
                0
            }
            0x0D => {
                self.filter_file = args[0];
                self.filter_chan = args[1];
                self.ack();
                0
            }
            0x0E => {
                self.mode = args[0];
                self.ack();
                0
            }
            0x0F => {
                let s = self.make_status(false);
                for v in [s, self.mode, 0, self.filter_file, self.filter_chan] {
                    self.write_result(v);
                }
                self.write_irq(IRQ_ACKNOWLEDGE);
                0
            }
            0x10 => {
                if !self.check_disc_present() {
                    return 0;
                }
                if !self.header_buf_valid {
                    self.error(0x80);
                    return 0;
                }
                for i in 0..8 {
                    let v = self.header_buf[i];
                    self.write_result(v);
                }
                self.write_irq(IRQ_ACKNOWLEDGE);
                0
            }
            0x11 => {
                if !self.check_disc_present() {
                    return 0;
                }
                let q = self.subq_buf_safe;
                for i in [1, 2, 3, 4, 5, 7, 8, 9] {
                    self.write_result(q[i]);
                }
                self.write_irq(IRQ_ACKNOWLEDGE);
                0
            }
            0x12 => {
                self.ack();
                44100 * 768 / 1000
            }
            0x13 => {
                if !self.check_disc_present() {
                    return 0;
                }
                let l = layout(self.disc.as_ref().unwrap());
                let s = self.make_status(false);
                self.write_result(s);
                self.write_result(bcd(l.first_track as u32));
                self.write_result(bcd(l.last_track as u32));
                self.write_irq(IRQ_ACKNOWLEDGE);
                0
            }
            0x14 => {
                if !self.check_disc_present() {
                    return 0;
                }
                let l = layout(self.disc.as_ref().unwrap());
                let track = if args[0] == 0 {
                    100
                } else {
                    let t = unbcd(args[0]);
                    if !bcd_valid(args[0]) || t < l.first_track as i32 || t > l.last_track as i32 {
                        self.error(ERR_BAD_ARGVAL);
                        return 0;
                    }
                    t
                };
                let m = aba_msf((l.track_lba(track) + 150) as u32);
                let s = self.make_status(false);
                self.write_result(s);
                self.write_result(m[0]);
                self.write_result(m[1]);
                self.write_irq(IRQ_ACKNOWLEDGE);
                0
            }
            0x15 => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.ack();
                self.seek_target = self.command_loc;
                self.stop_pipe();
                let target = self.seek_target;
                self.psr_counter = self.seek_time_now(target);
                self.pre_seek_hack(target);
                self.seek_finished = 0;
                self.drive_status = DS_SEEKING_LOGICAL;
                self.status_after_seek = DS_STANDBY;
                self.clear_aip();
                self.psr_counter
            }
            0x16 => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.ack();
                self.seek_target = self.command_loc;
                let target = self.seek_target;
                let psr = self.seek_time_now(target);
                self.start_seek(psr, DS_SEEKING, DS_STANDBY);
                self.clear_aip();
                self.psr_counter
            }
            0x19 => {
                self.test_command(args[0]);
                0
            }
            0x1A => {
                if !self.check_disc_present() {
                    return 0;
                }
                self.ack();
                33868
            }
            0x1C => 0,
            0x1D => {
                self.ack();
                0
            }
            0x1E => {
                self.header_buf_valid = false;
                self.ack();
                if !self.check_disc_present() {
                    return 26000;
                }
                let t = self.seek_time_now(0);
                self.seek_target = 0;
                self.hold_logical_pos = false;
                self.drive_status = DS_PAUSED;
                self.clear_aip();
                30_000_000 + t
            }
            _ => 0,
        }
    }

    fn run_command_part2(&mut self, cmd: u8) -> i32 {
        match cmd {
            0x07 | 0x08 => {
                self.psr_counter = 0;
                self.complete();
                0
            }
            0x09 | 0x12 | 0x1E => {
                self.complete();
                0
            }
            0x0A | 0x15 | 0x16 => {
                // Mednafen `Command_Seek_PartN`.
                if matches!(self.drive_status, DS_SEEKING | DS_SEEKING_LOGICAL | DS_SEEKING_LOGICAL2) {
                    return self.psr_counter.max(256);
                }
                if self.seek_finished != 0 {
                    self.begin_results();
                    if self.seek_finished < 0 {
                        let s = self.make_status(false) | 0x04;
                        self.write_result(s);
                        self.write_result(0x04);
                        self.write_irq(IRQ_DISC_ERROR);
                    } else {
                        self.complete();
                    }
                }
                0
            }
            0x1A => {
                let s = self.make_status(false);
                match self.disc_id {
                    Some(id) => {
                        for v in [s, 0x00, 0x20, 0x00] {
                            self.write_result(v);
                        }
                        for v in id {
                            self.write_result(v);
                        }
                        self.write_irq(IRQ_COMPLETE);
                    }
                    None => {
                        let dt = self.disc.as_ref().map_or(0, |d| layout(d).disc_type);
                        for v in [s | 0x08, 0x90, dt, 0x00, 0xFF, 0, 0, 0] {
                            self.write_result(v);
                        }
                        self.write_irq(IRQ_DISC_ERROR);
                    }
                }
                0
            }
            _ => 0,
        }
    }

    fn test_command(&mut self, sub: u8) {
        let results: &[u8] = match sub {
            0x00..=0x03 | 0x10..=0x1A | 0x04 => {
                self.ack();
                return;
            }
            0x51 => {
                for v in [0x01, 0x00, 0x00] {
                    self.write_result(v);
                }
                return;
            }
            0x75 => {
                for v in [0x00, 0xC0, 0x00, 0x00] {
                    self.write_result(v);
                }
                return;
            }
            0x05 => &[0x00, 0x00],
            0x20 => &[0x97, 0x01, 0x10, 0xC2],
            0x21 => &[0x01],
            0x22 => &[0x66, 0x6f, 0x72, 0x20, 0x55, 0x2f, 0x43],
            0x23 | 0x24 => &[0x43, 0x58, 0x44, 0x32, 0x35, 0x34, 0x35, 0x51],
            0x25 => &[0x43, 0x58, 0x44, 0x31, 0x38, 0x31, 0x35, 0x51],
            _ => {
                self.error(0x10);
                return;
            }
        };
        for &v in results {
            self.write_result(v);
        }
        self.write_irq(IRQ_ACKNOWLEDGE);
    }
}

/// Parameter counts (min, max) of the commands Mednafen implements.
fn command_args(cmd: u8) -> Option<(u8, u8)> {
    Some(match cmd {
        0x01 | 0x04..=0x0C | 0x0F..=0x11 | 0x13 | 0x15 | 0x16 | 0x1A..=0x1C | 0x1E => (0, 0),
        0x02 => (3, 3),
        0x03 => (0, 1),
        0x0D | 0x1D => (2, 2),
        0x0E | 0x12 | 0x14 | 0x19 => (1, 1),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the controller (with a throwaway SPU) until `t`.
    fn run(c: &mut Cdrom, spu: &mut Spu, t: u64) {
        while c.next_event() <= t {
            let e = c.next_event();
            c.update(e, spu);
        }
    }

    /// Issue `cmd` at `now` and run until its first interrupt; returns (INT, time, results).
    fn exchange(c: &mut Cdrom, spu: &mut Spu, now: u64, cmd: u8) -> (u8, u64, Vec<u8>) {
        c.write(0, 1, now, None, spu);
        c.write(2, 0x1F, now, None, spu);
        c.write(0, 0, now, None, spu);
        c.write(1, cmd, now, None, spu);
        let mut t = now;
        while !c.irq() {
            t = c.next_event();
            c.update(t, spu);
        }
        c.write(0, 1, t, None, spu);
        let int = c.read(3) & 7;
        let mut bytes = Vec::new();
        while c.read(0) & 0x20 != 0 {
            bytes.push(c.read(1));
        }
        c.write(3, 0x1F, t, None, spu);
        (int, t, bytes)
    }

    #[test]
    fn prng_matches_mednafen() {
        // The first values of Mednafen's MDFN_PseudoRNG after ResetState().
        let mut r = Prng::default();
        let a = r.next();
        let b = r.next();
        assert_ne!(a, b);
        let mut r2 = Prng::default();
        assert_eq!(r2.next(), a);
        let mut r3 = Prng::default();
        for _ in 0..1000 {
            assert!(r3.range(0, 3000) <= 3000);
        }
    }

    #[test]
    fn empty_drive_answers_getstat_with_the_shell_open() {
        let mut c = Cdrom::new(None, *b"SCEA");
        let mut spu = Spu::default();
        let (int, t, bytes) = exchange(&mut c, &mut spu, 0, 0x01);
        assert_eq!((int, bytes), (3, vec![0x10]));
        // 10500..13500 + 1815, then 8500 more before it runs.
        assert!((10500 + 1815 + 8500..=13500 + 1815 + 8500 + 768).contains(&t), "{t}");
        // GetlocL with no disc: not ready.
        let (int, _, bytes) = exchange(&mut c, &mut spu, t + 5000, 0x10);
        assert_eq!((int, bytes), (5, vec![0x11, 0x80]));
        run(&mut c, &mut spu, t + 100_000);
    }

    #[test]
    fn unknown_command_and_bad_parameter_count() {
        let mut c = Cdrom::new(None, *b"SCEA");
        let mut spu = Spu::default();
        let (int, t, bytes) = exchange(&mut c, &mut spu, 0, 0x17);
        assert_eq!((int, bytes), (5, vec![0x11, 0x40]));
        assert_eq!(c.unknown_commands, vec![0x17]);
        // Setloc without its three parameters.
        let (int, _, bytes) = exchange(&mut c, &mut spu, t + 5000, 0x02);
        assert_eq!((int, bytes), (5, vec![0x11, 0x20]));
        assert_eq!(c.recent.back().unwrap().ints, vec![5]);
    }

    #[test]
    fn status_register_index_and_fifo_bits() {
        let mut c = Cdrom::new(None, *b"SCEA");
        let mut spu = Spu::default();
        assert_eq!(c.read(0), 0x18, "parameter FIFO empty and not full");
        c.write(0, 0, 0, None, &mut spu);
        c.write(2, 0x55, 0, None, &mut spu);
        assert_eq!(c.read(0) & 0x08, 0);
        c.write(0, 0, 0, None, &mut spu);
        assert_eq!(c.read(3), 0xFF, "index 0 reads FFh, not the interrupt enable");
        c.write(0, 1, 0, None, &mut spu);
        assert_eq!(c.read(3), 0xE0);
    }

    #[test]
    fn subq_has_the_pregap_counting_down_to_index_01() {
        // Mednafen: the sector before INDEX 01 has relative time 00:00:00.
        let mut q = [0u8; 12];
        q[0] = 0x41;
        subq_crc(&mut q);
        assert!(subq_crc_ok(&q));
        q[11] ^= 0xFF;
        assert!(!subq_crc_ok(&q));
        assert_eq!(aba_msf(150), [0x00, 0x02, 0x00]);
    }

    #[test]
    fn edc_of_a_zero_form2_sector() {
        let mut buf = vec![0u8; 2352];
        synth_data_sector(&mut buf, TrackMode::Mode2Raw, 0);
        assert_eq!(&buf[..12], &SYNC);
        assert_eq!(&buf[12..16], &[0x00, 0x02, 0x00, 0x02]);
        assert_eq!(buf[18], 0x20);
        assert_eq!(u32::from_le_bytes(buf[2348..].try_into().unwrap()), edc(&buf[16..2348]));
    }
}

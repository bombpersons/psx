//! System bus: main RAM, scratchpad, BIOS ROM, expansion and I/O.
//!
//! Devices are not modelled yet. I/O is a plain register file that remembers
//! writes and returns them on reads, and every I/O access is recorded so a
//! harness can see (and override) what the CPU exchanged with hardware.

pub const RAM_SIZE: usize = 2 * 1024 * 1024;
pub const BIOS_SIZE: usize = 512 * 1024;
pub const SCRATCH_SIZE: usize = 1024;
pub const I_STAT: u32 = 0x1F80_1070;
pub const I_MASK: u32 = 0x1F80_1074;
/// DMA channel n registers: MADR at 0x1F801080 + 0x10*n, then BCR, CHCR.
pub const DMA_BASE: u32 = 0x1F80_1080;
pub const DPCR: u32 = 0x1F80_10F0;
pub const DICR: u32 = 0x1F80_10F4;
pub const TIMERS: u32 = 0x1F80_1100;
pub const GP0: u32 = 0x1F80_1810;
/// System clock (Hz).
pub const SYSCLK: u64 = 33_868_800;
/// Device catch-up granularity in system clocks (see [`Bus::tick`]).
pub const DEVICE_BATCH: u64 = 128;
pub const GP1: u32 = 0x1F80_1814;

/// A DMA transfer the bus started but whose device side is not modelled yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnmodelledDma {
    pub channel: u32,
    pub madr: u32,
    pub bcr: u32,
    pub chcr: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Byte = 1,
    Half = 2,
    Word = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoAccess {
    pub addr: u32,
    pub width: Width,
    pub write: bool,
    pub value: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Bus {
    pub ram: Vec<u8>,
    #[serde(with = "crate::state::big_array")]
    pub scratch: [u8; SCRATCH_SIZE],
    /// Not saved: restored from the BIOS file (its hash is in the state header).
    #[serde(skip)]
    pub bios: Vec<u8>,
    /// Physical 0x1F801000..0x1F803000 as a register file.
    pub io: Vec<u8>,
    pub cache_control: u32,
    /// I/O accesses since the last [`Bus::take_io_log`] (only when `log_io` is set).
    #[serde(skip)]
    io_log: Vec<IoAccess>,
    #[serde(skip)]
    pub log_io: bool,
    /// Set when software writes I_STAT; cleared by [`Bus::take_irq_ack`].
    irq_acknowledged: bool,
    /// Started transfers with no device model (their RAM/device effects are missing).
    #[serde(skip)]
    pub unmodelled_dma: Vec<UnmodelledDma>,
    /// MDEC-out (channel 1) transfer waiting for decoder output: (madr, bcr, chcr, words).
    pending_mdec_out: Option<(u32, u32, u32, u32)>,
    /// The most recent DMA transfers (channel, madr, bcr, chcr), newest last.
    #[serde(skip)]
    pub recent_dma: std::collections::VecDeque<UnmodelledDma>,
    pub gpu: psx_gpu::Gpu,
    /// The last vblank by which the GPU had drawn something new (see
    /// [`psx_gpu::Gpu::draws`]), and the draw count then. Analysis only.
    #[serde(skip)]
    pub last_draw_frame: u64,
    #[serde(skip)]
    draws_seen: u64,
    /// GP1(00h)/(01h) etc. are not tracked in GPUSTAT yet; this mirrors the
    /// display/DMA bits software set so status reads are plausible.
    gpustat_dma_dir: u32,
    pub timers: crate::timers::Timers,
    /// System clock cycles elapsed, as far as devices have been advanced.
    pub now: u64,
    /// CPU cycles not yet handed to devices.
    pending: u64,
    irq_cache: bool,
    irq_dirty: bool,
    next_vblank: u64,
    /// Completed video frames (vblanks).
    pub frames: u64,
    /// When false, devices do not raise interrupts (lockstep supplies them).
    pub irq_sources: bool,
    pub cdrom: crate::cdrom::Cdrom,
    cdrom_irq: bool,
    pub sio0: crate::sio0::Sio0,
    pub mdec: crate::mdec::Mdec,
    pub spu: crate::spu::Spu,
    /// Analysis hooks (write watches, CD transfers, BIOS calls).
    #[serde(skip)]
    pub obs: crate::observe::Observer,
    /// PC of the instruction currently executing (set by the CPU each step).
    pub cur_pc: u32,
    /// Reads of I/O registers no device handles yet: physical address -> count.
    #[serde(skip)]
    pub unhandled_reads: std::collections::BTreeMap<u32, u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BusError {
    /// No device decodes this physical address.
    Unmapped(u32),
}

fn physical(addr: u32) -> u32 {
    if addr >= 0xC000_0000 { addr } else { addr & 0x1FFF_FFFF }
}

impl Bus {
    pub fn new(bios: Vec<u8>) -> Self {
        let mut b = bios;
        b.resize(BIOS_SIZE, 0);
        Bus {
            ram: vec![0; RAM_SIZE],
            scratch: [0; SCRATCH_SIZE],
            bios: b,
            io: vec![0; 0x2000],
            cache_control: 0,
            io_log: Vec::new(),
            log_io: false,
            irq_acknowledged: false,
            unmodelled_dma: Vec::new(),
            recent_dma: Default::default(),
            pending_mdec_out: None,
            gpu: psx_gpu::Gpu::new(),
            last_draw_frame: 0,
            draws_seen: 0,
            gpustat_dma_dir: 0,
            timers: Default::default(),
            now: 0,
            pending: 0,
            irq_cache: false,
            irq_dirty: true,
            next_vblank: 0,
            frames: 0,
            irq_sources: true,
            cdrom: Default::default(),
            cdrom_irq: false,
            sio0: Default::default(),
            mdec: Default::default(),
            spu: Default::default(),
            unhandled_reads: Default::default(),
            obs: Default::default(),
            cur_pc: 0,
        }
    }

    /// Record a DMA write of `bytes` at physical `start` if it touches a watch.
    fn observe_dma(&mut self, ch: u32, start: u32, bytes: u32) {
        if self.obs.active() && self.obs.overlaps(start, bytes) {
            let (frame, cycle) = (self.frames, self.now);
            self.obs.record(crate::observe::WriteHit {
                frame,
                cycle,
                writer: crate::observe::Writer::Dma,
                address: 0x8000_0000 | (start & 0x1F_FFFF),
                width: bytes,
                pc: None,
                value: None,
                old: None,
                dma_channel: Some(ch),
            });
        }
    }

    /// An SPU register write, recorded first for `--trace-spu`.
    fn spu_write(&mut self, off: u32, v: u16) -> bool {
        if self.obs.spu.is_some() {
            self.observe_spu_write(off, v);
        }
        self.spu.write16(off, v)
    }

    fn observe_spu_write(&mut self, off: u32, v: u16) {
        use crate::observe::{KeyOn, SpuEvent};
        let (frame, cycle, pc) = (self.frames, self.cpu_now(), self.cur_pc);
        let (event, first) = match off {
            0x188 => ("key_on", 0),
            0x18A => ("key_on", 16),
            0x18C => ("key_off", 0),
            0x18E => ("key_off", 16),
            0x1A8 => {
                let address = self.spu.transfer_addr();
                self.obs.spu.as_mut().unwrap().cpu_upload(frame, cycle, pc, address);
                return;
            }
            _ => return,
        };
        for voice in (first..(first + 16).min(24)).filter(|i| v & (1 << (i - first)) != 0) {
            let key_on = (event == "key_on").then(|| {
                let (start, pitch, volume_raw, adsr, noise, pitch_modulated) = self.spu.voice_settings(voice as usize);
                let extent = psx_assets::adpcm::sample_extent(&self.spu.ram, start as usize);
                let last_flags = if extent.is_empty() { 0 } else { self.spu.ram[extent.end - 15] };
                // Fixed volumes hold volume/2 in bits 0-14; bit 15 selects a sweep.
                let fraction = |raw: u16| (raw & 0x8000 == 0).then(|| ((raw << 1) as i16) as f64 / 32768.0);
                KeyOn {
                    start,
                    pitch,
                    rate_hz: pitch.min(0x4000) as f64 * 44_100.0 / 4096.0,
                    volume: volume_raw.map(fraction),
                    volume_raw,
                    adsr,
                    noise,
                    pitch_modulated,
                    sample_sha256: psx_assets::adpcm::sample_sha256(&self.spu.ram, start as usize),
                    sample_bytes: extent.len() as u32,
                    looped: last_flags & psx_assets::adpcm::FLAG_REPEAT != 0,
                }
            });
            let log = self.obs.spu.as_mut().unwrap();
            if event == "key_on" {
                log.awaiting_end |= 1 << voice;
            }
            log.record(SpuEvent { frame, cycle, event, voice: Some(voice), pc: Some(pc), key_on, upload: None });
        }
    }

    pub fn pal(&self) -> bool {
        self.gpu.display.pal()
    }

    /// System clocks per video frame (PAL 50 Hz, NTSC 60 Hz; field timing approximated).
    pub fn frame_cycles(&self) -> u64 {
        if self.pal() { SYSCLK / 50 } else { SYSCLK * 1001 / 60000 }
    }

    fn raise_irq(&mut self, bits: u32) {
        if self.irq_sources && bits != 0 {
            let v = self.io_word(I_STAT) | bits;
            self.poke_io(I_STAT, Width::Word, v);
        }
    }

    /// Account `cycles` of CPU time. Devices catch up in batches of
    /// [`DEVICE_BATCH`] cycles and always before any I/O access, so software
    /// never observes stale device state; interrupts can be up to one batch late.
    #[inline]
    pub fn tick(&mut self, cycles: u64) {
        self.pending += cycles;
        if self.pending >= DEVICE_BATCH {
            self.flush();
        }
    }

    /// System clocks elapsed including CPU time not yet handed to devices.
    #[inline]
    pub fn time(&self) -> u64 {
        self.now + self.pending
    }

    /// Bring every device up to the CPU's time.
    #[inline]
    pub fn flush(&mut self) {
        if self.pending > 0 {
            let c = std::mem::take(&mut self.pending);
            self.run_devices(c);
        }
    }

    /// System clock including CPU time not yet handed to devices.
    pub fn cpu_now(&self) -> u64 {
        self.now + self.pending
    }

    fn run_devices(&mut self, cycles: u64) {
        self.now += cycles;
        if self.next_vblank == 0 {
            self.next_vblank = self.frame_cycles();
        }
        // Both rates are 16.16 fixed point per system clock. Every case is a
        // compile-time constant (this runs every device batch; no divides).
        const fn hblank_rate(lines: u64, frame_cycles: u64) -> u64 {
            ((1u64 << 16) * lines) / frame_cycles
        }
        let hblank = if self.pal() {
            const { hblank_rate(314, SYSCLK / 50) }
        } else {
            const { hblank_rate(263, SYSCLK * 1001 / 60000) }
        };
        // Dot clock: 53.693175 MHz / divider (10, 8, 5, 4 for widths 256, 320, 512, 640; 7 for 368).
        const fn dot_rate(div: u64) -> u64 {
            ((53_693_175u64 << 16) / div) / SYSCLK
        }
        let dotclock = match self.gpu.display.width() {
            256 => const { dot_rate(10) },
            320 => const { dot_rate(8) },
            368 => const { dot_rate(7) },
            512 => const { dot_rate(5) },
            _ => const { dot_rate(4) },
        };
        let irq = self.timers.tick(cycles, crate::timers::Rates { dotclock, hblank });
        self.raise_irq(irq);
        if self.spu.tick(cycles, &mut self.cdrom.audio) {
            self.raise_irq(1 << 9);
        }
        if let Some(log) = &mut self.obs.spu {
            let ended = self.spu.endx() & log.awaiting_end;
            for voice in (0..24).filter(|v| ended & (1 << v) != 0) {
                log.awaiting_end &= !(1 << voice);
                log.record(crate::observe::SpuEvent {
                    frame: self.frames,
                    cycle: self.now,
                    event: "end",
                    voice: Some(voice),
                    pc: None,
                    key_on: None,
                    upload: None,
                });
            }
        }
        if self.sio0.tick(self.now) {
            self.raise_irq(1 << 7);
        }
        let cd = self.cdrom.tick(self.now);
        if cd && !self.cdrom_irq {
            self.raise_irq(1 << 2);
        }
        self.cdrom_irq = cd;
        if self.now >= self.next_vblank {
            self.next_vblank += self.frame_cycles();
            self.frames += 1;
            if self.gpu.draws != self.draws_seen {
                self.draws_seen = self.gpu.draws;
                self.last_draw_frame = self.frames;
            }
            self.raise_irq(1);
        }
    }

    fn ram_word(&self, addr: u32) -> u32 {
        self.ram_u32((addr & 0x1F_FFFC) as usize)
    }

    /// DMA channel 2: block transfers either way, or a linked list of GP0 packets.
    fn dma_gpu(&mut self, madr: u32, bcr: u32, chcr: u32) {
        let to_device = chcr & 1 != 0;
        let step: u32 = if chcr & 2 != 0 { 4u32.wrapping_neg() } else { 4 };
        match (chcr >> 9) & 3 {
            2 => {
                let mut addr = madr & 0x1F_FFFC;
                for _ in 0..0x10_0000 {
                    let header = self.ram_word(addr);
                    for i in 1..=(header >> 24) {
                        let w = self.ram_word(addr + 4 * i);
                        self.gpu.gp0(w);
                    }
                    if header & 0x80_0000 != 0 {
                        break;
                    }
                    addr = header & 0x1F_FFFC;
                }
            }
            0 | 1 => {
                let words = if (chcr >> 9) & 3 == 0 {
                    if bcr & 0xFFFF == 0 { 0x10000 } else { bcr & 0xFFFF }
                } else {
                    (bcr & 0xFFFF) * (bcr >> 16).max(1)
                };
                let mut addr = madr & 0x1F_FFFC;
                for _ in 0..words {
                    if to_device {
                        let w = self.ram_word(addr);
                        self.gpu.gp0(w);
                    } else {
                        let w = self.gpu.gpuread().unwrap_or(0);
                        let o = addr as usize;
                        self.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
                    }
                    addr = addr.wrapping_add(step) & 0x1F_FFFC;
                }
            }
            _ => {}
        }
    }

    /// Plausible GPUSTAT: always ready (bits 26-28), DMA direction, display
    /// state. Timing-dependent bits (interlace field, busy) are not modelled.
    fn gpustat(&self) -> u32 {
        let d = &self.gpu.display;
        let st = &self.gpu.state;
        let mut v = (st.texpage as u32 & 0x7FF) | ((st.set_mask as u32) << 11) | ((st.check_mask as u32) << 12);
        v |= (d.mode & 0x3F) << 17 | ((d.mode >> 6) & 1) << 16 | ((d.mode >> 7) & 1) << 14;
        v |= ((!d.enabled) as u32) << 23;
        v |= 0b111 << 26;
        v |= self.gpustat_dma_dir << 29;
        // Bit 31: odd/even line. Toggles per field when interlaced, per scanline
        // otherwise, and reads 0 during vblank.
        let (line, in_vblank) = self.scanline();
        let odd = if d.mode & 0x20 != 0 { self.frames & 1 == 1 } else { line & 1 == 1 };
        if odd && !in_vblank {
            v |= 1 << 31;
        }
        if d.mode & 0x20 != 0 && self.frames & 1 == 1 {
            v |= 1 << 13;
        }
        v
    }

    /// Current scanline within the frame and whether it is in vblank.
    pub fn scanline(&self) -> (u64, bool) {
        let fc = self.frame_cycles();
        let lines = if self.pal() { 314 } else { 263 };
        let into = (self.now + fc).saturating_sub(self.next_vblank.max(fc)) % fc;
        let line = into * lines / fc;
        let visible = if self.pal() { 288 } else { 240 };
        (line, line >= visible)
    }

    /// CHCR was written: run the transfer immediately if it is started.
    /// Completion interrupts are not raised yet.
    fn dma_start(&mut self, ch: u32) {
        let base = DMA_BASE + 0x10 * ch;
        let (madr, bcr, chcr) = (self.io_word(base), self.io_word(base + 4), self.io_word(base + 8));
        let sync = (chcr >> 9) & 3;
        // Mode 0 needs the trigger bit (28); other modes start on bit 24 alone.
        let started = chcr & (1 << 24) != 0 && (sync != 0 || chcr & (1 << 28) != 0);
        if !started {
            return;
        }
        if self.recent_dma.len() >= 32 {
            self.recent_dma.pop_front();
        }
        self.recent_dma.push_back(UnmodelledDma { channel: ch, madr, bcr, chcr });
        match ch {
            6 => {
                // Ordering-table clear: a backwards linked list ending in 0xFFFFFF.
                let mut addr = madr & 0x1F_FFFC;
                let words = if bcr & 0xFFFF == 0 { 0x10000 } else { bcr & 0xFFFF };
                self.observe_dma(6, addr.wrapping_sub(4 * (words - 1)) & 0x1F_FFFC, words * 4);
                for i in 0..words {
                    let v = if i == words - 1 { 0x00FF_FFFF } else { addr.wrapping_sub(4) & 0x1F_FFFF };
                    let o = addr as usize % RAM_SIZE;
                    self.ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
                    addr = addr.wrapping_sub(4) & 0x1F_FFFC;
                }
            }
            2 => self.dma_gpu(madr, bcr, chcr),
            0 => {
                let words = Self::dma_words(bcr, chcr);
                let step: u32 = if chcr & 2 != 0 { 4u32.wrapping_neg() } else { 4 };
                let mut addr = madr & 0x1F_FFFC;
                for _ in 0..words {
                    let w = self.ram_word(addr);
                    self.mdec.write_word(w);
                    addr = addr.wrapping_add(step) & 0x1F_FFFC;
                }
                self.try_mdec_out();
            }
            1 => {
                // Request-driven: completes only once the MDEC has produced the data.
                self.pending_mdec_out = Some((madr, bcr, chcr, Self::dma_words(bcr, chcr)));
                self.try_mdec_out();
                return;
            }
            4 => {
                let words = Self::dma_words(bcr, chcr);
                if chcr & 1 == 0 {
                    self.observe_dma(4, madr & 0x1F_FFFC, words * 4);
                }
                let mut addr = madr & 0x1F_FFFC;
                let mut fired = false;
                if chcr & 1 != 0 {
                    if let Some(log) = &mut self.obs.spu {
                        let upload = crate::observe::SpuUpload {
                            address: self.spu.transfer_addr(),
                            bytes: words * 4,
                            source: "dma",
                            ram: Some(0x8000_0000 | addr),
                        };
                        log.record(crate::observe::SpuEvent {
                            frame: self.frames,
                            cycle: self.now,
                            event: "upload",
                            voice: None,
                            pc: Some(self.cur_pc),
                            key_on: None,
                            upload: Some(upload),
                        });
                    }
                }
                for _ in 0..words {
                    if chcr & 1 != 0 {
                        let w = self.ram_word(addr);
                        fired |= self.spu.fifo_write(w as u16);
                        fired |= self.spu.fifo_write((w >> 16) as u16);
                    } else {
                        let ((lo, f0), (hi, f1)) = (self.spu.fifo_read(), self.spu.fifo_read());
                        fired |= f0 | f1;
                        let w = lo as u32 | (hi as u32) << 16;
                        let o = addr as usize;
                        self.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
                    }
                    addr = (addr + 4) & 0x1F_FFFC;
                }
                if fired {
                    self.raise_irq(1 << 9);
                }
            }
            3 => {
                let words = if (chcr >> 9) & 3 == 0 {
                    if bcr & 0xFFFF == 0 { 0x10000 } else { bcr & 0xFFFF }
                } else {
                    (bcr & 0xFFFF) * (bcr >> 16).max(1)
                };
                self.cdrom.note_dma(words * 4);
                let (lba, off, size) = self.cdrom.fifo_state();
                let remaining = self.cdrom.fifo_remaining();
                let (frame, cycle, pc) = (self.frames, self.now, self.cur_pc);
                if let Some(log) = &mut self.obs.cd {
                    log.push(crate::observe::CdTransfer {
                        frame,
                        cycle,
                        lba,
                        fifo_offset: off,
                        bytes: (words * 4).min(remaining),
                        sector_size: size,
                        ram: 0x8000_0000 | (madr & 0x1F_FFFC),
                        pc,
                    });
                }
                self.observe_dma(3, madr & 0x1F_FFFC, words * 4);
                let mut addr = madr & 0x1F_FFFC;
                for _ in 0..words {
                    let w = self.cdrom.dma_word();
                    let o = addr as usize;
                    self.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
                    addr = (addr + 4) & 0x1F_FFFC;
                }
            }
            _ => self.unmodelled_dma.push(UnmodelledDma { channel: ch, madr, bcr, chcr }),
        }
        self.finish_dma(ch, chcr);
    }

    fn dma_words(bcr: u32, chcr: u32) -> u32 {
        if (chcr >> 9) & 3 == 0 {
            if bcr & 0xFFFF == 0 { 0x10000 } else { bcr & 0xFFFF }
        } else {
            (bcr & 0xFFFF) * (bcr >> 16).max(1)
        }
    }

    /// Finish a pending MDEC-out DMA if enough decoded words are available.
    fn try_mdec_out(&mut self) {
        let Some((madr, _bcr, chcr, words)) = self.pending_mdec_out else { return };
        if self.mdec.out_available() < words as usize {
            return;
        }
        self.pending_mdec_out = None;
        self.observe_dma(1, madr & 0x1F_FFFC, words * 4);
        let step: u32 = if chcr & 2 != 0 { 4u32.wrapping_neg() } else { 4 };
        let mut addr = madr & 0x1F_FFFC;
        for _ in 0..words {
            let w = self.mdec.dma_out_word();
            let o = addr as usize;
            self.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
            addr = addr.wrapping_add(step) & 0x1F_FFFC;
        }
        self.finish_dma(1, chcr);
    }

    /// Mark channel `ch` complete: clear busy bits and flag DICR.
    fn finish_dma(&mut self, ch: u32, chcr: u32) {
        let base = DMA_BASE + 0x10 * ch;
        self.poke_io(base + 8, Width::Word, chcr & !((1 << 24) | (1 << 28)));
        // A completion flag is only latched while both the channel's enable and
        // the master enable (bit 23) are set (as PCSX-Redux does); otherwise
        // stale flags would fire when software later enables the interrupt.
        let dicr = self.io_word(DICR);
        if dicr & (1 << 23) != 0 && dicr & (1 << (16 + ch)) != 0 {
            self.set_dicr(dicr | (1 << (24 + ch)));
        }
    }

    /// Store DICR, recompute the master flag and raise I_STAT bit 3 on its rising edge.
    fn set_dicr(&mut self, v: u32) {
        let old_master = self.io_word(DICR) & (1 << 31) != 0;
        // Master flag as PCSX-Redux computes it: force, or master enable with *any*
        // channel flag set (not ANDed with the per-channel enables, unlike the
        // psx-spx formula). Croc's loader depends on this: enabling DMA IRQs
        // fires once and its handler acknowledges stale flags left by the BIOS.
        // Open question: which formula real hardware uses.
        let master = v & (1 << 15) != 0 || (v & (1 << 23) != 0 && (v >> 24) & 0x7F != 0);
        let v = (v & 0x7FFF_FFFF) | ((master as u32) << 31);
        self.poke_io(DICR, Width::Word, v);
        if master && !old_master {
            self.raise_irq(1 << 3);
        }
    }

    fn io_word(&self, phys: u32) -> u32 {
        let o = (phys - 0x1F80_1000) as usize;
        u32::from_le_bytes([self.io[o], self.io[o + 1], self.io[o + 2], self.io[o + 3]])
    }

    /// Interrupt controller output: any enabled, pending source (Cause.IP2).
    pub fn irq_pending(&self) -> bool {
        self.io_word(I_STAT) & self.io_word(I_MASK) & 0x7FF != 0
    }

    /// Cached [`Bus::irq_pending`], recomputed only after I_STAT/I_MASK change.
    #[inline]
    pub fn irq_line(&mut self) -> bool {
        if self.irq_dirty {
            self.irq_cache = self.irq_pending();
            self.irq_dirty = false;
        }
        self.irq_cache
    }

    /// Recompute derived caches after a save state was decoded.
    pub(crate) fn after_restore(&mut self) {
        self.irq_dirty = true;
        // Draw tracking is not saved: count idle frames from the restore.
        self.last_draw_frame = self.frames;
        self.draws_seen = self.gpu.draws;
    }

    #[inline]
    pub fn take_irq_ack(&mut self) -> bool {
        // Runs every instruction: only store when the flag is set.
        let acked = self.irq_acknowledged;
        if acked {
            self.irq_acknowledged = false;
        }
        acked
    }

    fn log_access(&mut self, a: IoAccess) {
        if self.log_io {
            self.io_log.push(a);
        }
    }

    pub fn take_io_log(&mut self) -> Vec<IoAccess> {
        std::mem::take(&mut self.io_log)
    }

    /// True when a virtual address decodes to the I/O region.
    pub fn is_io(addr: u32) -> bool {
        let p = physical(addr);
        (0x1F80_1000..0x1F80_3000).contains(&p)
    }

    /// Word at a RAM offset (`o` is already reduced modulo [`RAM_SIZE`]).
    #[inline(always)]
    fn ram_u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.ram[o..o + 4].try_into().unwrap())
    }

    #[inline(always)]
    pub fn read(&mut self, addr: u32, width: Width) -> Result<u32, BusError> {
        let p = physical(addr);
        // Fast paths: RAM and BIOS (the overwhelmingly common cases), inlined
        // into the CPU; everything else goes through the out-of-line decoder.
        if p < 0x0080_0000 {
            let o = p as usize % RAM_SIZE;
            return Ok(match width {
                Width::Word => self.ram_u32(o),
                Width::Half => u16::from_le_bytes([self.ram[o], self.ram[o + 1]]) as u32,
                Width::Byte => self.ram[o] as u32,
            });
        }
        if width == Width::Word && (0x1FC0_0000..0x1FC8_0000).contains(&p) {
            let o = (p - 0x1FC0_0000) as usize;
            return Ok(u32::from_le_bytes(self.bios[o..o + 4].try_into().unwrap()));
        }
        self.read_slow(addr, p, width)
    }

    #[inline(never)]
    fn read_slow(&mut self, addr: u32, p: u32, width: Width) -> Result<u32, BusError> {
        if (0x1F80_1000..0x1F80_3000).contains(&p) {
            self.flush();
        }
        let n = width as usize;
        let le = |b: &[u8], o: usize| -> u32 { b[o..o + n].iter().rev().fold(0u32, |acc, &x| (acc << 8) | x as u32) };
        Ok(match p {
            0x0000_0000..=0x007F_FFFF => le(&self.ram, p as usize % RAM_SIZE),
            0x1F00_0000..=0x1F7F_FFFF => u32::MAX >> (32 - 8 * n),
            0x1F80_0000..=0x1F80_03FF if addr >> 29 != 5 => le(&self.scratch, (p - 0x1F80_0000) as usize),
            GP0 if width == Width::Word => {
                let v = self.gpu.gpuread().unwrap_or(0);
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1100..=0x1F80_112F => {
                let v = self.timers.read(((p - TIMERS) >> 4) as usize, p & 0xC) >> (8 * (p & 3));
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            GP1 if width == Width::Word => {
                let v = self.gpustat();
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1820 | 0x1F80_1824 if width == Width::Word => {
                let v = self.mdec.read(p - 0x1F80_1820);
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1040..=0x1F80_104F => {
                let v = self.sio0.read(p - 0x1F80_1040);
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1800..=0x1F80_1803 => {
                let now = self.now;
                let mut v = 0u32;
                for i in 0..n as u32 {
                    v |= (self.cdrom.read((p + i).min(0x1F80_1803) - 0x1F80_1800, now) as u32) << (8 * i);
                }
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1C00..=0x1F80_1FFF => {
                let mut v = 0u32;
                let mut o = p & !1;
                while o < p + n as u32 {
                    v |= (self.spu.read16(o - 0x1F80_1C00) as u32) << (8 * (o - (p & !1)));
                    o += 2;
                }
                let v = if p & 1 != 0 { v >> 8 } else { v };
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1000..=0x1F80_2FFF => {
                let v = le(&self.io, (p - 0x1F80_1000) as usize);
                // Memory control (1F801000-23h, RAM_SIZE 1F801060h) and SIO1's mode/control/baud
                // (1F801058-5Fh) are plain read/write latches, so the stored value is the answer.
                let latch = (0x1F80_1000..0x1F80_1024).contains(&p)
                    || (0x1F80_1060..0x1F80_1064).contains(&p)
                    || (0x1F80_1058..0x1F80_1060).contains(&p);
                let handled = latch
                    || (0x1F80_1C00..0x1F80_1E00).contains(&p)
                    || (0x1F80_1800..0x1F80_1804).contains(&p)
                    || (0x1F80_1070..0x1F80_1078).contains(&p)
                    || (0x1F80_1080..0x1F80_1100).contains(&p);
                if !handled {
                    *self.unhandled_reads.entry(p).or_default() += 1;
                }
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1FC0_0000..=0x1FC7_FFFF => le(&self.bios, (p - 0x1FC0_0000) as usize),
            0xFFFE_0130 => self.cache_control,
            _ => return Err(BusError::Unmapped(p)),
        })
    }

    #[inline(always)]
    pub fn write(&mut self, addr: u32, width: Width, value: u32) -> Result<(), BusError> {
        let p = physical(addr);
        // Fast path: RAM with no write watch armed.
        if p < 0x0080_0000 && !self.obs.active() {
            let o = p as usize % RAM_SIZE;
            match width {
                Width::Word => self.ram[o..o + 4].copy_from_slice(&value.to_le_bytes()),
                Width::Half => self.ram[o..o + 2].copy_from_slice(&(value as u16).to_le_bytes()),
                Width::Byte => self.ram[o] = value as u8,
            }
            return Ok(());
        }
        self.write_slow(addr, p, width, value)
    }

    #[inline(never)]
    fn write_slow(&mut self, addr: u32, p: u32, width: Width, value: u32) -> Result<(), BusError> {
        if (0x1F80_1000..0x1F80_3000).contains(&p) {
            self.flush();
            if (I_STAT..I_MASK + 4).contains(&p) {
                self.irq_dirty = true;
            }
        }
        let n = width as usize;
        let put = |b: &mut [u8], o: usize| {
            for i in 0..n {
                b[o + i] = (value >> (8 * i)) as u8;
            }
        };
        match p {
            0x0000_0000..=0x007F_FFFF => {
                let o = p as usize % RAM_SIZE;
                if self.obs.active() && self.obs.overlaps(o as u32, n as u32) {
                    let old = self.ram[o..o + n].iter().rev().fold(0u32, |acc, &x| (acc << 8) | x as u32);
                    let (frame, cycle, pc) = (self.frames, self.cpu_now(), self.cur_pc);
                    self.obs.record(crate::observe::WriteHit {
                        frame,
                        cycle,
                        writer: crate::observe::Writer::Cpu,
                        address: addr,
                        width: n as u32,
                        pc: Some(pc),
                        value: Some(value & (u32::MAX >> (32 - 8 * n))),
                        old: Some(old),
                        dma_channel: None,
                    });
                }
                put(&mut self.ram, o)
            }
            0x1F00_0000..=0x1F7F_FFFF => {}
            0x1F80_0000..=0x1F80_03FF if addr >> 29 != 5 => put(&mut self.scratch, (p - 0x1F80_0000) as usize),
            0x1F80_1000..=0x1F80_2FFF => {
                if (0x1F80_1C00..0x1F80_2000).contains(&p) {
                    let mut fired = self.spu_write((p & !1) - 0x1F80_1C00, value as u16);
                    if width == Width::Word {
                        fired |= self.spu_write((p & !1) + 2 - 0x1F80_1C00, (value >> 16) as u16);
                    }
                    if fired {
                        self.raise_irq(1 << 9);
                    }
                } else if (p == 0x1F80_1820 || p == 0x1F80_1824) && width == Width::Word {
                    self.mdec.write(p - 0x1F80_1820, value);
                    self.try_mdec_out();
                } else if (0x1F80_1040..0x1F80_1050).contains(&p) {
                    let now = self.now;
                    self.sio0.write(p - 0x1F80_1040, value, now);
                } else if (0x1F80_1800..0x1F80_1804).contains(&p) {
                    let now = self.now;
                    self.cdrom.write_at_frame(p - 0x1F80_1800, value as u8, now, self.frames);
                } else if (TIMERS..TIMERS + 0x30).contains(&p) {
                    self.timers.write(((p - TIMERS) >> 4) as usize, p & 0xC, value);
                } else if p == GP0 && width == Width::Word {
                    self.gpu.gp0(value);
                } else if p == GP1 && width == Width::Word {
                    if (value >> 24) & 0x3F == 0x04 {
                        self.gpustat_dma_dir = value & 3;
                    }
                    self.gpu.gp1(value);
                } else if p & !3 >= DMA_BASE && p & !3 < DPCR && (p & 0xF) == 8 {
                    put(&mut self.io, (p - 0x1F80_1000) as usize);
                    self.dma_start((p - DMA_BASE) >> 4);
                } else if p & !3 == DICR {
                    // Bits 24-30 are write-1-to-clear flags; bit 31 is computed.
                    let cur = self.io_word(DICR);
                    let v = if width == Width::Word {
                        value
                    } else {
                        (cur & !(0xFFFF << (8 * (p & 2)))) | (value << (8 * (p & 2)))
                    };
                    let flags = (cur & 0x7F00_0000) & !(v & 0x7F00_0000);
                    self.set_dicr((v & 0x00FF_803F) | flags);
                } else if p & !3 == I_STAT {
                    // Interrupt acknowledge: writing 0 clears a bit, 1 leaves it.
                    let cur = self.io_word(I_STAT);
                    let v = cur & (value << (8 * (p & 3)) | !(u32::MAX >> (32 - 8 * n) << (8 * (p & 3))));
                    self.poke_io(I_STAT, Width::Word, v);
                    self.irq_acknowledged = true;
                } else {
                    put(&mut self.io, (p - 0x1F80_1000) as usize);
                }
                self.log_access(IoAccess { addr: p, width, write: true, value });
            }
            0x1FC0_0000..=0x1FC7_FFFF => {}
            0xFFFE_0130 => self.cache_control = value,
            _ => return Err(BusError::Unmapped(p)),
        }
        Ok(())
    }

    /// Overwrite an I/O register's stored value (harness injection).
    pub fn poke_io(&mut self, phys: u32, width: Width, value: u32) {
        if (I_STAT..I_MASK + 4).contains(&phys) {
            self.irq_dirty = true;
        }
        let o = (phys - 0x1F80_1000) as usize;
        for i in 0..width as usize {
            self.io[o + i] = (value >> (8 * i)) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cd_history_keeps_exact_frames_across_video_mode_changes_and_polling() {
        let mut m = crate::Machine::new(vec![0; 512 * 1024]);
        m.bus.frames = 42;
        m.bus.now = 10_000_000;
        m.bus.gpu.display.mode = 8;
        m.bus.write(0x1f801801, crate::Width::Byte, 1).unwrap();
        m.bus.frames = 43;
        m.bus.now = 11_000_000;
        m.bus.gpu.display.mode = 0;
        m.bus.write(0x1f801801, crate::Width::Byte, 1).unwrap();
        let command = m.bus.cdrom.recent.back().unwrap();
        assert_eq!(command.frame, Some(42));
        assert_eq!(command.last_frame, Some(43));
        assert_eq!(command.time, 10_000_000);
        assert_eq!(command.last_time, 11_000_000);
        assert_eq!(command.repeats, 2);
    }

    use super::*;

    #[test]
    fn spu_trace_records_uploads_key_on_off_and_the_first_end_of_keyed_voices() {
        let mut b = Bus::new(vec![]);
        b.obs.spu = Some(crate::observe::SpuLog::new(100));
        // A silent block, a data block and an END block at 0x80100000.
        let mut sample = [0u8; 48];
        sample[16] = 0x0C;
        sample[18..32].fill(0x77);
        sample[33] = psx_assets::adpcm::FLAG_END;
        for (i, w) in sample.chunks(4).enumerate() {
            b.write(0x8010_0000 + 4 * i as u32, Width::Word, u32::from_le_bytes(w.try_into().unwrap())).unwrap();
        }
        let spu = |b: &mut Bus, reg: u32, v: u32| b.write(0x1F80_1C00 + reg, Width::Half, v).unwrap();
        spu(&mut b, 0x1A6, 0x1000 >> 3);
        b.write(DMA_BASE + 0x40, Width::Word, 0x8010_0000).unwrap();
        b.write(DMA_BASE + 0x44, Width::Word, (1 << 16) | 12).unwrap();
        b.write(DMA_BASE + 0x48, Width::Word, 0x0100_0201).unwrap();
        // Manual FIFO writes merge into one upload while contiguous.
        spu(&mut b, 0x1A6, 0x2000 >> 3);
        for v in [1, 2, 3, 4] {
            spu(&mut b, 0x1A8, v);
        }
        let voice = 0x10; // voice 1
        spu(&mut b, voice, 0x3FFF);
        spu(&mut b, voice + 2, 0x8000);
        spu(&mut b, voice + 4, 0x1000);
        spu(&mut b, voice + 6, 0x1000 >> 3);
        spu(&mut b, voice + 8, 0x00FF);
        // Voice 3 plays the same sample but was keyed while not tracing.
        spu(&mut b, 0x34, 0x1000);
        spu(&mut b, 0x36, 0x1000 >> 3);
        let log = b.obs.spu.take();
        spu(&mut b, 0x188, 1 << 3);
        b.obs.spu = log;
        spu(&mut b, 0x188, 1 << 1);
        b.run_devices(200_000);
        spu(&mut b, 0x18C, 1 << 1);
        b.run_devices(200_000);

        let log = b.obs.spu.as_ref().unwrap();
        let kinds: Vec<_> = log.events.iter().map(|e| (e.event, e.voice)).collect();
        assert_eq!(
            kinds,
            [("upload", None), ("upload", None), ("key_on", Some(1)), ("end", Some(1)), ("key_off", Some(1))]
        );
        let dma = log.events[0].upload.as_ref().unwrap();
        assert_eq!((dma.address, dma.bytes, dma.source, dma.ram), (0x1000, 48, "dma", Some(0x8010_0000)));
        let cpu = log.events[1].upload.as_ref().unwrap();
        assert_eq!((cpu.address, cpu.bytes, cpu.source, cpu.ram), (0x2000, 8, "cpu", None));
        let on = log.events[2].key_on.as_ref().unwrap();
        assert_eq!((on.start, on.pitch, on.rate_hz, on.sample_bytes, on.looped), (0x1000, 0x1000, 44_100.0, 48, false));
        assert_eq!(on.volume, [Some(0x3FFF as f64 * 2.0 / 32768.0), None]);
        assert_eq!(on.sample_sha256, psx_assets::adpcm::sample_sha256(&sample, 0));
        assert_eq!(log.counts["upload"], 2);
        assert_eq!(b.spu.endx() & (1 << 3), 1 << 3, "voice 3 reached END without a report");
    }

    #[test]
    fn otc_dma_builds_linked_list() {
        let mut b = Bus::new(vec![]);
        b.write(DMA_BASE + 0x60, Width::Word, 0x8010_0010).unwrap();
        b.write(DMA_BASE + 0x64, Width::Word, 4).unwrap();
        b.write(DMA_BASE + 0x68, Width::Word, 0x1100_0002).unwrap();
        let w = |b: &mut Bus, a| b.read(a, Width::Word).unwrap();
        assert_eq!(w(&mut b, 0x8010_0010), 0x0010_000C);
        assert_eq!(w(&mut b, 0x8010_0004), 0x00FF_FFFF);
        assert_eq!(w(&mut b, DMA_BASE + 0x68) & (1 << 24), 0, "busy cleared");
    }

    #[test]
    fn dma_completion_sets_dicr_flag_and_irq() {
        let mut b = Bus::new(vec![]);
        b.write(I_MASK, Width::Word, 1 << 3).unwrap();
        b.write(DICR, Width::Word, (1 << 23) | (1 << 22)).unwrap(); // master + ch6 enable
        b.write(DMA_BASE + 0x60, Width::Word, 0x8010_0010).unwrap();
        b.write(DMA_BASE + 0x64, Width::Word, 4).unwrap();
        b.write(DMA_BASE + 0x68, Width::Word, 0x1100_0002).unwrap();
        let dicr = b.read(DICR, Width::Word).unwrap();
        assert_ne!(dicr & (1 << 30), 0, "ch6 flag");
        assert_ne!(dicr & (1 << 31), 0, "master flag");
        assert!(b.irq_pending());
        b.write(DICR, Width::Word, (1 << 23) | (1 << 22) | (1 << 30)).unwrap();
        assert_eq!(b.read(DICR, Width::Word).unwrap() >> 30, 0, "flag and master cleared");
    }

    #[test]
    fn i_stat_write_acknowledges() {
        let mut b = Bus::new(vec![]);
        b.poke_io(I_STAT, Width::Word, 0b101);
        b.write(I_MASK, Width::Word, 0b001).unwrap();
        assert!(b.irq_pending());
        b.write(I_STAT, Width::Word, !0b001).unwrap();
        assert!(!b.irq_pending());
        assert_eq!(b.read(I_STAT, Width::Word).unwrap(), 0b100);
    }

    #[test]
    fn ram_mirrors_scratch_and_bios() {
        let mut bios = vec![0u8; 16];
        bios[0..4].copy_from_slice(&0x3C08_0013u32.to_le_bytes());
        let mut b = Bus::new(bios);
        b.write(0x8000_0010, Width::Word, 0xDEAD_BEEF).unwrap();
        assert_eq!(b.read(0xA020_0010, Width::Word).unwrap(), 0xDEAD_BEEF, "2 MiB mirror via KSEG1");
        assert_eq!(b.read(0x0000_0012, Width::Half).unwrap(), 0xDEAD);
        assert_eq!(b.read(0xBFC0_0000, Width::Word).unwrap(), 0x3C08_0013);
        b.write(0x1F80_0004, Width::Byte, 0x5A).unwrap();
        assert_eq!(b.read(0x9F80_0004, Width::Byte).unwrap(), 0x5A);
        assert!(b.read(0xBF80_0004, Width::Byte).is_err(), "no scratchpad through KSEG1");
        b.log_io = true;
        b.write(0x1F80_1074, Width::Word, 7).unwrap();
        assert_eq!(b.take_io_log().len(), 1);
    }
}

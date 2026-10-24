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
    /// Started transfers with no device model (their RAM/device effects are missing).
    #[serde(skip)]
    pub unmodelled_dma: Vec<UnmodelledDma>,
    /// The DMA controller (Mednafen's).
    pub dma: crate::dma::Dma,
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
    /// The GPU's interrupt line (GP0(1Fh)), for its rising edge.
    gpu_irq: bool,
    /// When the GPU was last updated and its next event (Mednafen's
    /// `GPU_Update`: drawing time, the command FIFO and the video timing).
    gpu_ts: u64,
    gpu_next: u64,
    /// The GPU event from which idle GPU and DMA updates are being skipped
    /// (see [`Bus::wake`]); the GPU's next event is then its phase's end.
    lazy: Option<u64>,
    /// The root counters (Mednafen's `timer.cpp`) and their next event.
    pub timers: crate::timers::Timers,
    timer_next: u64,
    /// System clock cycles elapsed, as far as devices have been advanced.
    pub now: u64,
    /// CPU cycles not yet handed to devices.
    pending: u64,
    /// When devices next need to run: the next scheduled DMA update or GPU
    /// event, whichever is first (0: at once).
    #[serde(skip)]
    next_event: u64,
    /// A vblank happened: Mednafen's frame ends after this instruction, and
    /// its DMA catches up there ([`Bus::end_instruction`]).
    #[serde(skip)]
    frame_ended: bool,
    /// [`Bus::irq_line`]'s cache, recomputed after a restore.
    #[serde(skip)]
    irq_cache: bool,
    #[serde(skip)]
    irq_dirty: bool,
    /// Scanlines, fields and vblank (Mednafen's GPU timing).
    pub video: crate::video::Video,
    /// Completed video frames (vblanks).
    pub frames: u64,
    pub cdrom: crate::cdrom::Cdrom,
    cdrom_irq: bool,
    /// When the CD-ROM controller next needs an update (its own event, as in Mednafen).
    #[serde(skip)]
    cd_next: u64,
    /// For [`crate::Machine`]'s run loop: CPU time before which no device
    /// event is due ([`Bus::next_deadline`]); 0 after an access outside
    /// RAM, BIOS and scratchpad, which may have changed any device.
    #[serde(skip)]
    pub(crate) deadline: u64,
    /// Counts what could make a polling loop's next pass differ from its
    /// last (see `Machine::skip_spin`): events, interrupts, CPU stores and
    /// GTE operations, and I/O accesses other than pure reads and the CD-ROM
    /// index write.
    #[serde(skip)]
    pub(crate) spin_breaks: u64,
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

/// The scratchpad offset of a virtual address that reaches it (not through KSEG1).
#[inline(always)]
pub fn scratch_offset(addr: u32) -> Option<usize> {
    let p = physical(addr);
    (p & !0x3FF == 0x1F80_0000 && addr >> 29 != 5).then_some((p & 0x3FF) as usize)
}

impl Bus {
    pub fn new(bios: Vec<u8>) -> Self {
        let mut b = bios;
        b.resize(BIOS_SIZE, 0);
        let video = crate::video::Video::new(crate::video::bios_is_pal(&b));
        let cdrom = crate::cdrom::Cdrom::new(None, crate::cdrom::bios_scex(&b));
        Bus {
            ram: vec![0; RAM_SIZE],
            scratch: [0; SCRATCH_SIZE],
            bios: b,
            io: vec![0; 0x2000],
            cache_control: 0,
            io_log: Vec::new(),
            log_io: false,
            unmodelled_dma: Vec::new(),
            recent_dma: Default::default(),
            dma: Default::default(),
            next_event: 0,
            frame_ended: false,
            gpu: psx_gpu::Gpu::timed(),
            last_draw_frame: 0,
            draws_seen: 0,
            gpu_irq: false,
            gpu_ts: 0,
            gpu_next: 0,
            lazy: None,
            timers: Default::default(),
            timer_next: 1024,
            now: 0,
            pending: 0,
            irq_cache: false,
            irq_dirty: true,
            video,
            frames: 0,
            cdrom,
            cdrom_irq: false,
            cd_next: 0,
            deadline: 0,
            spin_breaks: 0,
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

    /// System clocks per video field in the current display mode, on average.
    pub fn frame_cycles(&self) -> u64 {
        self.video.field_cycles(self.gpu.display.mode)
    }

    pub(crate) fn raise_irq(&mut self, bits: u32) {
        if bits != 0 {
            self.spin_breaks += 1;
            let v = self.io_word(I_STAT) | bits;
            self.poke_io(I_STAT, Width::Word, v);
        }
    }

    /// Account `cycles` of CPU time. Devices catch up at their next event
    /// (Mednafen's: the 128-clock DMA grid and the video line phases) and
    /// always before any I/O access, so software never observes stale device
    /// state; other devices' interrupts can be up to 128 clocks late.
    #[inline]
    pub fn tick(&mut self, cycles: u64) {
        self.pending += cycles;
        if self.now + self.pending >= self.next_event {
            self.flush();
        }
    }

    /// After each instruction: when a frame ended during it, Mednafen's
    /// `Emulate` returns and brings the DMA up to date.
    #[inline]
    pub fn end_instruction(&mut self) {
        let t = self.time();
        if t >= self.cd_next.min(self.gpu_next).min(self.timer_next).min(self.dma_next()).min(self.sio0.next) {
            self.events(t);
        }
        if self.frame_ended {
            self.frame_ended = false;
            self.flush();
            let now = self.now;
            self.wake(now + 1);
            self.gpu_next = self.gpu_update(now);
            self.cd_update(now);
            self.timer_update(now);
            self.dma_update(now);
            self.sio_update(now);
            self.schedule();
        }
    }

    /// Mednafen's event check before an access outside RAM and the BIOS, and
    /// at each instruction: the GPU's, the CD-ROM controller's, the timers'
    /// and the DMA's events due by `t`, in time order (and in that order at
    /// the same time, as Mednafen's event list starts).
    #[inline]
    pub fn events(&mut self, t: u64) {
        loop {
            let (g, c, tm, d, s) = (self.gpu_next, self.cd_next, self.timer_next, self.dma_next(), self.sio0.next);
            let e = g.min(c).min(tm).min(d).min(s);
            if e > t {
                break;
            }
            self.spin_breaks += 1;
            if g == e {
                if self.lazy.is_some() {
                    self.wake(g);
                    continue;
                }
                self.gpu_next = self.gpu_update(g);
                self.maybe_idle(g);
            } else if c == e {
                self.cd_update(c);
            } else if tm == e {
                self.timer_update(tm);
            } else if d == e {
                self.dma_update(d);
            } else {
                self.sio_update(s);
            }
        }
    }

    /// Bring the controller port to `t` (Mednafen's `FrontIO::Update`).
    fn sio_update(&mut self, t: u64) {
        self.sio0.next = self.sio0.update(t);
        self.after_sio();
    }

    fn after_sio(&mut self) {
        if self.sio0.take_irq() {
            self.raise_irq(1 << 7);
        }
    }

    /// Bring the timers to `t` (Mednafen's `TIMER_Update`).
    fn timer_update(&mut self, t: u64) {
        self.timer_next = self.timers.update(t);
        self.after_timers();
    }

    fn after_timers(&mut self) {
        let irq = self.timers.take_irq();
        self.raise_irq(irq);
    }

    /// The DMA's next event: the 128-clock grid point after its last update.
    #[inline]
    fn dma_next(&self) -> u64 {
        if self.lazy.is_some() { u64::MAX } else { self.dma.next_update(self.dma.last) }
    }

    /// Mednafen updates the GPU and the DMA at least every 128 clocks. While
    /// the GPU, DMA and MDEC are idle and timer 0 does not count dots, those
    /// updates only add saturated drawing time, bank DMA and MDEC clocks and
    /// advance the video counters within a line phase, which come out the
    /// same in one update as in many. So after an idle GPU event at `t` they
    /// are skipped up to the phase's end (still an event), and [`Bus::wake`]
    /// replays them before anything could tell: an access to the GPU, DMA,
    /// MDEC or timers.
    fn maybe_idle(&mut self, t: u64) {
        if self.gpu_ts == t && self.gpu.idle() && self.dma_idle() && !self.timers.counts_dots() {
            self.lazy = Some(t);
            self.gpu_next = t + self.video.clocks_to_phase_end();
            self.schedule();
        }
    }

    /// Replay the skipped updates due before `t` as Mednafen makes them (the
    /// DMA's at multiples of 128, the GPU's every 128 clocks from the event
    /// it went idle at) and schedule the next ones again.
    pub(crate) fn wake(&mut self, t: u64) {
        let Some(c0) = self.lazy.take() else { return };
        let phase_end = self.gpu_next;
        // The phase's end stays an event of its own.
        let t = t.min(phase_end);
        let gpu_last = if t > c0 { c0 + (t - 1 - c0) / 128 * 128 } else { c0 };
        let dma_last = t.saturating_sub(1) / 128 * 128;
        if gpu_last < dma_last {
            self.gpu_update(gpu_last);
        }
        if dma_last > self.dma.last {
            self.dma_update(dma_last);
        }
        self.gpu_update(gpu_last);
        self.gpu_next = (gpu_last + 128).min(phase_end);
        self.schedule();
    }

    /// Bring the GPU to `t` (Mednafen's `GPU_Update`): credit drawing time
    /// and run its FIFO once, then the video timing. Returns its next event:
    /// at most 128 clocks away, or the end of the line phase.
    pub(crate) fn gpu_update(&mut self, t: u64) -> u64 {
        if t > self.gpu_ts {
            let clocks = t - self.gpu_ts;
            self.gpu_ts = t;
            self.gpu.credit(clocks);
            self.after_gpu();
            let video = self.video.tick(clocks, t, &self.gpu.display, &mut self.timers);
            if let Some(next) = video.timer_next {
                self.timer_next = next;
            }
            self.after_timers();
            self.gpu.readout_field = self.video.readout_field;
            if video.vblanks > 0 {
                self.frames += video.vblanks;
                if self.gpu.draws != self.draws_seen {
                    self.draws_seen = self.gpu.draws;
                    self.last_draw_frame = self.frames;
                }
                self.raise_irq(1);
                self.frame_ended = true;
            }
        }
        t + self.video.clocks_to_phase_end().min(128)
    }

    /// After the GPU ran commands: GP0(1Fh)'s interrupt, on its rising edge.
    pub(crate) fn after_gpu(&mut self) {
        let irq = self.gpu.irq;
        if irq && !self.gpu_irq {
            self.raise_irq(1 << 1);
        }
        self.gpu_irq = irq;
    }

    /// Bring the CD-ROM controller (which clocks the SPU) to `t`.
    fn cd_update(&mut self, t: u64) {
        let spu_irq = self.cdrom.update(t, &mut self.spu);
        self.after_cd(spu_irq);
    }

    fn after_cd(&mut self, spu_irq: bool) {
        if spu_irq {
            self.raise_irq(1 << 9);
        }
        let cd = self.cdrom.irq();
        if cd && !self.cdrom_irq {
            self.raise_irq(1 << 2);
        }
        self.cdrom_irq = cd;
        self.cd_next = self.cdrom.next_event();
        if let Some(log) = &mut self.obs.spu {
            let ended = self.spu.endx() & log.awaiting_end;
            for voice in (0..24).filter(|v| ended & (1 << v) != 0) {
                log.awaiting_end &= !(1 << voice);
                log.record(crate::observe::SpuEvent {
                    frame: self.frames,
                    cycle: self.cdrom.time(),
                    event: "end",
                    voice: Some(voice),
                    pc: None,
                    key_on: None,
                    upload: None,
                });
            }
        }
    }

    /// Power on a new drive holding `disc` (Mednafen `Power` then `SetDisc`).
    pub fn insert_disc(&mut self, disc: Option<psx_media::Disc>) {
        self.cdrom = crate::cdrom::Cdrom::new(disc, crate::cdrom::bios_scex(&self.bios));
        self.cdrom.set_time(self.time());
        self.cdrom_irq = false;
        self.cd_next = self.cdrom.next_event();
    }

    /// The earliest of the next device flush and the devices' next events.
    pub(crate) fn next_deadline(&self) -> u64 {
        self.next_event
            .min(self.cd_next)
            .min(self.gpu_next)
            .min(self.timer_next)
            .min(self.dma_next())
            .min(self.sio0.next)
    }

    fn schedule(&mut self) {
        self.next_event =
            if self.lazy.is_some() { self.gpu_next } else { self.dma.next_update(self.now).min(self.gpu_next) };
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
        self.schedule();
    }

    /// GPUSTAT: the GPU's state (Mednafen's `GPU_Read`), display state, and
    /// the field and line parity from the video timing as of the GPU's last
    /// update.
    fn gpustat(&self) -> u32 {
        let d = &self.gpu.display;
        let st = &self.gpu.state;
        let mut v = (st.texpage as u32 & 0x7FF) | ((st.set_mask as u32) << 11) | ((st.check_mask as u32) << 12);
        v |= (d.mode & 0x3F) << 17 | ((d.mode >> 6) & 1) << 16 | ((d.mode >> 7) & 1) << 14;
        v |= ((!d.enabled) as u32) << 23;
        v |= self.gpu.status_bits();
        // Mednafen: bit 31 is the parity of the VRAM line being read out, and
        // bit 13 the inverse of the interlaced field.
        v |= (self.video.cur_line_readout & 1) << 31;
        v |= (!self.video.field as u32) << 13;
        v
    }

    /// Current scanline within the field and whether it is in vblank.
    pub fn scanline(&self) -> (u64, bool) {
        (self.video.scanline as u64, self.video.in_vblank)
    }

    /// A channel was started (CHCR bit 24 set): analysis hooks only; the
    /// transfer itself runs in [`crate::dma`].
    pub(crate) fn dma_started(&mut self, ch: usize) {
        let c = self.dma.ch[ch];
        let (madr, bcr, chcr) = (c.base_addr, c.block_control, c.chan_control);
        let ch = ch as u32;
        if self.recent_dma.len() >= 32 {
            self.recent_dma.pop_front();
        }
        self.recent_dma.push_back(UnmodelledDma { channel: ch, madr, bcr, chcr });
        let words = if (chcr >> 9) & 3 == 0 {
            if bcr & 0xFFFF == 0 { 0x10000 } else { bcr & 0xFFFF }
        } else {
            (bcr & 0xFFFF) * (bcr >> 16).max(1)
        };
        match ch {
            6 => self.observe_dma(6, madr.wrapping_sub(4 * (words - 1)) & 0x1F_FFFC, words * 4),
            1 => self.observe_dma(1, madr & 0x1F_FFFC, words * 4),
            2 if chcr & 1 == 0 && (chcr >> 9) & 3 != 2 => self.observe_dma(2, madr & 0x1F_FFFC, words * 4),
            4 => {
                if chcr & 1 == 0 {
                    self.observe_dma(4, madr & 0x1F_FFFC, words * 4);
                } else if let Some(log) = &mut self.obs.spu {
                    let upload = crate::observe::SpuUpload {
                        address: self.spu.transfer_addr(),
                        bytes: words * 4,
                        source: "dma",
                        ram: Some(0x8000_0000 | (madr & 0x1F_FFFC)),
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
            3 => {
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
            }
            5 => self.unmodelled_dma.push(UnmodelledDma { channel: ch, madr, bcr, chcr }),
            _ => {}
        }
    }

    pub fn io_word(&self, phys: u32) -> u32 {
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
        self.next_event = 0;
        self.cd_next = self.cdrom.next_event();
        self.gpu.timed = true;
        // Draw tracking is not saved: count idle frames from the restore.
        self.last_draw_frame = self.frames;
        self.draws_seen = self.gpu.draws;
    }

    fn log_access(&mut self, a: IoAccess) {
        if self.log_io {
            self.io_log.push(a);
        }
    }

    pub fn take_io_log(&mut self) -> Vec<IoAccess> {
        std::mem::take(&mut self.io_log)
    }

    /// Wait states of a CPU read, Mednafen's `MemRW`: `a` is the address
    /// after Mednafen's segment masking and `bytes` the access size (3 for
    /// the 24-bit halves of LWL/LWR). The CPU adds its own 2 cycles (1 for
    /// LWCz) on top.
    pub fn read_cost(a: u32, bytes: u32) -> u64 {
        match a {
            0..=0x007F_FFFF => 3,
            0x1F80_1C00..=0x1F80_1FFF => {
                if bytes == 4 {
                    36
                } else {
                    16
                }
            }
            0x1F80_1800..=0x1F80_180F => 6 * bytes.next_power_of_two() as u64,
            0x1F80_1000..=0x1F80_1023
            | 0x1F80_1040..=0x1F80_105F
            | 0x1F80_1070..=0x1F80_1077
            | 0x1F80_1080..=0x1F80_113F
            | 0x1F80_1810..=0x1F80_1817
            | 0x1F80_1820..=0x1F80_1827 => 1,
            _ => 0,
        }
    }

    /// Whether Mednafen's `MemRW` runs due events before an access to the
    /// physical (segment-masked) address `a`: anything but RAM and the BIOS.
    #[inline]
    pub fn checks_events(a: u32) -> bool {
        !(a < 0x0080_0000 || (0x1FC0_0000..=0x1FC7_FFFF).contains(&a))
    }

    /// True when a virtual address decodes to the I/O region.
    pub fn is_io(addr: u32) -> bool {
        let p = physical(addr);
        (0x1F80_1000..0x1F80_3000).contains(&p)
    }

    /// An aligned read at a physical RAM address (below 8 MiB, mirrored).
    #[inline(always)]
    pub(crate) fn read_ram(&self, p: u32, width: Width) -> u32 {
        let o = p as usize % RAM_SIZE;
        match width {
            Width::Word => self.ram_u32(o),
            Width::Half => u16::from_le_bytes([self.ram[o], self.ram[o + 1]]) as u32,
            Width::Byte => self.ram[o] as u32,
        }
    }

    /// Word at a RAM offset (`o` is already reduced modulo [`RAM_SIZE`]).
    #[inline(always)]
    fn ram_u32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.ram[o..o + 4].try_into().unwrap())
    }

    /// A scratchpad access at offset `o` (from [`scratch_offset`]); a
    /// misaligned one wraps within the 1 KiB.
    #[inline(always)]
    pub fn read_scratch(&self, o: usize, width: Width) -> u32 {
        let s = &self.scratch;
        match width {
            Width::Word if o <= SCRATCH_SIZE - 4 => u32::from_le_bytes(s[o..o + 4].try_into().unwrap()),
            Width::Half if o <= SCRATCH_SIZE - 2 => u16::from_le_bytes([s[o], s[o + 1]]) as u32,
            _ => (0..width as usize).rev().fold(0, |v, i| v << 8 | s[(o + i) % SCRATCH_SIZE] as u32),
        }
    }

    #[inline(always)]
    pub fn write_scratch(&mut self, o: usize, width: Width, value: u32) {
        let s = &mut self.scratch;
        match width {
            Width::Word if o <= SCRATCH_SIZE - 4 => s[o..o + 4].copy_from_slice(&value.to_le_bytes()),
            Width::Half if o <= SCRATCH_SIZE - 2 => s[o..o + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            _ => (0..width as usize).for_each(|i| s[(o + i) % SCRATCH_SIZE] = (value >> (8 * i)) as u8),
        }
    }

    #[inline(always)]
    pub fn read(&mut self, addr: u32, width: Width) -> Result<u32, BusError> {
        let p = physical(addr);
        // Fast paths: RAM and BIOS (the overwhelmingly common cases), inlined
        // into the CPU; everything else goes through the out-of-line decoder.
        if p < 0x0080_0000 {
            return Ok(self.read_ram(p, width));
        }
        if width == Width::Word && (0x1FC0_0000..0x1FC8_0000).contains(&p) {
            let o = (p - 0x1FC0_0000) as usize;
            return Ok(u32::from_le_bytes(self.bios[o..o + 4].try_into().unwrap()));
        }
        if let Some(o) = scratch_offset(addr) {
            return Ok(self.read_scratch(o, width));
        }
        self.read_slow(addr, p, width)
    }

    /// Account `cycles` of skipped loop passes (see `Machine::skip_spin`),
    /// as CPU time handed over like any other.
    pub(crate) fn skip_cycles(&mut self, cycles: u64) {
        self.tick(cycles);
    }

    /// What a polling loop's pass depends on in the devices besides their
    /// state, which is still between events: the CD-ROM register index and
    /// the CPU time not yet handed over.
    pub(crate) fn spin_key(&self) -> (u8, u64) {
        (self.cdrom.reg_selector(), self.pending)
    }

    /// Before a write to the GPU, DMA, MDEC or timers: the skipped updates
    /// due by now, as the events Mednafen runs before an access. Reads see
    /// nothing the skipped updates would change (the GPU's and MDEC's state
    /// is idle, the DMA's registers still, and the video's readout line and
    /// field and timer 1's retrace counts change only at phase ends).
    #[inline(always)]
    fn wake_for(&mut self, p: u32) {
        if self.lazy.is_some() && matches!(p, DMA_BASE..=0x1F80_10FF | 0x1F80_1100..=0x1F80_113F | GP0..=0x1F80_1827) {
            self.wake(self.time() + 1);
        }
    }

    #[inline(never)]
    fn read_slow(&mut self, addr: u32, p: u32, width: Width) -> Result<u32, BusError> {
        self.deadline = 0;
        // Reads that change nothing and return the same until an event:
        // GPUSTAT, CD-ROM status and interrupt flags, the interrupt and DMA
        // registers.
        if !matches!(p, 0x1F80_1814..=0x1F80_1817 | I_STAT..=0x1F80_10FF)
            && !(matches!(p, 0x1F80_1800..=0x1F80_180F) && matches!(p & 3, 0 | 3))
        {
            self.spin_breaks += 1;
        }
        if (0x1F80_1000..0x1F80_3000).contains(&p) {
            self.flush();
        }
        let n = width as usize;
        let le = |b: &[u8], o: usize| -> u32 { b[o..o + n].iter().rev().fold(0u32, |acc, &x| (acc << 8) | x as u32) };
        Ok(match p {
            0x0000_0000..=0x007F_FFFF => le(&self.ram, p as usize % RAM_SIZE),
            0x1F00_0000..=0x1F7F_FFFF => u32::MAX >> (32 - 8 * n),
            0x1F80_0000..=0x1F80_03FF if addr >> 29 != 5 => le(&self.scratch, (p - 0x1F80_0000) as usize),
            // Mednafen's `GPU_Read`: either register at any width, shifted down by the byte offset.
            GP0..=0x1F80_1817 => {
                let v = if p & 4 != 0 { self.gpustat() } else { self.gpu.read_data() };
                let v = (v >> (8 * (p & 3))) & (u32::MAX >> (32 - 8 * n));
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1100..=0x1F80_113F => {
                let now = self.now;
                let v = self.timers.read(now, p - TIMERS);
                self.after_timers();
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1820..=0x1F80_1827 => {
                let v = self.mdec.read(p) & (u32::MAX >> (32 - 8 * n));
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1040..=0x1F80_104F => {
                let now = self.now;
                let v = self.sio0.read(now, p - 0x1F80_1040) & (u32::MAX >> (32 - 8 * n));
                self.after_sio();
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            // Mednafen: one 8-bit register read whatever the width, mirrored every 4 bytes.
            0x1F80_1800..=0x1F80_180F => {
                let v = self.cdrom.read(p & 3) as u32;
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            // Mednafen: a word reads two halfwords; a narrower read gets the
            // halfword at the even address, cut to its width.
            0x1F80_1C00..=0x1F80_1FFF => {
                let o = (p & !1) - 0x1F80_1C00;
                let v = if width == Width::Word {
                    self.spu.read16(o) as u32 | (self.spu.read16(o | 2) as u32) << 16
                } else {
                    self.spu.read16(o) as u32 & (u32::MAX >> (32 - 8 * n))
                };
                if self.spu.take_irq() {
                    self.raise_irq(1 << 9);
                }
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            // Mednafen's `IRQ_Read`: the high bits read as 1F80h.
            I_STAT..=0x1F80_1077 => {
                let v = ((self.io_word(p & !3) | 0x1F80_0000) >> (8 * (p & 3))) & (u32::MAX >> (32 - 8 * n));
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            DMA_BASE..=0x1F80_10FF => {
                let v = self.dma.read(p) & (u32::MAX >> (32 - 8 * n));
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1000..=0x1F80_2FFF => {
                let v = match p {
                    // Memory control (Mednafen's `SysControl`): 1F801020h aliases register 0.
                    0x1F80_1000..=0x1F80_1023 => {
                        let i = (p & 0x1F) >> 2;
                        let or = if i < 2 { 0x1F00_0000 } else { 0 };
                        (self.io_word(0x1F80_1000 + 4 * i) | or) >> (8 * (p & 3))
                    }
                    // SIO1, Mednafen's dummy `sio.cpp`: data, status (always 0), mode,
                    // control and baud rate, each read at its halfword.
                    0x1F80_1050..=0x1F80_105F => {
                        let r = match p & 0xE {
                            0x0 => self.io_word(0x1F80_1050),
                            0x8 | 0xA | 0xE => self.io_word(0x1F80_1050 | (p & 0xE)) & 0xFFFF,
                            _ => 0,
                        };
                        r >> (8 * (p & 1))
                    }
                    // Anything else Mednafen doesn't decode reads as 0 (RAM_SIZE at 1F801060h too).
                    _ => {
                        *self.unhandled_reads.entry(p).or_default() += 1;
                        0
                    }
                } & (u32::MAX >> (32 - 8 * n));
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
        if let Some(o) = scratch_offset(addr) {
            self.write_scratch(o, width, value);
            return Ok(());
        }
        self.write_slow(addr, p, width, value)
    }

    #[inline(never)]
    fn write_slow(&mut self, addr: u32, p: u32, width: Width, value: u32) -> Result<(), BusError> {
        self.deadline = 0;
        // The CD-ROM index write only selects a register (the value is in
        // the polling loop's key).
        if !(matches!(p, 0x1F80_1800..=0x1F80_180F) && p & 3 == 0) {
            self.spin_breaks += 1;
        }
        self.wake_for(p);
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
                    // Mednafen's `SPU_Write` takes 16 bits, whatever the width.
                    let mut fired = self.spu_write((p & !1) - 0x1F80_1C00, value as u16);
                    if width == Width::Word {
                        fired |= self.spu_write((p & !1) + 2 - 0x1F80_1C00, (value >> 16) as u16);
                    }
                    if fired {
                        self.raise_irq(1 << 9);
                    }
                } else if (0x1F80_1820..0x1F80_1828).contains(&p) {
                    self.mdec.write(p, value);
                } else if (0x1F80_1040..0x1F80_1050).contains(&p) {
                    let now = self.now;
                    self.sio0.write(now, p - 0x1F80_1040, value);
                    self.after_sio();
                } else if (0x1F80_1800..0x1F80_1810).contains(&p) {
                    let now = self.now;
                    let fired = self.cdrom.write(p & 3, value as u8, now, Some(self.frames), &mut self.spu);
                    self.after_cd(fired);
                } else if (TIMERS..TIMERS + 0x40).contains(&p) {
                    let now = self.now;
                    self.timer_next = self.timers.write(now, p - TIMERS, value);
                    self.after_timers();
                } else if (GP0..GP0 + 8).contains(&p) {
                    // Mednafen's `GPU_Write`: the whole value, shifted up by the byte offset.
                    let v = value << (8 * (p & 3));
                    if p & 4 != 0 {
                        self.gpu.gp1(v);
                    } else {
                        self.gpu.gp0(v);
                    }
                    self.after_gpu();
                } else if (DMA_BASE..TIMERS).contains(&p) {
                    let now = self.now;
                    self.dma_write(now, p, value);
                } else if p & !3 == I_STAT {
                    // Interrupt acknowledge (Mednafen's `IRQ_Write`): the whole value, shifted
                    // up by the byte offset, is ANDed into the 16-bit status.
                    let v = self.io_word(I_STAT) & (value << (8 * (p & 3))) & 0xFFFF;
                    self.poke_io(I_STAT, Width::Word, v);
                } else if p & !3 == I_MASK {
                    self.poke_io(I_MASK, Width::Word, (value << (8 * (p & 3))) & 0xFFFF);
                } else if p < 0x1F80_1024 {
                    // Memory control: the whole value, shifted and masked (Mednafen's `SysControl_Mask`).
                    const MASK: [u32; 8] = [
                        0x00FF_FFFF,
                        0x00FF_FFFF,
                        0xFFFF_FFFF,
                        0x2F1F_FFFF,
                        0xFFFF_FFFF,
                        0x2F1F_FFFF,
                        0x2F1F_FFFF,
                        0xFFFF_FFFF,
                    ];
                    let i = (p & 0x1F) >> 2;
                    self.poke_io(0x1F80_1000 + 4 * i, Width::Word, (value << (8 * (p & 3))) & MASK[i as usize]);
                } else if (0x1F80_1050..0x1F80_1060).contains(&p) {
                    // SIO1 (Mednafen's dummy `sio.cpp`): data, mode, control and baud rate latch.
                    let v = value << (8 * (p & 1));
                    match p & 0xE {
                        0x0 => self.poke_io(0x1F80_1050, Width::Word, v),
                        0x8 | 0xA | 0xE => self.poke_io(0x1F80_1050 | (p & 0xE), Width::Half, v),
                        _ => {}
                    }
                } else {
                    put(&mut self.io, (p - 0x1F80_1000) as usize);
                }
                self.log_access(IoAccess { addr: p, width, write: true, value });
            }
            0x1FC0_0000..=0x1FC7_FFFF => {}
            // Bits 6 and 10 don't stick (Mednafen's SetBIU).
            0xFFFE_0130 => self.cache_control = value & !0x440,
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
        // Run it and acknowledge its INT3, so the next one can be answered.
        m.bus.events(10_100_000);
        for (reg, v) in [(0, 1), (3, 0x1F), (0, 0)] {
            m.bus.write(0x1f801800 + reg, crate::Width::Byte, v).unwrap();
        }
        m.bus.frames = 43;
        m.bus.now = 11_000_000;
        m.bus.gpu.display.mode = 0;
        m.bus.write(0x1f801801, crate::Width::Byte, 1).unwrap();
        // The repeat is folded in once its response is known.
        m.bus.events(11_100_000);
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
        // The transfer takes 48 clocks a word (Mednafen's DMA timing); the
        // DMA runs at the event check after an instruction.
        b.tick(12 * 48);
        b.end_instruction();
        assert_eq!(b.read(DMA_BASE + 0x48, Width::Word).unwrap() & (1 << 24), 0, "DMA done");
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
        // Key on takes effect at the next sample (a second write before it
        // would replace this one).
        b.tick(2000);
        b.end_instruction();
        b.obs.spu = log;
        spu(&mut b, 0x188, 1 << 1);
        // The CD-ROM controller clocks the SPU.
        let advance = |b: &mut Bus, n: u64| {
            b.tick(n);
            b.end_instruction();
        };
        advance(&mut b, 200_000);
        spu(&mut b, 0x18C, 1 << 1);
        advance(&mut b, 200_000);

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
    fn long_otc_dma_holds_the_cpu_until_the_grid() {
        let mut b = Bus::new(vec![]);
        b.write(DMA_BASE + 0x60, Width::Word, 0x8010_1000).unwrap();
        b.write(DMA_BASE + 0x64, Width::Word, 1000).unwrap();
        b.write(DMA_BASE + 0x68, Width::Word, 0x1100_0002).unwrap();
        assert!(b.dma.halted, "64 of 1000 words done at the start");
        b.tick(1000);
        b.end_instruction();
        assert!(b.dma.halted);
        b.tick(24);
        b.end_instruction();
        assert!(!b.dma.halted, "done at the 1024-clock update");
        assert_eq!(b.read(DMA_BASE + 0x68, Width::Word).unwrap() & (1 << 24), 0, "busy cleared");
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
        assert_eq!(b.read(I_STAT, Width::Word).unwrap(), 0x1F80_0000 | 0b100);
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

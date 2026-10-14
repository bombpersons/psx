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

pub struct Bus {
    pub ram: Vec<u8>,
    pub scratch: [u8; SCRATCH_SIZE],
    pub bios: Vec<u8>,
    /// Physical 0x1F801000..0x1F803000 as a register file.
    pub io: Vec<u8>,
    pub cache_control: u32,
    /// I/O accesses since the last [`Bus::take_io_log`] (only when `log_io` is set).
    io_log: Vec<IoAccess>,
    pub log_io: bool,
    /// Set when software writes I_STAT; cleared by [`Bus::take_irq_ack`].
    irq_acknowledged: bool,
    /// Started transfers with no device model (their RAM/device effects are missing).
    pub unmodelled_dma: Vec<UnmodelledDma>,
    /// MDEC-out (channel 1) transfer waiting for decoder output: (madr, bcr, chcr, words).
    pending_mdec_out: Option<(u32, u32, u32, u32)>,
    /// The most recent DMA transfers (channel, madr, bcr, chcr), newest last.
    pub recent_dma: std::collections::VecDeque<UnmodelledDma>,
    pub gpu: psx_gpu::Gpu,
    /// GP1(00h)/(01h) etc. are not tracked in GPUSTAT yet; this mirrors the
    /// display/DMA bits software set so status reads are plausible.
    gpustat_dma_dir: u32,
    pub timers: crate::timers::Timers,
    /// System clock cycles elapsed.
    pub now: u64,
    next_vblank: u64,
    /// Completed video frames (vblanks).
    pub frames: u64,
    /// When false, devices do not raise interrupts (lockstep supplies them).
    pub irq_sources: bool,
    pub cdrom: crate::cdrom::Cdrom,
    cdrom_irq: bool,
    pub sio0: crate::sio0::Sio0,
    pub mdec: crate::mdec::Mdec,
    /// Reads of I/O registers no device handles yet: physical address -> count.
    pub unhandled_reads: std::collections::BTreeMap<u32, u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BusError {
    /// No device decodes this physical address.
    Unmapped(u32),
}

fn physical(addr: u32) -> u32 {
    if addr >= 0xC000_0000 {
        addr
    } else {
        addr & 0x1FFF_FFFF
    }
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
            gpustat_dma_dir: 0,
            timers: Default::default(),
            now: 0,
            next_vblank: 0,
            frames: 0,
            irq_sources: true,
            cdrom: Default::default(),
            cdrom_irq: false,
            sio0: Default::default(),
            mdec: Default::default(),
            unhandled_reads: Default::default(),
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

    /// Advance devices by `cycles` system clocks.
    pub fn tick(&mut self, cycles: u64) {
        self.now += cycles;
        if self.next_vblank == 0 {
            self.next_vblank = self.frame_cycles();
        }
        let lines = if self.pal() { 314 } else { 263 };
        let hblank = ((1u64 << 16) * lines) / self.frame_cycles();
        // Dot clock: 53.693175 MHz / divider (10, 8, 5, 4 for widths 256, 320, 512, 640; 7 for 368).
        let div = match self.gpu.display.width() {
            256 => 10,
            320 => 8,
            368 => 7,
            512 => 5,
            _ => 4,
        };
        let dotclock = ((53_693_175u64 << 16) / div) / SYSCLK;
        let irq = self.timers.tick(cycles, crate::timers::Rates { dotclock, hblank });
        self.raise_irq(irq);
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
            self.raise_irq(1);
        }
    }

    fn ram_word(&self, addr: u32) -> u32 {
        let o = (addr & 0x1F_FFFC) as usize;
        u32::from_le_bytes([self.ram[o], self.ram[o + 1], self.ram[o + 2], self.ram[o + 3]])
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
            3 => {
                let words = if (chcr >> 9) & 3 == 0 {
                    if bcr & 0xFFFF == 0 { 0x10000 } else { bcr & 0xFFFF }
                } else {
                    (bcr & 0xFFFF) * (bcr >> 16).max(1)
                };
                self.cdrom.note_dma(words * 4);
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

    pub fn take_irq_ack(&mut self) -> bool {
        std::mem::take(&mut self.irq_acknowledged)
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

    pub fn read(&mut self, addr: u32, width: Width) -> Result<u32, BusError> {
        let p = physical(addr);
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
            // SPUSTAT: mode bits 0-5 mirror SPUCNT; never busy (no SPU model yet).
            0x1F80_1DAE => {
                let v = (le(&self.io, 0x1F80_1DAA - 0x1F80_1000) & 0x3F) >> (8 * (p & 1));
                self.log_access(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1000..=0x1F80_2FFF => {
                let v = le(&self.io, (p - 0x1F80_1000) as usize);
                let handled = (0x1F80_1C00..0x1F80_1E00).contains(&p) || (0x1F80_1800..0x1F80_1804).contains(&p) || (0x1F80_1070..0x1F80_1078).contains(&p) || (0x1F80_1080..0x1F80_1100).contains(&p);
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

    pub fn write(&mut self, addr: u32, width: Width, value: u32) -> Result<(), BusError> {
        let p = physical(addr);
        let n = width as usize;
        let put = |b: &mut [u8], o: usize| {
            for i in 0..n {
                b[o + i] = (value >> (8 * i)) as u8;
            }
        };
        match p {
            0x0000_0000..=0x007F_FFFF => put(&mut self.ram, p as usize % RAM_SIZE),
            0x1F00_0000..=0x1F7F_FFFF => {}
            0x1F80_0000..=0x1F80_03FF if addr >> 29 != 5 => put(&mut self.scratch, (p - 0x1F80_0000) as usize),
            0x1F80_1000..=0x1F80_2FFF => {
                if (p == 0x1F80_1820 || p == 0x1F80_1824) && width == Width::Word {
                    self.mdec.write(p - 0x1F80_1820, value);
                    self.try_mdec_out();
                } else if (0x1F80_1040..0x1F80_1050).contains(&p) {
                    let now = self.now;
                    self.sio0.write(p - 0x1F80_1040, value, now);
                } else if (0x1F80_1800..0x1F80_1804).contains(&p) {
                    let now = self.now;
                    self.cdrom.write(p - 0x1F80_1800, value as u8, now);
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
                    let v = if width == Width::Word { value } else { (cur & !(0xFFFF << (8 * (p & 2)))) | (value << (8 * (p & 2))) };
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
        let o = (phys - 0x1F80_1000) as usize;
        for i in 0..width as usize {
            self.io[o + i] = (value >> (8 * i)) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

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
pub const GP0: u32 = 0x1F80_1810;
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
    /// I/O accesses since the last [`Bus::take_io_log`].
    io_log: Vec<IoAccess>,
    /// Set when software writes I_STAT; cleared by [`Bus::take_irq_ack`].
    irq_acknowledged: bool,
    /// Started transfers with no device model (their RAM/device effects are missing).
    pub unmodelled_dma: Vec<UnmodelledDma>,
    pub gpu: psx_gpu::Gpu,
    /// GP1(00h)/(01h) etc. are not tracked in GPUSTAT yet; this mirrors the
    /// display/DMA bits software set so status reads are plausible.
    gpustat_dma_dir: u32,
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
            irq_acknowledged: false,
            unmodelled_dma: Vec::new(),
            gpu: psx_gpu::Gpu::new(),
            gpustat_dma_dir: 0,
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
        v
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
            _ => self.unmodelled_dma.push(UnmodelledDma { channel: ch, madr, bcr, chcr }),
        }
        // Transfer done: clear start/trigger.
        self.poke_io(base + 8, Width::Word, chcr & !((1 << 24) | (1 << 28)));
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
                self.io_log.push(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            GP1 if width == Width::Word => {
                let v = self.gpustat();
                self.io_log.push(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1F80_1000..=0x1F80_2FFF => {
                let v = le(&self.io, (p - 0x1F80_1000) as usize);
                self.io_log.push(IoAccess { addr: p, width, write: false, value: v });
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
                if p == GP0 && width == Width::Word {
                    self.gpu.gp0(value);
                } else if p == GP1 && width == Width::Word {
                    if (value >> 24) & 0x3F == 0x04 {
                        self.gpustat_dma_dir = value & 3;
                    }
                    self.gpu.gp1(value);
                } else if p & !3 >= DMA_BASE && p & !3 < DPCR && (p & 0xF) == 8 {
                    put(&mut self.io, (p - 0x1F80_1000) as usize);
                    self.dma_start((p - DMA_BASE) >> 4);
                } else if p & !3 == I_STAT {
                    // Interrupt acknowledge: writing 0 clears a bit, 1 leaves it.
                    let cur = self.io_word(I_STAT);
                    let v = cur & (value << (8 * (p & 3)) | !(u32::MAX >> (32 - 8 * n) << (8 * (p & 3))));
                    self.poke_io(I_STAT, Width::Word, v);
                    self.irq_acknowledged = true;
                } else {
                    put(&mut self.io, (p - 0x1F80_1000) as usize);
                }
                self.io_log.push(IoAccess { addr: p, width, write: true, value });
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
        b.write(0x1F80_1074, Width::Word, 7).unwrap();
        assert_eq!(b.take_io_log().len(), 1);
    }
}

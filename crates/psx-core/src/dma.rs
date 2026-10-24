//! DMA controller, ported from Mednafen (`crates/psx-oracle/mednafen/src/psx/dma.cpp`).
//!
//! Channels advance on a grid of 128 system clocks (and whenever software
//! touches a DMA register): each word costs a clock, plus per-channel
//! overheads (SPU 47, CD-ROM reads 8, linked-list headers 10 or 15, GPU
//! blocks 7). A started channel first runs 64 clocks at once. While a
//! channel in manual mode (sync 0) has words left, the CPU is held; a GPU
//! block transfer (sync 1) instead slows each CPU data read by the block
//! size less one. The interrupt (DICR) follows Mednafen: a channel's flag is
//! set when its enable is, and the line is the master enable with any flag,
//! or the force bit.

use crate::bus::Bus;

pub const MDEC_IN: usize = 0;
pub const MDEC_OUT: usize = 1;
pub const GPU: usize = 2;
pub const CDROM: usize = 3;
pub const SPU: usize = 4;
pub const OTC: usize = 6;

/// Clocks between scheduled channel updates.
pub const GRID: u64 = 128;

#[derive(Clone, Copy, Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct Channel {
    pub base_addr: u32,
    pub block_control: u32,
    pub chan_control: u32,
    cur_addr: u32,
    word_counter: u16,
    clock_counter: i32,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Dma {
    pub ch: [Channel; 7],
    pub control: u32,
    pub int_control: u32,
    pub int_status: u8,
    irq_out: bool,
    /// Clocks to the next scheduled update (1-128).
    cycle_counter: i32,
    /// When the channels were last advanced.
    pub(crate) last: u64,
    /// A manual-mode transfer holds the CPU.
    pub halted: bool,
    /// Extra clocks on each CPU data read (GPU block transfers).
    pub cycle_steal: u64,
}

impl Default for Dma {
    fn default() -> Self {
        Dma {
            ch: Default::default(),
            control: 0,
            int_control: 0,
            int_status: 0,
            irq_out: false,
            cycle_counter: GRID as i32,
            last: 0,
            halted: false,
            cycle_steal: 0,
        }
    }
}

impl Dma {
    /// The next scheduled update after `now`.
    pub fn next_update(&self, now: u64) -> u64 {
        (now / GRID + 1) * GRID
    }

    pub fn read(&self, a: u32) -> u32 {
        let ch = ((a & 0x7F) >> 4) as usize;
        let v = match (ch, a & 0xC) {
            (7, 0x0) => self.control,
            (7, 0x4) => self.int_control | (self.int_status as u32) << 24 | (self.irq_out as u32) << 31,
            (7, _) => 0,
            (_, 0x0) => self.ch[ch].base_addr,
            (_, 0x4) => self.ch[ch].block_control,
            _ => self.ch[ch].chan_control,
        };
        v >> ((a & 3) * 8)
    }
}

impl Bus {
    /// Whether channel `ch` can move a word now (Mednafen's `ChCan`).
    /// The core's MDEC decodes at once, so MDEC output waits only for a
    /// 32-word block; the GPU's writes wait for its FIFO (GPUSTAT bit 28).
    fn dma_can(&self, ch: usize) -> bool {
        match ch {
            MDEC_OUT => self.mdec.out_available() >= 0x20,
            GPU if self.dma.ch[GPU].chan_control & 1 != 0 => self.gpu.ready(),
            OTC => self.dma.ch[OTC].chan_control & (1 << 28) != 0,
            5 => false,
            _ => true,
        }
    }

    fn dma_recalc_irq(&mut self) {
        let d = &self.dma;
        let out = (d.int_status != 0 && d.int_control >> 23 & 1 != 0) || d.int_control >> 15 & 1 != 0;
        if out && !d.irq_out {
            self.raise_irq(1 << 3);
        }
        self.dma.irq_out = out;
    }

    fn dma_recalc_halt(&mut self) {
        let halt = self
            .dma
            .ch
            .iter()
            .any(|c| c.chan_control & (1 << 24) != 0 && c.chan_control & (7 << 8) == 0 && c.word_counter > 0);
        let g = self.dma.ch[GPU];
        self.dma.cycle_steal =
            if !halt && g.chan_control & (1 << 24) != 0 && g.chan_control & 0x700 == 0x200 && self.dma_can(GPU) {
                ((g.block_control & 0xFFFF).max(1) - 1).min(200) as u64
            } else {
                0
            };
        self.dma.halted = halt;
    }

    /// One word between RAM and the channel's device (Mednafen's `ChRW`).
    fn dma_word(&mut self, ch: usize, mode: u32, v: u32) -> u32 {
        let mut extra = 0;
        let v = match ch {
            MDEC_IN => {
                if mode & 1 != 0 {
                    self.mdec.write_word(v);
                    v
                } else {
                    0
                }
            }
            MDEC_OUT => {
                if mode & 1 != 0 {
                    v
                } else {
                    self.mdec.dma_out_word()
                }
            }
            GPU => {
                if mode & 1 != 0 {
                    self.gpu.gp0(v);
                    self.after_gpu();
                    v
                } else {
                    self.gpu.read_data()
                }
            }
            CDROM => {
                if mode & 1 != 0 {
                    v
                } else {
                    extra = 8;
                    self.cdrom.dma_word()
                }
            }
            SPU => {
                extra = 47;
                let fired = if mode & 1 != 0 {
                    self.spu.fifo_write(v as u16) | self.spu.fifo_write((v >> 16) as u16)
                } else {
                    let ((lo, f0), (hi, f1)) = (self.spu.fifo_read(), self.spu.fifo_read());
                    return self.dma_word_done(ch, mode, extra, lo as u32 | (hi as u32) << 16, f0 | f1);
                };
                return self.dma_word_done(ch, mode, extra, v, fired);
            }
            OTC => {
                let c = &self.dma.ch[OTC];
                if c.word_counter == 1 { 0xFF_FFFF } else { c.cur_addr.wrapping_sub(4) & 0x1F_FFFF }
            }
            _ => {
                if mode & 1 != 0 {
                    v
                } else {
                    0
                }
            }
        };
        self.dma_word_done(ch, mode, extra, v, false)
    }

    fn dma_word_done(&mut self, ch: usize, mode: u32, extra: i32, v: u32, spu_irq: bool) -> u32 {
        if spu_irq {
            self.raise_irq(1 << 9);
        }
        self.dma.ch[ch].clock_counter -= extra.max(if mode & 0x100 != 0 { 7 } else { 0 });
        v
    }

    /// Advance channel `ch` by `clocks` (Mednafen's `RunChannelI`).
    fn dma_run_channel(&mut self, ch: usize, clocks: i32) {
        let mode = self.dma.ch[ch].chan_control & !(0x11 << 24);
        self.dma.ch[ch].clock_counter += clocks;
        while self.dma.ch[ch].clock_counter > 0 {
            let mut payload = true;
            if self.dma.ch[ch].word_counter == 0 {
                if self.dma.ch[ch].chan_control & (1 << 24) == 0 || !self.dma_can(ch) {
                    break;
                }
                let start = self.dma.ch[ch].base_addr;
                self.dma.ch[ch].cur_addr = start;
                if mode & (1 << 10) != 0 {
                    if start & 0x80_0000 != 0 {
                        self.dma_bus_error(ch);
                        break;
                    }
                    let header = self.ram_u32_at(start);
                    let c = &mut self.dma.ch[ch];
                    c.cur_addr = (start + 4) & 0xFF_FFFF;
                    c.word_counter = (header >> 24) as u16;
                    c.base_addr = header & 0xFF_FFFF;
                    c.clock_counter -= if c.word_counter != 0 { 15 } else { 10 };
                    payload = false;
                } else {
                    let c = &mut self.dma.ch[ch];
                    c.word_counter = c.block_control as u16;
                    if mode & (1 << 9) != 0 {
                        if ch == GPU {
                            c.clock_counter -= 7;
                        }
                        c.block_control =
                            (c.block_control & 0xFFFF) | (c.block_control.wrapping_sub(1 << 16) & 0xFFFF_0000);
                    }
                }
            } else if mode & 0x100 != 0 {
                let c = &mut self.dma.ch[ch];
                c.cur_addr = c.base_addr;
                c.word_counter = c.block_control as u16;
            }
            if payload {
                let addr = self.dma.ch[ch].cur_addr;
                if addr & 0x80_0000 != 0 {
                    self.dma_bus_error(ch);
                    break;
                }
                let v = if mode & 1 != 0 { self.ram_u32_at(addr) } else { 0 };
                let v = self.dma_word(ch, mode, v);
                if mode & 1 == 0 {
                    let o = (addr & 0x1F_FFFC) as usize;
                    self.ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
                }
                let c = &mut self.dma.ch[ch];
                c.cur_addr = if mode & 2 != 0 { c.cur_addr.wrapping_sub(4) } else { c.cur_addr + 4 } & 0xFF_FFFF;
                c.word_counter = c.word_counter.wrapping_sub(1);
                c.clock_counter -= 1;
            }
            let c = &mut self.dma.ch[ch];
            if mode & 0x100 != 0 {
                c.base_addr = c.cur_addr;
                c.block_control = (c.block_control & 0xFFFF_0000) | c.word_counter as u32;
            }
            if c.word_counter == 0 {
                if c.chan_control & (1 << 24) == 0 {
                    break;
                }
                let end = match (mode >> 9) & 3 {
                    0 => true,
                    1 => {
                        c.base_addr = c.cur_addr;
                        c.block_control >> 16 == 0
                    }
                    _ => c.base_addr == 0xFF_FFFF,
                };
                if end {
                    c.chan_control &= !(0x11 << 24);
                    if self.dma.int_control & (1 << (16 + ch)) != 0 {
                        self.dma.int_status |= 1 << ch;
                        self.dma_recalc_irq();
                    }
                    break;
                }
            }
        }
        let c = &mut self.dma.ch[ch];
        if c.clock_counter > 0 {
            c.clock_counter = 0;
        }
    }

    fn dma_bus_error(&mut self, ch: usize) {
        self.dma.ch[ch].chan_control &= !(0x11 << 24);
        self.dma.int_control |= 0x8000;
        self.dma_recalc_irq();
    }

    fn ram_u32_at(&self, addr: u32) -> u32 {
        let o = (addr & 0x1F_FFFC) as usize;
        u32::from_le_bytes(self.ram[o..o + 4].try_into().unwrap())
    }

    /// Advance every channel to `t` (Mednafen's `DMA_Update`).
    pub(crate) fn dma_update(&mut self, t: u64) {
        self.gpu_update(t);
        let clocks = t.saturating_sub(self.dma.last) as i32;
        self.dma.last = t;
        for ch in [0, 1, 2, 3, 4, 6] {
            self.dma_run_channel(ch, clocks);
        }
        self.dma.cycle_counter -= clocks;
        while self.dma.cycle_counter <= 0 {
            self.dma.cycle_counter += GRID as i32;
        }
        self.dma_recalc_halt();
    }

    /// A CPU write to a DMA register at `t` (Mednafen's `DMA_Write`).
    pub(crate) fn dma_write(&mut self, t: u64, a: u32, v: u32) {
        let ch = ((a & 0x7F) >> 4) as usize;
        let v = v << ((a & 3) * 8);
        self.dma_update(t);
        if ch == 7 {
            match a & 0xC {
                0x0 => {
                    self.dma.control = v;
                    self.dma_recalc_halt();
                }
                0x4 => {
                    self.dma.int_control = v & 0x00FF_803F;
                    self.dma.int_status &= !((v >> 24) as u8);
                    self.dma_recalc_irq();
                }
                _ => {}
            }
            return;
        }
        match a & 0xC {
            0x0 => self.dma.ch[ch].base_addr = v & 0xFF_FFFF,
            0x4 => self.dma.ch[ch].block_control = v,
            _ => {
                let old = self.dma.ch[ch].chan_control;
                if old & (1 << 24) != 0 && v & (1 << 24) == 0 {
                    // Stopped: finish the block it's on at most.
                    self.dma.ch[ch].chan_control &= !(1 << 24);
                    self.dma_run_channel(ch, 128 * 16);
                    self.dma.ch[ch].word_counter = 0;
                }
                self.dma.ch[ch].chan_control = if ch == OTC { (v & 0x5100_0000) | 2 } else { v & 0x7177_0703 };
                if old & (1 << 24) == 0 && v & (1 << 24) != 0 {
                    self.dma_started(ch);
                    let c = &mut self.dma.ch[ch];
                    c.word_counter = 0;
                    c.clock_counter = 0;
                    self.dma_run_channel(ch, 64);
                }
                self.dma_recalc_halt();
            }
        }
    }
}

//! R3000A interpreter (psx-spx "CPU Specifications").
//!
//! Modelled: branch delay slots, load delay slots (including the LWL/LWR
//! merge with an in-flight load and the "later write wins" rule), COP0
//! exceptions with the KU/IE stack, RFE, interrupts gated by SR.IEc/IM and
//! Cause.IP, cache isolation (SR.IsC drops stores), overflow/address-error/
//! reserved-instruction/coprocessor-unusable exceptions, and COP2 via psx-gte.
//!
//! Timing is Mednafen's (`crates/psx-oracle/mednafen/src/psx/cpu.cpp`): the
//! instruction cache (4-word fills, 4 cycles per uncached fetch), a cycle per
//! instruction that a load's latency can absorb until its register is used,
//! the bus's wait states ([`Bus::read_cost`]), multiply/divide and GTE
//! latencies, and no interrupts at COP2 instructions.

use psx_gte::Gte;

use crate::bus::{Bus, Width};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exception {
    Interrupt = 0x0,
    LoadAddressError = 0x4,
    StoreAddressError = 0x5,
    InstructionBusError = 0x6,
    DataBusError = 0x7,
    Syscall = 0x8,
    Break = 0x9,
    ReservedInstruction = 0xA,
    CoprocessorUnusable = 0xB,
    Overflow = 0xC,
}

/// COP0 register numbers used here.
pub mod cop0 {
    pub const BADVADDR: usize = 8;
    pub const SR: usize = 12;
    pub const CAUSE: usize = 13;
    pub const EPC: usize = 14;
    pub const PRID: usize = 15;
}

/// What one [`Cpu::step`] did, for tracing and lockstep comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepInfo {
    pub pc: u32,
    pub instruction: u32,
    /// Exception taken *instead of* completing this instruction (or before it, for interrupts).
    pub exception: Option<u8>,
    /// Destination register of a load from I/O space issued by this instruction.
    pub io_load: Option<(u32, u8)>,
    /// The data access this instruction made (the last one for SWL/SWR).
    pub mem: Option<MemAccess>,
}

/// One CPU data access, as seen on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemAccess {
    pub addr: u32,
    /// Loaded value (before sign extension / merging) or stored value.
    pub value: u32,
    pub width: u8,
    pub store: bool,
}

/// Receives what one step did. [`StepInfo`] records it for tracing and
/// lockstep; `()` discards it, so untraced run loops pay nothing for it.
pub trait StepSink {
    fn begin(&mut self, pc: u32, instruction: u32);
    fn exception(&mut self, e: Exception);
    fn io_load(&mut self, addr: u32, rt: u8);
    fn mem(&mut self, m: MemAccess);
}

impl StepSink for StepInfo {
    #[inline(always)]
    fn begin(&mut self, pc: u32, instruction: u32) {
        *self = StepInfo { pc, instruction, exception: None, io_load: None, mem: None };
    }
    #[inline(always)]
    fn exception(&mut self, e: Exception) {
        self.exception = Some(e as u8);
    }
    #[inline(always)]
    fn io_load(&mut self, addr: u32, rt: u8) {
        self.io_load = Some((addr, rt));
    }
    #[inline(always)]
    fn mem(&mut self, m: MemAccess) {
        self.mem = Some(m);
    }
}

impl StepSink for () {
    #[inline(always)]
    fn begin(&mut self, _: u32, _: u32) {}
    #[inline(always)]
    fn exception(&mut self, _: Exception) {}
    #[inline(always)]
    fn io_load(&mut self, _: u32, _: u8) {}
    #[inline(always)]
    fn mem(&mut self, _: MemAccess) {}
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cpu {
    pub regs: [u32; 32],
    pub hi: u32,
    pub lo: u32,
    pub pc: u32,
    next_pc: u32,
    /// Pending delayed load `(register, value)`.
    load: (usize, u32),
    /// The load that retires after the current instruction (taken at step start).
    load_in: (usize, u32),
    /// The current instruction's register write (MIPS writes at most one GPR).
    wreg: (usize, u32),
    /// The current instruction is a branch (taken or not): the next is its delay slot.
    branch: bool,
    delay_slot: bool,
    /// The current branch is taken / the delay slot's branch was (Cause.BT).
    /// Not in save states: only an exception in the delay slot right after a
    /// restore would miss BT.
    #[serde(skip)]
    taken: bool,
    #[serde(skip)]
    delay_taken: bool,
    /// The instruction being executed (0 before it is fetched), for Cause.CE.
    #[serde(skip)]
    instr: u32,
    pub cop0: [u32; 32],
    pub gte: Gte,
    pub cycles: u64,
    /// External interrupt line (Cause.IP2), driven by the interrupt controller.
    pub irq_line: bool,
    t: Timing,
}

/// Mednafen's CPU timing state, named as in `cpu.cpp`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Timing {
    /// Instruction cache: per word, the tag (address, | 2 when invalid) and data.
    icache: Vec<(u32, u32)>,
    /// Cycles of a load's latency still to absorb, by destination register
    /// (32: a load with none, LWCz).
    #[serde(with = "crate::state::big_array")]
    read_absorb: [u8; 33],
    read_absorb_which: u8,
    /// The register the last retiring load wrote (32: none): a load right
    /// after another skips the 2-cycle bus turnaround.
    read_fudge: u8,
    /// The load in flight's register (32: none) and latency, for the timing only.
    ld_which: u8,
    ld_absorb: u32,
    /// When the GTE's and the multiplier's results are ready.
    gte_done: u64,
    muldiv_done: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            icache: vec![(2, 0); 1024],
            read_absorb: [0; 33],
            read_absorb_which: 0,
            read_fudge: 0,
            ld_which: 32,
            ld_absorb: 0,
            gte_done: 0,
            muldiv_done: 0,
        }
    }
}

/// Mednafen's `addr_mask`: KSEG0 and KSEG1 lose their segment bits.
fn segment_mask(a: u32) -> u32 {
    match a >> 29 {
        4 => a & 0x7FFF_FFFF,
        5 => a & 0x1FFF_FFFF,
        _ => a,
    }
}

/// Multiply latency by the leading zeros of the first operand (Mednafen's `MULT_Tab24`).
fn mult_cycles(lz: u32) -> u64 {
    7 + if lz < 12 { 4 } else { 0 } + if lz < 21 { 3 } else { 0 }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn sext16(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}

impl Cpu {
    pub fn new() -> Self {
        let mut cop0 = [0; 32];
        cop0[cop0::PRID] = 0x0000_0002;
        // Most SR bits are undefined at reset; Mednafen's value is BEV | TS.
        cop0[cop0::SR] = 0x0060_0000;
        Cpu {
            regs: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0xBFC0_0000,
            next_pc: 0xBFC0_0004,
            load: (0, 0),
            load_in: (0, 0),
            wreg: (0, 0),
            branch: false,
            delay_slot: false,
            taken: false,
            delay_taken: false,
            instr: 0,
            cop0,
            gte: Gte::new(),
            cycles: 0,
            irq_line: false,
            t: Timing::default(),
        }
    }

    /// Program counter of the next instruction to execute after the current one.
    pub fn next_pc(&self) -> u32 {
        self.next_pc
    }

    /// Jump to `pc` (e.g. side-loading an EXE), clearing delay-slot state.
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
        self.branch = false;
        self.delay_slot = false;
        self.taken = false;
        self.delay_taken = false;
    }

    #[inline]
    fn set_reg(&mut self, r: usize, v: u32) {
        self.wreg = (r, v);
    }

    /// Complete an in-flight load immediately (when an exception pre-empts the
    /// instruction that would otherwise have retired it).
    fn flush_load(&mut self) {
        let (r, v) = self.load;
        self.regs[r] = v;
        self.regs[0] = 0;
        self.load = (0, 0);
    }

    fn cause_ip(&self) -> u32 {
        (self.cop0[cop0::CAUSE] & !0x400) | if self.irq_line { 0x400 } else { 0 }
    }

    /// True when an interrupt would be taken before the next instruction.
    #[inline]
    pub fn interrupt_pending(&self) -> bool {
        let sr = self.cop0[cop0::SR];
        // Cause.IP is computed on demand (Cause reads and exceptions use cause_ip()).
        sr & 1 != 0 && (sr & self.cause_ip() & 0xFF00) != 0
    }

    #[cold]
    #[inline(never)]
    fn exception(&mut self, cause: Exception, epc_pc: u32) {
        let sr = self.cop0[cop0::SR];
        // Push the KU/IE stack (bits 0..5 shift left by two).
        self.cop0[cop0::SR] = (sr & !0x3F) | ((sr << 2) & 0x3F);
        // Cause keeps only its interrupt bits; CE is the instruction's bits
        // 26-27 for every exception (psx-spx, Mednafen).
        let mut c = self.cause_ip() & 0xFF00;
        c |= (cause as u32) << 2 | (self.instr >> 26 & 3) << 28;
        let handler = if sr & (1 << 22) != 0 { 0xBFC0_0180 } else { 0x8000_0080 };
        if self.delay_slot {
            self.cop0[cop0::EPC] = epc_pc.wrapping_sub(4);
            c |= 1 << 31 | (self.delay_taken as u32) << 30;
        } else {
            self.cop0[cop0::EPC] = epc_pc;
        }
        self.cop0[cop0::CAUSE] = c;
        self.pc = handler;
        self.next_pc = handler.wrapping_add(4);
    }

    /// Execute one instruction (or take a pending interrupt).
    pub fn step(&mut self, bus: &mut Bus) -> StepInfo {
        let mut info = StepInfo::default();
        while !self.step_with(bus, &mut info) {}
        info
    }

    /// [`Cpu::step`], reporting what happened to `info`. The bus's devices
    /// are brought up to the CPU's time before each access and at the end.
    /// False when a DMA holds the CPU and it only spun for a cycle (nothing
    /// was executed or reported).
    #[inline(always)]
    pub fn step_with<I: StepSink>(&mut self, bus: &mut Bus, info: &mut I) -> bool {
        let ran = self.step_inner(bus, info);
        self.sync(bus);
        ran
    }

    /// A cycle per instruction, unless a load's latency absorbs it.
    #[inline(always)]
    fn issue_cycle(&mut self) {
        let w = self.t.read_absorb_which as usize;
        if self.t.read_absorb[w] != 0 {
            self.t.read_absorb[w] -= 1;
        } else {
            self.cycles += 1;
        }
    }

    /// Hand the cycles the CPU has used to the bus.
    #[inline(always)]
    fn sync(&self, bus: &mut Bus) {
        let t = bus.time();
        if self.cycles > t {
            bus.tick(self.cycles - t);
        }
    }

    #[inline(always)]
    fn step_inner<I: StepSink>(&mut self, bus: &mut Bus, info: &mut I) -> bool {
        let pc = self.pc;
        bus.cur_pc = pc;
        // Held by DMA, Mednafen's CPU still fetches and spends the cycle, and
        // runs the instruction only if it is COP2.
        let mut fetched = None;
        if bus.dma.halted && pc & 3 == 0 {
            let r = self.fetch(bus, pc);
            self.issue_cycle();
            match r {
                Ok(i) if i >> 26 != 0x12 => return false,
                r => fetched = Some(r),
            }
        }
        self.delay_slot = self.branch;
        self.delay_taken = self.taken;
        self.branch = false;
        self.taken = false;
        self.instr = 0;

        if pc & 3 != 0 {
            self.flush_load();
            self.cop0[cop0::BADVADDR] = pc;
            self.exception(Exception::LoadAddressError, pc);
            info.begin(pc, 0);
            info.exception(Exception::LoadAddressError);
            return true;
        }
        let prefetched = fetched.is_some();
        let instruction = match fetched.unwrap_or_else(|| self.fetch(bus, pc)) {
            Ok(i) => i,
            Err(_) => {
                self.flush_load();
                self.exception(Exception::InstructionBusError, pc);
                info.begin(pc, 0);
                info.exception(Exception::InstructionBusError);
                return true;
            }
        };
        self.instr = instruction;
        if !prefetched {
            self.issue_cycle();
        }
        // Mednafen takes no interrupt at a COP2 instruction.
        if self.interrupt_pending() && instruction >> 26 != 0x12 {
            // EPC is the instruction about to execute; it runs after RFE.
            self.retire_timing();
            self.flush_load();
            self.exception(Exception::Interrupt, pc);
            info.begin(pc, instruction);
            info.exception(Exception::Interrupt);
            return true;
        }

        self.pc = self.next_pc;
        self.next_pc = self.next_pc.wrapping_add(4);

        // Sources read the old register file; then the delayed load retires and
        // the instruction's own write lands last (a later write beats the load).
        self.load_in = std::mem::take(&mut self.load);
        self.wreg = (0, 0);
        info.begin(pc, instruction);
        self.issue_timing(instruction);
        if let Err(e) = self.execute(bus, instruction, pc, info) {
            info.exception(e);
        }
        // Indices are always < 32; the mask only spares the bounds checks.
        // A load into the register a load in flight is about to write cancels
        // that load: `lw t1; lw t1` never shows the first value (Mednafen,
        // DuckStation). LWL/LWR have already merged it.
        let (lr, lv) = self.load_in;
        if self.load.0 != lr {
            self.regs[lr & 31] = lv;
        }
        let (wr, wv) = self.wreg;
        self.regs[wr & 31] = wv;
        self.regs[0] = 0;
        true
    }

    /// Fetch through the instruction cache (Mednafen's `ReadInstruction`).
    #[inline(always)]
    fn fetch(&mut self, bus: &mut Bus, pc: u32) -> Result<u32, crate::bus::BusError> {
        let enabled = bus.cache_control & 0x800 != 0;
        let i = (pc as usize & 0xFFC) >> 2;
        let (tag, data) = self.t.icache[i];
        if enabled && tag == pc {
            return Ok(data);
        }
        self.cancel_absorb();
        if pc >= 0xA000_0000 || !enabled {
            self.cycles += 4;
            return bus.read(pc, Width::Word);
        }
        let line = pc & !0xF;
        for w in 0..4 {
            self.t.icache[(i & !3) + w].0 = line | (w as u32) << 2 | 2;
        }
        self.cycles += 3;
        for w in (i & 3)..4 {
            self.cycles += 1;
            let a = line | (w as u32) << 2;
            self.t.icache[(i & !3) + w] = (a, bus.read(a, Width::Word)?);
        }
        Ok(self.t.icache[i].1)
    }

    /// A load or cache miss ends any absorption in progress.
    #[inline(always)]
    fn cancel_absorb(&mut self) {
        self.t.read_absorb[self.t.read_absorb_which as usize] = 0;
        self.t.read_absorb_which = 0;
    }

    /// Mednafen's `DO_LDS` as far as timing goes: the retiring load's latency
    /// starts being absorbed.
    #[inline(always)]
    fn retire_timing(&mut self) {
        let w = self.t.ld_which;
        self.t.read_absorb[w as usize] = self.t.ld_absorb as u8;
        self.t.read_fudge = w;
        self.t.read_absorb_which |= w & 0x1F;
        self.t.ld_which = 32;
    }

    /// Before an instruction executes: its register dependencies end the
    /// absorption of loads into them (Mednafen's `GPR_DEP`/`GPR_RES`), then
    /// the load in flight retires.
    #[inline(always)]
    fn issue_timing(&mut self, i: u32) {
        let (rs, rt, rd) = ((i >> 21 & 31) as usize, (i >> 16 & 31) as usize, (i >> 11 & 31) as usize);
        let back = self.t.read_absorb[0];
        let mut clear = |r: usize| self.t.read_absorb[r] = 0;
        match i >> 26 {
            0x00 => match i & 0x3F {
                0x00 | 0x02 | 0x03 => {
                    clear(rt);
                    clear(rd)
                }
                0x04 | 0x06 | 0x07 | 0x20..=0x27 | 0x2A | 0x2B => {
                    clear(rs);
                    clear(rt);
                    clear(rd)
                }
                0x08 | 0x09 => {
                    clear(rs);
                    clear(rd)
                }
                0x10 | 0x12 => clear(rd),
                0x11 | 0x13 => clear(rs),
                0x18..=0x1B => {
                    clear(rs);
                    clear(rt)
                }
                _ => {}
            },
            0x01 => {
                clear(rs);
                clear(if rt & 0x1E == 0x10 { 31 } else { 0 });
            }
            0x03 => clear(31),
            0x04 | 0x05 => {
                clear(rs);
                clear(rt)
            }
            0x06 | 0x07 => clear(rs),
            0x08..=0x0E => {
                clear(rs);
                clear(rt)
            }
            0x0F => clear(rt),
            0x20..=0x26 => clear(rs),
            0x28..=0x2B | 0x2E => {
                clear(rs);
                clear(rt)
            }
            _ => {}
        }
        if i >> 26 != 0x03 {
            self.t.read_absorb[0] = back;
        }
        let rt8 = rt as u8;
        match i >> 26 {
            // LWL/LWR into the register in flight merge with it instead.
            0x22 | 0x26 if self.t.ld_which == rt8 => self.t.read_fudge = 0,
            0x22 | 0x26 => self.retire_timing(),
            // A load into the register in flight cancels it (when it can issue).
            0x20 | 0x24 => self.cancel_then_retire(rt8),
            0x21 | 0x23 | 0x25 => {
                let width = if i >> 26 == 0x23 { 4 } else { 2 };
                if self.regs[rs].wrapping_add(sext16(i & 0xFFFF)) & (width - 1) == 0 {
                    self.cancel_then_retire(rt8);
                } else {
                    self.retire_timing();
                }
            }
            0x10 if i >> 21 & 31 == 0 && matches!(rd, 3 | 5..=9 | 0x0B..=0x0F) => self.cancel_then_retire(rt8),
            0x12 if self.cop0[cop0::SR] & (1 << 30) != 0 && matches!(i >> 21 & 31, 0 | 2) => {
                self.cancel_then_retire(rt8)
            }
            _ => self.retire_timing(),
        }
    }

    #[inline(always)]
    fn cancel_then_retire(&mut self, rt: u8) {
        if self.t.ld_which == rt {
            self.t.ld_which = 0;
        }
        self.retire_timing();
    }

    /// Wait for the GTE (Mednafen's `gte_ts_done`); returns the cycles waited.
    #[inline(always)]
    fn gte_wait(&mut self) -> u64 {
        let waited = self.t.gte_done.saturating_sub(self.cycles);
        self.cycles += waited;
        waited
    }

    /// MFHI/MFLO wait for the multiplier, absorbing load latency meanwhile.
    fn muldiv_wait(&mut self) {
        if self.cycles < self.t.muldiv_done {
            if self.cycles == self.t.muldiv_done - 1 {
                self.t.muldiv_done -= 1;
            } else {
                while self.cycles < self.t.muldiv_done {
                    let w = self.t.read_absorb_which as usize;
                    self.t.read_absorb[w] = self.t.read_absorb[w].saturating_sub(1);
                    self.cycles += 1;
                }
            }
        }
    }

    /// A data read's timing, around the access itself (Mednafen's
    /// `ReadMemory`): `bytes` is Mednafen's access size, `lwc` for LWCz.
    #[inline(always)]
    fn read_timed(
        &mut self,
        bus: &mut Bus,
        addr: u32,
        w: Width,
        bytes: u32,
        lwc: bool,
    ) -> Result<u32, crate::bus::BusError> {
        self.cancel_absorb();
        let a = segment_mask(addr);
        if (0x1F80_0000..=0x1F80_03FF).contains(&a) {
            self.t.ld_absorb = 0;
            return bus.read(addr, w);
        }
        self.cycles += ((self.t.read_fudge >> 4) & 2) as u64;
        let start = self.cycles;
        self.cycles += bus.dma.cycle_steal;
        // Mednafen checks events before the access's wait states (and an SPU
        // read checks again after them).
        if Bus::checks_events(a) {
            bus.events(self.cycles);
        }
        self.cycles += Bus::read_cost(a, bytes);
        if (0x1F80_1C00..=0x1F80_1FFF).contains(&a) {
            bus.events(self.cycles);
        }
        if Bus::is_io(addr) {
            self.sync(bus);
        }
        let v = bus.read(addr, w);
        self.cycles += if lwc { 1 } else { 2 };
        self.t.ld_absorb = (self.cycles - start) as u32;
        v
    }

    /// A store with the cache isolated (SR.IsC) reaches the instruction
    /// cache's tags or data, or the scratchpad (Mednafen's `WriteMemory`).
    fn isolated_store(&mut self, bus: &mut Bus, addr: u32, bytes: u32, value: u32) {
        let biu = bus.cache_control;
        if biu & 0x800 != 0 {
            if biu & 7 != 0 {
                let valid = if biu & 4 != 0 { (value << ((addr & 3) * 8)) & 0xF } else { 0 };
                let line = ((addr & 0xFF0) >> 2) as usize;
                for i in 0..4 {
                    let invalid = if valid & (1 << i) != 0 { 0 } else { 2 };
                    self.t.icache[line + i].0 = invalid | (addr & !0xF) | (i as u32) << 2;
                }
            } else {
                self.t.icache[((addr & 0xFFC) >> 2) as usize].1 = value << ((addr & 3) * 8);
            }
        }
        if biu & 0x81 == 0x80 {
            for i in 0..bytes {
                bus.scratch[((addr + i) & 0x3FF) as usize] = (value >> (8 * i)) as u8;
            }
        }
    }

    fn branch_to(&mut self, target: u32) {
        self.next_pc = target;
        self.branch = true;
        self.taken = true;
    }

    /// A conditional branch: its delay slot is one (Cause.BD) taken or not.
    fn cond_branch(&mut self, cond: bool, offset: u32) {
        if cond {
            self.rel_branch(offset);
        } else {
            self.branch = true;
        }
    }

    fn rel_branch(&mut self, offset: u32) {
        let target = self.pc.wrapping_add(sext16(offset) << 2);
        self.branch_to(target);
    }

    #[allow(clippy::too_many_arguments)]
    fn load_mem(
        &mut self,
        bus: &mut Bus,
        addr: u32,
        w: Width,
        pc: u32,
        info: &mut impl StepSink,
        rt: usize,
        bytes: u32,
        lwc: bool,
    ) -> Result<u32, Exception> {
        // Widths are 1, 2 or 4: a mask test (is_multiple_of compiles to a divide here).
        if addr & (w as u32 - 1) != 0 {
            self.cop0[cop0::BADVADDR] = addr;
            self.exception(Exception::LoadAddressError, pc);
            return Err(Exception::LoadAddressError);
        }
        if Bus::is_io(addr) {
            info.io_load(addr, rt as u8);
        }
        let v = self.read_timed(bus, addr, w, bytes, lwc).map_err(|_| {
            self.exception(Exception::DataBusError, pc);
            Exception::DataBusError
        })?;
        info.mem(MemAccess { addr, value: v, width: w as u8, store: false });
        Ok(v)
    }

    fn store_mem(
        &mut self,
        bus: &mut Bus,
        addr: u32,
        w: Width,
        value: u32,
        pc: u32,
        info: &mut impl StepSink,
    ) -> Result<(), Exception> {
        // Widths are 1, 2 or 4: a mask test (is_multiple_of compiles to a divide here).
        if addr & (w as u32 - 1) != 0 {
            self.cop0[cop0::BADVADDR] = addr;
            self.exception(Exception::StoreAddressError, pc);
            return Err(Exception::StoreAddressError);
        }
        info.mem(MemAccess { addr, value, width: w as u8, store: true });
        if self.cop0[cop0::SR] & (1 << 16) != 0 {
            self.isolated_store(bus, addr, w as u32, value);
            return Ok(());
        }
        if Bus::checks_events(segment_mask(addr)) {
            bus.events(self.cycles);
        }
        if Bus::is_io(addr) {
            self.sync(bus);
        }
        bus.write(addr, w, value).map_err(|_| {
            self.exception(Exception::DataBusError, pc);
            Exception::DataBusError
        })
    }

    fn fail(&mut self, e: Exception, pc: u32) -> Result<(), Exception> {
        self.exception(e, pc);
        Err(e)
    }

    fn execute(&mut self, bus: &mut Bus, i: u32, pc: u32, info: &mut impl StepSink) -> Result<(), Exception> {
        let op = i >> 26;
        let rs = ((i >> 21) & 31) as usize;
        let rt = ((i >> 16) & 31) as usize;
        let rd = ((i >> 11) & 31) as usize;
        let shamt = (i >> 6) & 31;
        let imm = i & 0xFFFF;
        let simm = sext16(imm);
        let s = self.regs[rs];
        let t = self.regs[rt];
        match op {
            0x00 => match i & 0x3F {
                0x00 => self.set_reg(rd, t << shamt),
                0x02 => self.set_reg(rd, t >> shamt),
                0x03 => self.set_reg(rd, ((t as i32) >> shamt) as u32),
                0x04 => self.set_reg(rd, t << (s & 31)),
                0x06 => self.set_reg(rd, t >> (s & 31)),
                0x07 => self.set_reg(rd, ((t as i32) >> (s & 31)) as u32),
                0x08 => self.branch_to(s),
                0x09 => {
                    self.set_reg(rd, self.next_pc);
                    self.branch_to(s);
                }
                0x0C => return self.fail(Exception::Syscall, pc),
                0x0D => return self.fail(Exception::Break, pc),
                0x10 => {
                    self.muldiv_wait();
                    self.set_reg(rd, self.hi)
                }
                0x11 => self.hi = s,
                0x12 => {
                    self.muldiv_wait();
                    self.set_reg(rd, self.lo)
                }
                0x13 => self.lo = s,
                0x18 => {
                    self.t.muldiv_done =
                        self.cycles + mult_cycles((s ^ ((s as i32) >> 31) as u32 | 0x400).leading_zeros());
                    let p = (s as i32 as i64) * (t as i32 as i64);
                    self.hi = (p >> 32) as u32;
                    self.lo = p as u32;
                }
                0x19 => {
                    self.t.muldiv_done = self.cycles + mult_cycles((s | 0x400).leading_zeros());
                    let p = (s as u64) * (t as u64);
                    self.hi = (p >> 32) as u32;
                    self.lo = p as u32;
                }
                0x1A => {
                    self.t.muldiv_done = self.cycles + 37;
                    let (n, d) = (s as i32, t as i32);
                    if d == 0 {
                        self.hi = s;
                        self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
                    } else if s == 0x8000_0000 && d == -1 {
                        self.hi = 0;
                        self.lo = 0x8000_0000;
                    } else {
                        self.hi = (n % d) as u32;
                        self.lo = (n / d) as u32;
                    }
                }
                0x1B => {
                    self.t.muldiv_done = self.cycles + 37;
                    if t == 0 {
                        self.hi = s;
                        self.lo = 0xFFFF_FFFF;
                    } else {
                        self.hi = s % t;
                        self.lo = s / t;
                    }
                }
                0x20 => match (s as i32).checked_add(t as i32) {
                    Some(v) => self.set_reg(rd, v as u32),
                    None => return self.fail(Exception::Overflow, pc),
                },
                0x21 => self.set_reg(rd, s.wrapping_add(t)),
                0x22 => match (s as i32).checked_sub(t as i32) {
                    Some(v) => self.set_reg(rd, v as u32),
                    None => return self.fail(Exception::Overflow, pc),
                },
                0x23 => self.set_reg(rd, s.wrapping_sub(t)),
                0x24 => self.set_reg(rd, s & t),
                0x25 => self.set_reg(rd, s | t),
                0x26 => self.set_reg(rd, s ^ t),
                0x27 => self.set_reg(rd, !(s | t)),
                0x2A => self.set_reg(rd, ((s as i32) < (t as i32)) as u32),
                0x2B => self.set_reg(rd, (s < t) as u32),
                _ => return self.fail(Exception::ReservedInstruction, pc),
            },
            0x01 => {
                // BLTZ/BGEZ(+AL): bit 16 selects GEZ; rt 0x10/0x11 link.
                let ge = (i >> 16) & 1 != 0;
                let link = (i >> 17) & 0xF == 0x8;
                let taken = ((s as i32) < 0) ^ ge;
                if link {
                    self.set_reg(31, self.next_pc);
                }
                self.cond_branch(taken, imm);
            }
            0x02 => self.branch_to((self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2)),
            0x03 => {
                self.set_reg(31, self.next_pc);
                self.branch_to((self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2));
            }
            0x04 => self.cond_branch(s == t, imm),
            0x05 => self.cond_branch(s != t, imm),
            0x06 => self.cond_branch((s as i32) <= 0, imm),
            0x07 => self.cond_branch((s as i32) > 0, imm),
            0x08 => match (s as i32).checked_add(simm as i32) {
                Some(v) => self.set_reg(rt, v as u32),
                None => return self.fail(Exception::Overflow, pc),
            },
            0x09 => self.set_reg(rt, s.wrapping_add(simm)),
            0x0A => self.set_reg(rt, ((s as i32) < (simm as i32)) as u32),
            0x0B => self.set_reg(rt, (s < simm) as u32),
            0x0C => self.set_reg(rt, s & imm),
            0x0D => self.set_reg(rt, s | imm),
            0x0E => self.set_reg(rt, s ^ imm),
            0x0F => self.set_reg(rt, imm << 16),
            0x10 => self.cop0_op(i, pc)?,
            0x11 | 0x13 => return self.fail(Exception::CoprocessorUnusable, pc),
            0x12 => self.cop2_op(i, pc)?,
            0x20 | 0x21 | 0x23 | 0x24 | 0x25 => {
                let addr = s.wrapping_add(simm);
                let w = match op {
                    0x20 | 0x24 => Width::Byte,
                    0x21 | 0x25 => Width::Half,
                    _ => Width::Word,
                };
                let v = self.load_mem(bus, addr, w, pc, info, rt, w as u32, false)?;
                self.t.ld_which = rt as u8;
                let v = match op {
                    0x20 => v as u8 as i8 as i32 as u32,
                    0x21 => v as u16 as i16 as i32 as u32,
                    _ => v,
                };
                self.load = (rt, v);
            }
            0x22 | 0x26 => {
                // LWL/LWR merge with the in-flight value of rt (bypassing the load delay).
                let addr = s.wrapping_add(simm);
                let k = addr & 3;
                let bytes = if op == 0x22 { k + 1 } else { 4 - k };
                let aligned = self.load_mem(bus, addr & !3, Width::Word, pc, info, rt, bytes, false)?;
                self.t.ld_which = rt as u8;
                // In-flight load to rt merges (LWL/LWR bypass the load delay).
                let cur = if self.load_in.0 == rt { self.load_in.1 } else { self.regs[rt] };
                let sh = (addr & 3) * 8;
                let v = if op == 0x22 {
                    let keep = 0x00FF_FFFFu32.checked_shr(sh).unwrap_or(0);
                    (cur & keep) | (aligned << (24 - sh))
                } else {
                    let keep = 0xFFFF_FF00u32.checked_shl(24 - sh).unwrap_or(0);
                    (cur & keep) | (aligned >> sh)
                };
                self.load = (rt, v);
            }
            0x28 => self.store_mem(bus, s.wrapping_add(simm), Width::Byte, t, pc, info)?,
            0x29 => self.store_mem(bus, s.wrapping_add(simm), Width::Half, t, pc, info)?,
            0x2B => self.store_mem(bus, s.wrapping_add(simm), Width::Word, t, pc, info)?,
            0x2A | 0x2E => {
                let addr = s.wrapping_add(simm);
                let aligned_addr = addr & !3;
                if self.cop0[cop0::SR] & (1 << 16) != 0 {
                    // Mednafen stores the 1-4 bytes the instruction covers.
                    let k = addr & 3;
                    let (a, bytes, v) =
                        if op == 0x2A { (aligned_addr, k + 1, t >> (24 - 8 * k)) } else { (addr, 4 - k, t) };
                    self.isolated_store(bus, a, bytes, v);
                    return Ok(());
                }
                let mem = bus.read(aligned_addr, Width::Word).unwrap_or(0);
                let sh = (addr & 3) * 8;
                let v = if op == 0x2A {
                    let keep = 0xFFFF_FF00u32.checked_shl(sh).unwrap_or(0);
                    (mem & keep) | (t >> (24 - sh))
                } else {
                    let keep = 0x00FF_FFFFu32.checked_shr(24 - sh).unwrap_or(0);
                    (mem & keep) | (t << sh)
                };
                self.store_mem(bus, aligned_addr, Width::Word, v, pc, info)?;
            }
            0x32 => {
                let addr = s.wrapping_add(simm);
                if addr & 3 == 0 {
                    self.gte_wait();
                }
                let v = self.load_mem(bus, addr, Width::Word, pc, info, 0, 4, true)?;
                self.gte.write_data(rt, v);
            }
            0x3A => {
                let addr = s.wrapping_add(simm);
                if addr & 3 == 0 {
                    self.gte_wait();
                }
                let v = self.gte.read_data(rt);
                self.store_mem(bus, addr, Width::Word, v, pc, info)?;
            }
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => return self.fail(Exception::CoprocessorUnusable, pc),
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        Ok(())
    }

    fn cop0_op(&mut self, i: u32, pc: u32) -> Result<(), Exception> {
        let rt = ((i >> 16) & 31) as usize;
        let rd = ((i >> 11) & 31) as usize;
        match (i >> 21) & 31 {
            0x00 => {
                let v = if rd == cop0::CAUSE { self.cause_ip() } else { self.cop0[rd] };
                self.load = (rt, v);
                if matches!(rd, 3 | 5..=9 | 0x0B..=0x0F) {
                    self.t.ld_absorb = 0;
                    self.t.ld_which = rt as u8;
                }
            }
            0x04 => {
                let v = self.regs[rt];
                match rd {
                    cop0::CAUSE => self.cop0[rd] = (self.cop0[rd] & !0x300) | (v & 0x300),
                    cop0::PRID | cop0::BADVADDR => {}
                    _ => self.cop0[rd] = v,
                }
            }
            0x10 if i & 0x3F == 0x10 => {
                let sr = self.cop0[cop0::SR];
                self.cop0[cop0::SR] = (sr & !0xF) | ((sr >> 2) & 0xF);
            }
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        Ok(())
    }

    fn cop2_op(&mut self, i: u32, pc: u32) -> Result<(), Exception> {
        if self.cop0[cop0::SR] & (1 << 30) == 0 {
            return self.fail(Exception::CoprocessorUnusable, pc);
        }
        let rt = ((i >> 16) & 31) as usize;
        let rd = ((i >> 11) & 31) as usize;
        if i & (1 << 25) != 0 {
            self.gte_wait();
            self.t.gte_done = self.cycles + self.gte.execute(i & 0x01FF_FFFF) as u64;
            return Ok(());
        }
        let op = (i >> 21) & 31;
        match op {
            0x00 | 0x02 => {
                // A move from the GTE waits for it; the wait is the load's latency.
                self.t.ld_absorb = self.gte_wait() as u32;
                self.t.ld_which = rt as u8;
                self.load = (rt, if op == 0 { self.gte.read_data(rd) } else { self.gte.read_ctrl(rd) });
            }
            0x04 => {
                self.gte_wait();
                self.gte.write_data(rd, self.regs[rt])
            }
            0x06 => {
                self.gte_wait();
                self.gte.write_ctrl(rd, self.regs[rt])
            }
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        Ok(())
    }

    /// The delayed load in flight `(register, value)`; register 0 is none.
    pub fn pending_load(&self) -> (usize, u32) {
        self.load
    }

    /// Raise an external interrupt now (lockstep: follow an IRQ the oracle took).
    pub fn force_interrupt(&mut self) {
        self.flush_load();
        let pc = self.pc;
        self.delay_slot = self.branch;
        self.delay_taken = self.taken;
        self.instr = 0;
        self.cop0[cop0::CAUSE] |= 0x400;
        self.exception(Exception::Interrupt, pc);
        self.branch = false;
        self.taken = false;
    }
}

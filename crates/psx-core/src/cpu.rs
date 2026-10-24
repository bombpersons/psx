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
    /// The delayed load in flight `(register, value)`: it lands during the
    /// next instruction, after that reads its operands (Mednafen's `DO_LDS`).
    load: (usize, u32),
    /// Branch state, as bits: the current instruction is a branch (taken or
    /// not; the next is its delay slot), and is taken (Cause.BT); the same two
    /// for the branch whose delay slot is executing.
    bd: u8,
    /// The instruction being executed (0 before it is fetched), for Cause.CE.
    #[serde(skip)]
    instr: u32,
    pub cop0: [u32; 32],
    pub gte: Gte,
    pub cycles: u64,
    /// External interrupt line (Cause.IP2), driven by the interrupt controller.
    pub irq_line: bool,
    /// [`Cpu::interrupt_pending`] as of the last change to SR, Cause or the
    /// interrupt line (Mednafen's `IPCache`); recomputed by [`Cpu::step_with`].
    #[serde(skip)]
    int_pending: bool,
    t: Timing,
}

/// [`Cpu::spin_key`].
#[derive(PartialEq)]
/// Fields in the order compared: what differs most often first.
pub(crate) struct SpinKey {
    pc: u32,
    next_pc: u32,
    bd: u8,
    int: (bool, bool),
    load: (usize, u32),
    waits: [u64; 2],
    regs: [u32; 32],
    hi: u32,
    lo: u32,
    timing: [u8; 3],
    ld_absorb: u32,
    absorb: [u8; 33],
    cop0: [u32; 32],
}

/// [`Cpu::bd`]'s bits.
mod bd {
    pub const BRANCH: u8 = 1;
    pub const TAKEN: u8 = 2;
    pub const DELAY_SLOT: u8 = 4;
    pub const DELAY_TAKEN: u8 = 8;
}

/// Mednafen's CPU timing state, named as in `cpu.cpp`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Timing {
    /// Instruction cache: per word, the tag (address, | 2 when invalid) and data.
    #[serde(with = "crate::state::big_array")]
    icache: [(u32, u32); 1024],
    /// Cycles of a load's latency still to absorb, by destination register
    /// (32: a load with none, LWCz). Sized so a `u8` index needs no check.
    #[serde(with = "crate::state::big_array")]
    read_absorb: [u8; 256],
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
            icache: [(2, 0); 1024],
            read_absorb: [0; 256],
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
#[inline(always)]
pub(crate) fn segment_mask(a: u32) -> u32 {
    const MASK: [u32; 8] = [!0, !0, !0, !0, 0x7FFF_FFFF, 0x1FFF_FFFF, !0, !0];
    a & MASK[(a >> 29) as usize]
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
            bd: 0,
            instr: 0,
            cop0,
            gte: Gte::new(),
            cycles: 0,
            irq_line: false,
            int_pending: false,
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
        self.bd = 0;
    }

    /// The instruction's own write, after the load in flight has landed (a
    /// later write beats the load). Register 0 is cleared at the step's end.
    #[inline(always)]
    fn set_reg(&mut self, r: usize, v: u32) {
        self.regs[r & 31] = v;
    }

    /// The load in flight lands (Mednafen's `DO_LDS`).
    #[inline(always)]
    fn land(&mut self) {
        // Register 0 stands for none; its value is left behind.
        let (r, v) = self.load;
        self.regs[r & 31] = v;
        self.load.0 = 0;
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

    /// Recompute [`Cpu::interrupt_pending`]'s cache after SR, Cause or the interrupt line changed.
    #[inline(always)]
    pub(crate) fn refresh_int(&mut self) {
        self.int_pending = self.interrupt_pending();
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
        if self.bd & bd::DELAY_SLOT != 0 {
            self.cop0[cop0::EPC] = epc_pc.wrapping_sub(4);
            c |= 1 << 31 | ((self.bd & bd::DELAY_TAKEN != 0) as u32) << 30;
        } else {
            self.cop0[cop0::EPC] = epc_pc;
        }
        self.cop0[cop0::CAUSE] = c;
        self.refresh_int();
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
        self.refresh_int();
        let ran = self.step_inner::<I, true>(bus, info);
        self.sync(bus);
        ran
    }

    /// [`Cpu::step_with`] without handing the time to the bus, for
    /// [`crate::Machine`]'s run loop between device events. No DMA holds the
    /// CPU: a hold starts only at an I/O access or a device event, either of
    /// which ends that run.
    #[inline(always)]
    pub(crate) fn step_unsynced(&mut self, bus: &mut Bus) {
        debug_assert_eq!(self.int_pending, self.interrupt_pending());
        debug_assert!(!bus.dma.halted);
        self.step_inner::<(), false>(bus, &mut ());
    }

    /// [`Cpu::step_unsynced`] until the CPU's clock reaches [`Bus::deadline`]
    /// (which an access outside RAM, BIOS and scratchpad clears), at most
    /// `max_steps` times; returns the steps taken. A function of its own, so
    /// the hot loop is compiled apart from the event handling around it.
    #[inline(never)]
    pub(crate) fn run_unsynced(&mut self, bus: &mut Bus, max_steps: u64) -> u64 {
        let mut n = 0;
        while n < max_steps && self.cycles < bus.deadline {
            self.step_unsynced(bus);
            n += 1;
        }
        n
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
    pub(crate) fn sync(&self, bus: &mut Bus) {
        let t = bus.time();
        if self.cycles > t {
            bus.tick(self.cycles - t);
        }
    }

    #[inline(always)]
    fn step_inner<I: StepSink, const HOLD: bool>(&mut self, bus: &mut Bus, info: &mut I) -> bool {
        let pc = self.pc;
        bus.cur_pc = pc;
        // Held by DMA, Mednafen's CPU still fetches and spends the cycle, and
        // runs the instruction only if it is COP2.
        let mut fetched = None;
        if HOLD && bus.dma.halted && pc & 3 == 0 {
            let r = self.fetch(bus, pc);
            self.issue_cycle();
            match r {
                Ok(i) if i >> 26 != 0x12 => return false,
                r => fetched = Some(r),
            }
        }
        self.bd = (self.bd & (bd::BRANCH | bd::TAKEN)) << 2;

        if pc & 3 != 0 {
            self.instr = 0;
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
                self.instr = 0;
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
        if self.int_pending && instruction >> 26 != 0x12 {
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

        info.begin(pc, instruction);
        // Each instruction reads its operands, then does its load-delay timing
        // and lands the load in flight (`issue`), then writes its result.
        if let Err(e) = self.execute(bus, instruction, pc, info) {
            // The load in flight lands even so (if the instruction has not
            // landed it already).
            self.land();
            info.exception(e);
        }
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
        bus.spin_breaks += 1;
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

    /// Before an instruction executes: the registers it reads or writes end
    /// the absorption of loads' latency into them (Mednafen's `GPR_DEP` and
    /// `GPR_RES`; register 0 stands for none, and its entry is kept).
    #[inline(always)]
    fn deps<const N: usize>(&mut self, regs: [usize; N]) {
        let absorb = &mut self.t.read_absorb;
        let back = absorb[0];
        for r in regs {
            absorb[r & 31] = 0;
        }
        absorb[0] = back;
    }

    /// `deps`, then the load in flight retires and lands (every instruction
    /// but a load).
    #[inline(always)]
    fn issue<const N: usize>(&mut self, regs: [usize; N]) {
        self.deps(regs);
        self.retire_timing();
        self.land();
    }

    /// A load (`loads`) into the register a load in flight is about to write
    /// cancels that load: `lw t1; lw t1` never shows the first value
    /// (Mednafen, DuckStation). Otherwise the load in flight lands.
    #[inline(always)]
    fn land_unless_reloaded(&mut self, loads: bool, rt: usize) {
        if loads && self.load.0 == rt {
            self.load = (0, 0);
        } else {
            self.land();
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
        self.bd |= bd::BRANCH | bd::TAKEN;
    }

    /// A conditional branch: its delay slot is one (Cause.BD) taken or not.
    fn cond_branch(&mut self, cond: bool, offset: u32) {
        if cond {
            self.rel_branch(offset);
        } else {
            self.bd |= bd::BRANCH;
        }
    }

    fn rel_branch(&mut self, offset: u32) {
        let target = self.pc.wrapping_add(sext16(offset) << 2);
        self.branch_to(target);
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
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
        // RAM, the common case, inline: no events, no I/O; `read_timed`'s
        // timing with RAM's 3 cycles.
        let a = segment_mask(addr);
        if a < 0x0080_0000 && addr & (w as u32 - 1) == 0 {
            self.cancel_absorb();
            self.cycles += ((self.t.read_fudge >> 4) & 2) as u64;
            let absorb = bus.dma.cycle_steal + 3 + if lwc { 1 } else { 2 };
            self.cycles += absorb;
            self.t.ld_absorb = absorb as u32;
            let v = bus.read_ram(a, w);
            info.mem(MemAccess { addr, value: v, width: w as u8, store: false });
            return Ok(v);
        }
        self.load_slow(bus, addr, w, pc, info, rt, bytes, lwc)
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn load_slow(
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

    #[inline(always)]
    fn store_mem(
        &mut self,
        bus: &mut Bus,
        addr: u32,
        w: Width,
        value: u32,
        pc: u32,
        info: &mut impl StepSink,
    ) -> Result<(), Exception> {
        // RAM, the common case, inline (with the cache not isolated).
        if segment_mask(addr) < 0x0080_0000 && addr & (w as u32 - 1) == 0 && self.cop0[cop0::SR] & (1 << 16) == 0 {
            bus.spin_breaks += 1;
            info.mem(MemAccess { addr, value, width: w as u8, store: true });
            return bus.write(addr, w, value).map_err(|_| {
                self.exception(Exception::DataBusError, pc);
                Exception::DataBusError
            });
        }
        self.store_slow(bus, addr, w, value, pc, info)
    }

    #[inline(never)]
    fn store_slow(
        &mut self,
        bus: &mut Bus,
        addr: u32,
        w: Width,
        value: u32,
        pc: u32,
        info: &mut impl StepSink,
    ) -> Result<(), Exception> {
        // I/O stores count in `Bus::write_slow` (some are pure).
        if !Bus::is_io(addr) {
            bus.spin_breaks += 1;
        }
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
        // Mednafen's `WriteMemory` writes the scratchpad itself, with no event check.
        if let Some(o) = crate::bus::scratch_offset(addr) {
            bus.write_scratch(o, w, value);
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

    #[inline(always)]
    fn execute(&mut self, bus: &mut Bus, i: u32, pc: u32, info: &mut impl StepSink) -> Result<(), Exception> {
        let op = i >> 26;
        let rs = ((i >> 21) & 31) as usize;
        let rt = ((i >> 16) & 31) as usize;
        // Decoded where used, not for every instruction.
        macro_rules! rd {
            () => {
                ((i >> 11) & 31) as usize
            };
        }
        macro_rules! shamt {
            () => {
                (i >> 6) & 31
            };
        }
        macro_rules! imm {
            () => {
                i & 0xFFFF
            };
        }
        macro_rules! simm {
            () => {
                sext16(i & 0xFFFF)
            };
        }
        let s = self.regs[rs];
        let t = self.regs[rt];
        match op {
            0x00 => match i & 0x3F {
                0x00 => {
                    self.issue([rt, rd!()]);
                    self.set_reg(rd!(), t << shamt!())
                }
                0x02 => {
                    self.issue([rt, rd!()]);
                    self.set_reg(rd!(), t >> shamt!())
                }
                0x03 => {
                    self.issue([rt, rd!()]);
                    self.set_reg(rd!(), ((t as i32) >> shamt!()) as u32)
                }
                0x04 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), t << (s & 31))
                }
                0x06 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), t >> (s & 31))
                }
                0x07 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), ((t as i32) >> (s & 31)) as u32)
                }
                0x08 => {
                    self.issue([rs, rd!()]);
                    self.branch_to(s)
                }
                0x09 => {
                    self.issue([rs, rd!()]);
                    self.set_reg(rd!(), self.next_pc);
                    self.branch_to(s);
                }
                0x0C => {
                    self.retire_timing();
                    return self.fail(Exception::Syscall, pc);
                }
                0x0D => {
                    self.retire_timing();
                    return self.fail(Exception::Break, pc);
                }
                0x10 => {
                    self.issue([rd!()]);
                    self.muldiv_wait();
                    self.set_reg(rd!(), self.hi)
                }
                0x11 => {
                    self.issue([rs]);
                    self.hi = s
                }
                0x12 => {
                    self.issue([rd!()]);
                    self.muldiv_wait();
                    self.set_reg(rd!(), self.lo)
                }
                0x13 => {
                    self.issue([rs]);
                    self.lo = s
                }
                0x18 => {
                    self.issue([rs, rt]);
                    self.t.muldiv_done =
                        self.cycles + mult_cycles((s ^ ((s as i32) >> 31) as u32 | 0x400).leading_zeros());
                    let p = (s as i32 as i64) * (t as i32 as i64);
                    self.hi = (p >> 32) as u32;
                    self.lo = p as u32;
                }
                0x19 => {
                    self.issue([rs, rt]);
                    self.t.muldiv_done = self.cycles + mult_cycles((s | 0x400).leading_zeros());
                    let p = (s as u64) * (t as u64);
                    self.hi = (p >> 32) as u32;
                    self.lo = p as u32;
                }
                0x1A => {
                    self.issue([rs, rt]);
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
                    self.issue([rs, rt]);
                    self.t.muldiv_done = self.cycles + 37;
                    if t == 0 {
                        self.hi = s;
                        self.lo = 0xFFFF_FFFF;
                    } else {
                        self.hi = s % t;
                        self.lo = s / t;
                    }
                }
                0x20 => {
                    self.issue([rs, rt, rd!()]);
                    match (s as i32).checked_add(t as i32) {
                        Some(v) => self.set_reg(rd!(), v as u32),
                        None => return self.fail(Exception::Overflow, pc),
                    }
                }
                0x21 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), s.wrapping_add(t))
                }
                0x22 => {
                    self.issue([rs, rt, rd!()]);
                    match (s as i32).checked_sub(t as i32) {
                        Some(v) => self.set_reg(rd!(), v as u32),
                        None => return self.fail(Exception::Overflow, pc),
                    }
                }
                0x23 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), s.wrapping_sub(t))
                }
                0x24 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), s & t)
                }
                0x25 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), s | t)
                }
                0x26 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), s ^ t)
                }
                0x27 => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), !(s | t))
                }
                0x2A => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), ((s as i32) < (t as i32)) as u32)
                }
                0x2B => {
                    self.issue([rs, rt, rd!()]);
                    self.set_reg(rd!(), (s < t) as u32)
                }
                _ => {
                    self.retire_timing();
                    return self.fail(Exception::ReservedInstruction, pc);
                }
            },
            0x01 => {
                // BLTZ/BGEZ(+AL): bit 16 selects GEZ; rt 0x10/0x11 link.
                let ge = (i >> 16) & 1 != 0;
                let link = (i >> 17) & 0xF == 0x8;
                let taken = ((s as i32) < 0) ^ ge;
                self.issue([rs, if link { 31 } else { 0 }]);
                if link {
                    self.set_reg(31, self.next_pc);
                }
                self.cond_branch(taken, imm!());
            }
            0x02 => {
                self.issue([]);
                self.branch_to((self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2))
            }
            0x03 => {
                self.issue([31]);
                self.set_reg(31, self.next_pc);
                self.branch_to((self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2));
            }
            0x04 => {
                self.issue([rs, rt]);
                self.cond_branch(s == t, imm!())
            }
            0x05 => {
                self.issue([rs, rt]);
                self.cond_branch(s != t, imm!())
            }
            0x06 => {
                self.issue([rs]);
                self.cond_branch((s as i32) <= 0, imm!())
            }
            0x07 => {
                self.issue([rs]);
                self.cond_branch((s as i32) > 0, imm!())
            }
            0x08 => {
                self.issue([rs, rt]);
                match (s as i32).checked_add(simm!() as i32) {
                    Some(v) => self.set_reg(rt, v as u32),
                    None => return self.fail(Exception::Overflow, pc),
                }
            }
            0x09 => {
                self.issue([rs, rt]);
                self.set_reg(rt, s.wrapping_add(simm!()))
            }
            0x0A => {
                self.issue([rs, rt]);
                self.set_reg(rt, ((s as i32) < (simm!() as i32)) as u32)
            }
            0x0B => {
                self.issue([rs, rt]);
                self.set_reg(rt, (s < simm!()) as u32)
            }
            0x0C => {
                self.issue([rs, rt]);
                self.set_reg(rt, s & imm!())
            }
            0x0D => {
                self.issue([rs, rt]);
                self.set_reg(rt, s | imm!())
            }
            0x0E => {
                self.issue([rs, rt]);
                self.set_reg(rt, s ^ imm!())
            }
            0x0F => {
                self.issue([rt]);
                self.set_reg(rt, imm!() << 16)
            }
            0x10 => {
                // MFC0 of these registers loads like a memory read.
                if rs == 0 && matches!(rd!(), 3 | 5..=9 | 0x0B..=0x0F) {
                    self.cancel_then_retire(rt as u8);
                } else {
                    self.retire_timing();
                }
                self.land_unless_reloaded(rs == 0, rt);
                self.cop0_op(i, pc, t)?
            }
            0x11 | 0x13 => {
                self.retire_timing();
                return self.fail(Exception::CoprocessorUnusable, pc);
            }
            0x12 => {
                // MFC2/CFC2 load (when COP2 is usable).
                bus.spin_breaks += 1;
                let loads = self.cop0[cop0::SR] & (1 << 30) != 0 && matches!(rs, 0 | 2);
                if loads {
                    self.cancel_then_retire(rt as u8);
                } else {
                    self.retire_timing();
                }
                self.land_unless_reloaded(loads, rt);
                self.cop2_op(i, pc, t)?
            }
            // Each with its width as a constant.
            0x20 => {
                self.load_op(bus, s.wrapping_add(simm!()), Width::Byte, pc, info, rs, rt, |v| v as u8 as i8 as u32)?
            }
            0x21 => {
                self.load_op(bus, s.wrapping_add(simm!()), Width::Half, pc, info, rs, rt, |v| v as u16 as i16 as u32)?
            }
            0x23 => self.load_op(bus, s.wrapping_add(simm!()), Width::Word, pc, info, rs, rt, |v| v)?,
            0x24 => self.load_op(bus, s.wrapping_add(simm!()), Width::Byte, pc, info, rs, rt, |v| v)?,
            0x25 => self.load_op(bus, s.wrapping_add(simm!()), Width::Half, pc, info, rs, rt, |v| v)?,
            0x22 | 0x26 => {
                // LWL/LWR merge with the in-flight value of rt (bypassing the load delay).
                let addr = s.wrapping_add(simm!());
                self.deps([rs]);
                // LWL/LWR into the register in flight merge with it instead.
                if self.t.ld_which == rt as u8 {
                    self.t.read_fudge = 0;
                } else {
                    self.retire_timing();
                }
                let k = addr & 3;
                let bytes = if op == 0x22 { k + 1 } else { 4 - k };
                let aligned = self.load_mem(bus, addr & !3, Width::Word, pc, info, rt, bytes, false)?;
                self.t.ld_which = rt as u8;
                // In-flight load to rt merges (LWL/LWR bypass the load delay).
                let cur = if self.load.0 == rt { self.load.1 } else { self.regs[rt] };
                self.land_unless_reloaded(true, rt);
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
            0x28 => {
                self.issue([rs, rt]);
                self.store_mem(bus, s.wrapping_add(simm!()), Width::Byte, t, pc, info)?
            }
            0x29 => {
                self.issue([rs, rt]);
                self.store_mem(bus, s.wrapping_add(simm!()), Width::Half, t, pc, info)?
            }
            0x2B => {
                self.issue([rs, rt]);
                self.store_mem(bus, s.wrapping_add(simm!()), Width::Word, t, pc, info)?
            }
            0x2A | 0x2E => {
                bus.spin_breaks += 1;
                self.issue([rs, rt]);
                let addr = s.wrapping_add(simm!());
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
                bus.spin_breaks += 1;
                self.issue([]);
                let addr = s.wrapping_add(simm!());
                if addr & 3 == 0 {
                    self.gte_wait();
                }
                let v = self.load_mem(bus, addr, Width::Word, pc, info, 0, 4, true)?;
                self.gte.write_data(rt, v);
            }
            0x3A => {
                self.issue([]);
                let addr = s.wrapping_add(simm!());
                if addr & 3 == 0 {
                    self.gte_wait();
                }
                let v = self.gte.read_data(rt);
                self.store_mem(bus, addr, Width::Word, v, pc, info)?;
            }
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => {
                self.retire_timing();
                return self.fail(Exception::CoprocessorUnusable, pc);
            }
            _ => {
                self.retire_timing();
                return self.fail(Exception::ReservedInstruction, pc);
            }
        }
        Ok(())
    }

    /// LB, LH, LW, LBU, LHU: `extend` sign-extends (or not) the value read.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn load_op(
        &mut self,
        bus: &mut Bus,
        addr: u32,
        w: Width,
        pc: u32,
        info: &mut impl StepSink,
        rs: usize,
        rt: usize,
        extend: impl Fn(u32) -> u32,
    ) -> Result<(), Exception> {
        self.deps([rs]);
        // A load into the register in flight cancels it (when it can issue).
        if addr & (w as u32 - 1) == 0 {
            self.cancel_then_retire(rt as u8);
        } else {
            self.retire_timing();
        }
        let v = self.load_mem(bus, addr, w, pc, info, rt, w as u32, false)?;
        self.land_unless_reloaded(true, rt);
        self.t.ld_which = rt as u8;
        self.load = (rt, extend(v));
        Ok(())
    }

    fn cop0_op(&mut self, i: u32, pc: u32, t: u32) -> Result<(), Exception> {
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
                let v = t;
                match rd {
                    cop0::CAUSE => self.cop0[rd] = (self.cop0[rd] & !0x300) | (v & 0x300),
                    cop0::PRID | cop0::BADVADDR => {}
                    _ => self.cop0[rd] = v,
                }
                self.refresh_int();
            }
            0x10 if i & 0x3F == 0x10 => {
                let sr = self.cop0[cop0::SR];
                self.cop0[cop0::SR] = (sr & !0xF) | ((sr >> 2) & 0xF);
                self.refresh_int();
            }
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        Ok(())
    }

    fn cop2_op(&mut self, i: u32, pc: u32, t: u32) -> Result<(), Exception> {
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
                self.gte.write_data(rd, t)
            }
            0x06 => {
                self.gte_wait();
                self.gte.write_ctrl(rd, t)
            }
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        Ok(())
    }

    /// What a polling loop's pass depends on in the CPU, with times relative
    /// to its clock (see `Machine::skip_spin`).
    pub(crate) fn spin_key(&self) -> SpinKey {
        let t = &self.t;
        SpinKey {
            regs: self.regs,
            hi: self.hi,
            lo: self.lo,
            pc: self.pc,
            next_pc: self.next_pc,
            load: self.load,
            bd: self.bd,
            cop0: self.cop0,
            absorb: t.read_absorb[..33].try_into().unwrap(),
            timing: [t.read_absorb_which, t.read_fudge, t.ld_which],
            ld_absorb: t.ld_absorb,
            waits: [t.gte_done.saturating_sub(self.cycles), t.muldiv_done.saturating_sub(self.cycles)],
            int: (self.int_pending, self.irq_line),
        }
    }

    /// [`Cpu::spin_key`] without the registers and the value of the load in
    /// flight, which a wait loop's passes step (see `crate::loops`).
    pub(crate) fn loop_key(&self) -> SpinKey {
        let mut k = self.spin_key();
        k.regs = [0; 32];
        k.load.1 = 0;
        k
    }

    /// `self.loop_key() == *k`, without building the key.
    pub(crate) fn loop_key_is(&self, k: &SpinKey) -> bool {
        let t = &self.t;
        let same = self.pc == k.pc
            && self.next_pc == k.next_pc
            && self.bd == k.bd
            && (self.int_pending, self.irq_line) == k.int
            && self.load.0 == k.load.0
            && [t.gte_done.saturating_sub(self.cycles), t.muldiv_done.saturating_sub(self.cycles)] == k.waits
            && (self.hi, self.lo) == (k.hi, k.lo)
            && [t.read_absorb_which, t.read_fudge, t.ld_which] == k.timing
            && t.ld_absorb == k.ld_absorb
            && t.read_absorb[..33] == k.absorb
            && self.cop0 == k.cop0;
        debug_assert_eq!(same, self.loop_key() == *k);
        same
    }

    /// Whether fetching `pc` now takes the time it took the last time: it
    /// hits the instruction cache, or bypasses it.
    pub(crate) fn fetches_alike(&self, bus: &Bus, pc: u32) -> bool {
        let enabled = bus.cache_control & 0x800 != 0;
        pc >= 0xA000_0000 || !enabled || self.t.icache[(pc as usize & 0xFFC) >> 2].0 == pc
    }

    /// The load in flight as it is held (its value stays after it lands).
    pub(crate) fn load_raw(&self) -> (usize, u32) {
        self.load
    }

    pub(crate) fn set_load_value(&mut self, v: u32) {
        self.load.1 = v;
    }

    /// The instruction the last step ran.
    pub(crate) fn last_instr(&self) -> u32 {
        self.instr
    }

    pub(crate) fn int_pending_cached(&self) -> bool {
        self.int_pending
    }

    /// Move the CPU's clock on by `cycles` of passes that changed nothing else.
    pub(crate) fn skip_cycles(&mut self, cycles: u64) {
        self.cycles += cycles;
        // Waits already over stay over; pending ones are equal in every pass.
        if self.t.gte_done > self.cycles - cycles {
            self.t.gte_done += cycles;
        }
        if self.t.muldiv_done > self.cycles - cycles {
            self.t.muldiv_done += cycles;
        }
    }

    /// The delayed load in flight `(register, value)`; register 0 is none.
    pub fn pending_load(&self) -> (usize, u32) {
        if self.load.0 == 0 { (0, 0) } else { self.load }
    }
}

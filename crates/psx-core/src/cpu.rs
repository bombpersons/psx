//! R3000A interpreter (psx-spx "CPU Specifications").
//!
//! Modelled: branch delay slots, load delay slots (including the LWL/LWR
//! merge with an in-flight load and the "later write wins" rule), COP0
//! exceptions with the KU/IE stack, RFE, interrupts gated by SR.IEc/IM and
//! Cause.IP, cache isolation (SR.IsC drops stores), overflow/address-error/
//! reserved-instruction/coprocessor-unusable exceptions, and COP2 via psx-gte.
//!
//! Not modelled yet: instruction cache, cycle-accurate timing (every
//! instruction costs one cycle), the GTE-command-vs-interrupt hardware quirk.

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
}

pub struct Cpu {
    pub regs: [u32; 32],
    pub hi: u32,
    pub lo: u32,
    pub pc: u32,
    next_pc: u32,
    /// Pending delayed load `(register, value)`.
    load: (usize, u32),
    branch: bool,
    delay_slot: bool,
    pub cop0: [u32; 32],
    pub gte: Gte,
    pub cycles: u64,
    /// External interrupt line (Cause.IP2), driven by the interrupt controller.
    pub irq_line: bool,
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
        // Most SR bits are undefined at reset; this is PCSX-Redux's value
        // (CU0 | BEV | TS) so lockstep comparison starts equal.
        cop0[cop0::SR] = 0x1090_0000;
        Cpu {
            regs: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0xBFC0_0000,
            next_pc: 0xBFC0_0004,
            load: (0, 0),
            branch: false,
            delay_slot: false,
            cop0,
            gte: Gte::new(),
            cycles: 0,
            irq_line: false,
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
    }

    /// The pending delayed load, if any.
    pub fn pending_load(&self) -> Option<(usize, u32)> {
        (self.load.0 != 0).then_some(self.load)
    }

    /// Override the value of an in-flight load (lockstep I/O injection).
    pub fn patch_pending_load(&mut self, reg: usize, value: u32) -> bool {
        if self.load.0 == reg && reg != 0 {
            self.load.1 = value;
            true
        } else {
            false
        }
    }

    #[inline]
    fn set_reg(&mut self, out: &mut [u32; 32], r: usize, v: u32) {
        out[r] = v;
        out[0] = 0;
        let _ = self;
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
    pub fn interrupt_pending(&self) -> bool {
        let sr = self.cop0[cop0::SR];
        sr & 1 != 0 && (sr & self.cause_ip() & 0xFF00) != 0
    }

    fn exception(&mut self, cause: Exception, epc_pc: u32) {
        let sr = self.cop0[cop0::SR];
        // Push the KU/IE stack (bits 0..5 shift left by two).
        self.cop0[cop0::SR] = (sr & !0x3F) | ((sr << 2) & 0x3F);
        let mut c = self.cause_ip() & !0x8000_007C;
        c |= (cause as u32) << 2;
        let handler = if sr & (1 << 22) != 0 { 0xBFC0_0180 } else { 0x8000_0080 };
        if self.delay_slot {
            self.cop0[cop0::EPC] = epc_pc.wrapping_sub(4);
            c |= 1 << 31;
        } else {
            self.cop0[cop0::EPC] = epc_pc;
        }
        self.cop0[cop0::CAUSE] = c;
        self.pc = handler;
        self.next_pc = handler.wrapping_add(4);
    }

    /// Execute one instruction (or take a pending interrupt).
    pub fn step(&mut self, bus: &mut Bus) -> StepInfo {
        self.cycles += 1;
        let pc = self.pc;
        self.cop0[cop0::CAUSE] = self.cause_ip();
        self.delay_slot = self.branch;
        self.branch = false;

        if pc & 3 != 0 {
            self.flush_load();
            self.cop0[cop0::BADVADDR] = pc;
            self.exception(Exception::LoadAddressError, pc);
            return StepInfo { pc, instruction: 0, exception: Some(Exception::LoadAddressError as u8), io_load: None };
        }
        let instruction = match bus.read(pc, Width::Word) {
            Ok(i) => i,
            Err(_) => {
                self.flush_load();
                self.exception(Exception::InstructionBusError, pc);
                return StepInfo { pc, instruction: 0, exception: Some(Exception::InstructionBusError as u8), io_load: None };
            }
        };
        if self.interrupt_pending() {
            // EPC is the instruction about to execute; it runs after RFE.
            self.flush_load();
            self.exception(Exception::Interrupt, pc);
            return StepInfo { pc, instruction, exception: Some(Exception::Interrupt as u8), io_load: None };
        }

        self.pc = self.next_pc;
        self.next_pc = self.next_pc.wrapping_add(4);

        let mut out = self.regs;
        let (lr, lv) = self.load;
        out[lr] = lv;
        out[0] = 0;
        self.load = (0, 0);

        let mut info = StepInfo { pc, instruction, exception: None, io_load: None };
        if let Err(e) = self.execute(bus, instruction, pc, &mut out, &mut info) {
            info.exception = Some(e as u8);
        }
        self.regs = out;
        self.regs[0] = 0;
        info
    }

    fn branch_to(&mut self, target: u32) {
        self.next_pc = target;
        self.branch = true;
    }

    fn rel_branch(&mut self, offset: u32) {
        let target = self.pc.wrapping_add(sext16(offset) << 2);
        self.branch_to(target);
    }

    fn load_mem(&mut self, bus: &mut Bus, addr: u32, w: Width, pc: u32, info: &mut StepInfo, rt: usize) -> Result<u32, Exception> {
        if !addr.is_multiple_of(w as u32) {
            self.cop0[cop0::BADVADDR] = addr;
            self.exception(Exception::LoadAddressError, pc);
            return Err(Exception::LoadAddressError);
        }
        if Bus::is_io(addr) {
            info.io_load = Some((addr, rt as u8));
        }
        bus.read(addr, w).map_err(|_| {
            self.exception(Exception::DataBusError, pc);
            Exception::DataBusError
        })
    }

    fn store_mem(&mut self, bus: &mut Bus, addr: u32, w: Width, value: u32, pc: u32) -> Result<(), Exception> {
        if !addr.is_multiple_of(w as u32) {
            self.cop0[cop0::BADVADDR] = addr;
            self.exception(Exception::StoreAddressError, pc);
            return Err(Exception::StoreAddressError);
        }
        if self.cop0[cop0::SR] & (1 << 16) != 0 {
            return Ok(()); // cache isolated: the store hits the (unmodelled) cache only
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

    fn execute(&mut self, bus: &mut Bus, i: u32, pc: u32, out: &mut [u32; 32], info: &mut StepInfo) -> Result<(), Exception> {
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
                0x00 => self.set_reg(out, rd, t << shamt),
                0x02 => self.set_reg(out, rd, t >> shamt),
                0x03 => self.set_reg(out, rd, ((t as i32) >> shamt) as u32),
                0x04 => self.set_reg(out, rd, t << (s & 31)),
                0x06 => self.set_reg(out, rd, t >> (s & 31)),
                0x07 => self.set_reg(out, rd, ((t as i32) >> (s & 31)) as u32),
                0x08 => self.branch_to(s),
                0x09 => {
                    self.set_reg(out, rd, self.next_pc);
                    self.branch_to(s);
                }
                0x0C => return self.fail(Exception::Syscall, pc),
                0x0D => return self.fail(Exception::Break, pc),
                0x10 => self.set_reg(out, rd, self.hi),
                0x11 => self.hi = s,
                0x12 => self.set_reg(out, rd, self.lo),
                0x13 => self.lo = s,
                0x18 => {
                    let p = (s as i32 as i64) * (t as i32 as i64);
                    self.hi = (p >> 32) as u32;
                    self.lo = p as u32;
                }
                0x19 => {
                    let p = (s as u64) * (t as u64);
                    self.hi = (p >> 32) as u32;
                    self.lo = p as u32;
                }
                0x1A => {
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
                    if t == 0 {
                        self.hi = s;
                        self.lo = 0xFFFF_FFFF;
                    } else {
                        self.hi = s % t;
                        self.lo = s / t;
                    }
                }
                0x20 => match (s as i32).checked_add(t as i32) {
                    Some(v) => self.set_reg(out, rd, v as u32),
                    None => return self.fail(Exception::Overflow, pc),
                },
                0x21 => self.set_reg(out, rd, s.wrapping_add(t)),
                0x22 => match (s as i32).checked_sub(t as i32) {
                    Some(v) => self.set_reg(out, rd, v as u32),
                    None => return self.fail(Exception::Overflow, pc),
                },
                0x23 => self.set_reg(out, rd, s.wrapping_sub(t)),
                0x24 => self.set_reg(out, rd, s & t),
                0x25 => self.set_reg(out, rd, s | t),
                0x26 => self.set_reg(out, rd, s ^ t),
                0x27 => self.set_reg(out, rd, !(s | t)),
                0x2A => self.set_reg(out, rd, ((s as i32) < (t as i32)) as u32),
                0x2B => self.set_reg(out, rd, (s < t) as u32),
                _ => return self.fail(Exception::ReservedInstruction, pc),
            },
            0x01 => {
                // BLTZ/BGEZ(+AL): bit 16 selects GEZ; rt 0x10/0x11 link.
                let ge = (i >> 16) & 1 != 0;
                let link = (i >> 17) & 0xF == 0x8;
                let taken = ((s as i32) < 0) ^ ge;
                if link {
                    self.set_reg(out, 31, self.next_pc);
                }
                if taken {
                    self.rel_branch(imm);
                }
            }
            0x02 => self.branch_to((self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2)),
            0x03 => {
                self.set_reg(out, 31, self.next_pc);
                self.branch_to((self.pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2));
            }
            0x04 => {
                if s == t {
                    self.rel_branch(imm)
                }
            }
            0x05 => {
                if s != t {
                    self.rel_branch(imm)
                }
            }
            0x06 => {
                if (s as i32) <= 0 {
                    self.rel_branch(imm)
                }
            }
            0x07 => {
                if (s as i32) > 0 {
                    self.rel_branch(imm)
                }
            }
            0x08 => match (s as i32).checked_add(simm as i32) {
                Some(v) => self.set_reg(out, rt, v as u32),
                None => return self.fail(Exception::Overflow, pc),
            },
            0x09 => self.set_reg(out, rt, s.wrapping_add(simm)),
            0x0A => self.set_reg(out, rt, ((s as i32) < (simm as i32)) as u32),
            0x0B => self.set_reg(out, rt, (s < simm) as u32),
            0x0C => self.set_reg(out, rt, s & imm),
            0x0D => self.set_reg(out, rt, s | imm),
            0x0E => self.set_reg(out, rt, s ^ imm),
            0x0F => self.set_reg(out, rt, imm << 16),
            0x10 => self.cop0_op(i, pc, out)?,
            0x11 | 0x13 => return self.fail(Exception::CoprocessorUnusable, pc),
            0x12 => self.cop2_op(i, pc, out)?,
            0x20 | 0x21 | 0x23 | 0x24 | 0x25 => {
                let addr = s.wrapping_add(simm);
                let w = match op {
                    0x20 | 0x24 => Width::Byte,
                    0x21 | 0x25 => Width::Half,
                    _ => Width::Word,
                };
                let v = self.load_mem(bus, addr, w, pc, info, rt)?;
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
                let aligned = self.load_mem(bus, addr & !3, Width::Word, pc, info, rt)?;
                let cur = out[rt];
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
            0x28 => self.store_mem(bus, s.wrapping_add(simm), Width::Byte, t, pc)?,
            0x29 => self.store_mem(bus, s.wrapping_add(simm), Width::Half, t, pc)?,
            0x2B => self.store_mem(bus, s.wrapping_add(simm), Width::Word, t, pc)?,
            0x2A | 0x2E => {
                let addr = s.wrapping_add(simm);
                let aligned_addr = addr & !3;
                let mem = if self.cop0[cop0::SR] & (1 << 16) != 0 {
                    return Ok(());
                } else {
                    bus.read(aligned_addr, Width::Word).unwrap_or(0)
                };
                let sh = (addr & 3) * 8;
                let v = if op == 0x2A {
                    let keep = 0xFFFF_FF00u32.checked_shl(sh).unwrap_or(0);
                    (mem & keep) | (t >> (24 - sh))
                } else {
                    let keep = 0x00FF_FFFFu32.checked_shr(24 - sh).unwrap_or(0);
                    (mem & keep) | (t << sh)
                };
                self.store_mem(bus, aligned_addr, Width::Word, v, pc)?;
            }
            0x32 => {
                let addr = s.wrapping_add(simm);
                let v = self.load_mem(bus, addr, Width::Word, pc, info, 0)?;
                self.gte.write_data(rt, v);
            }
            0x3A => {
                let addr = s.wrapping_add(simm);
                let v = self.gte.read_data(rt);
                self.store_mem(bus, addr, Width::Word, v, pc)?;
            }
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => return self.fail(Exception::CoprocessorUnusable, pc),
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        Ok(())
    }

    fn cop0_op(&mut self, i: u32, pc: u32, out: &mut [u32; 32]) -> Result<(), Exception> {
        let rt = ((i >> 16) & 31) as usize;
        let rd = ((i >> 11) & 31) as usize;
        match (i >> 21) & 31 {
            0x00 => {
                let v = if rd == cop0::CAUSE { self.cause_ip() } else { self.cop0[rd] };
                self.load = (rt, v);
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
        let _ = out;
        Ok(())
    }

    fn cop2_op(&mut self, i: u32, pc: u32, out: &mut [u32; 32]) -> Result<(), Exception> {
        if self.cop0[cop0::SR] & (1 << 30) == 0 {
            return self.fail(Exception::CoprocessorUnusable, pc);
        }
        let rt = ((i >> 16) & 31) as usize;
        let rd = ((i >> 11) & 31) as usize;
        if i & (1 << 25) != 0 {
            self.gte.execute(i & 0x01FF_FFFF);
            return Ok(());
        }
        match (i >> 21) & 31 {
            0x00 => self.load = (rt, self.gte.read_data(rd)),
            0x02 => self.load = (rt, self.gte.read_ctrl(rd)),
            0x04 => self.gte.write_data(rd, self.regs[rt]),
            0x06 => self.gte.write_ctrl(rd, self.regs[rt]),
            _ => return self.fail(Exception::ReservedInstruction, pc),
        }
        let _ = out;
        Ok(())
    }

    /// Raise an external interrupt now (lockstep: follow an IRQ the oracle took).
    pub fn force_interrupt(&mut self) {
        self.flush_load();
        let pc = self.pc;
        self.delay_slot = self.branch;
        self.cop0[cop0::CAUSE] |= 0x400;
        self.exception(Exception::Interrupt, pc);
        self.branch = false;
    }
}

//! Where a game keeps its pad: the controller's reply followed from the
//! serial port into RAM, and the game's reads of it.
//!
//! A load from JOY_DATA during a pad poll ([`crate::sio0::DataRead`]) marks
//! the bytes of its destination register with the reply byte it returned
//! (the poll's exchange and the byte's position: 3 and 4 are the buttons,
//! 5 to 8 the sticks). Each register is four byte lanes. Moves, arithmetic
//! and logic (`or`, `nor`, `xori`, `addiu`, ...) carry the marks lane by
//! lane, so a halfword assembled from two bytes and inverted keeps both;
//! shifts move them between lanes; `andi` keeps the lanes its mask touches;
//! any other write to a register clears it. A store of a marked register
//! marks the RAM bytes it writes, and remembers whether an exception handler
//! wrote them. The bytes an exception handler wrote from the latest poll are
//! the **pad buffer**: the PsyQ library's, the BIOS's or the game's own,
//! found with no presses scripted. A load of the buffer is a **read**: the
//! game consuming its pad, or inside a handler the driver copying it on or
//! a game reading it in a callback (the two look alike). A game that keeps the reply in
//! registers has no buffer; its loads from JOY_DATA outside a handler are
//! its reads.
//!
//! Exceptions are tracked from the step that takes one to the `rfe` that
//! returns. Only what the CPU does is followed: DMA into RAM is not.

use std::collections::BTreeMap;

use crate::cpu::StepInfo;
use crate::sio0::Sio0;

/// A byte of a poll's reply: its exchange and position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Mark {
    pub exchange: u64,
    pub pos: u8,
    /// The byte the controller sent.
    pub value: u8,
}

type Lanes = [Option<Mark>; 4];

/// A RAM byte holding a reply byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    pub mark: Mark,
    /// Written by an exception handler (the poll's own copy), not the game's
    /// main code (a copy the game made of it).
    pub by_handler: bool,
    /// The byte stored, XOR the byte the controller sent: 0 for a copy,
    /// 0xFF for one inverted.
    pub mask: u8,
}

/// A read of the pad: a load of the bytes a handler stored from a poll, or
/// of JOY_DATA outside a handler.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PadRead {
    pub pc: u32,
    /// The address read: a RAM address (KSEG0) or JOY_DATA's.
    pub addr: u32,
    /// The reply bytes it read.
    pub marks: Vec<Mark>,
    /// Made inside an exception handler: the driver copying the bytes on,
    /// or a game reading its pad in a callback the handler runs (VSync's).
    pub in_handler: bool,
}

/// One byte of the pad buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BufferByte {
    /// KSEG0 for main RAM, the physical address for the scratchpad.
    pub addr: u32,
    pub pos: u8,
    /// The byte stored XOR the byte sent, when every store of it so far
    /// kept the same: 0 for a copy, 0xFF inverted. None when the game
    /// changes the byte in other ways.
    pub mask: Option<u8>,
}

/// Follows reply bytes from the serial port; feed it every step.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct PadFlow {
    regs: [Lanes; 32],
    /// By physical address (main RAM's mirrors folded).
    ram: BTreeMap<u32, Held>,
    in_handler: bool,
    /// Each handler-written byte's mask, while every store of it kept one.
    masks: BTreeMap<u32, Option<u8>>,
    /// Reads not yet taken by [`PadFlow::take_reads`].
    reads: Vec<PadRead>,
}

const JOY_DATA: u32 = 0x1F80_1040;

/// The physical address, main RAM's mirrors folded.
fn fold(addr: u32) -> u32 {
    let p = addr & 0x1FFF_FFFF;
    if p < 0x0080_0000 { p & 0x1F_FFFF } else { p }
}

/// A physical address as code would address it: KSEG0 for main RAM.
fn shown(p: u32) -> u32 {
    if p < 0x0080_0000 { 0x8000_0000 | p } else { p }
}

fn union(a: Lanes, b: Lanes) -> Lanes {
    [0, 1, 2, 3].map(|i| a[i].or(b[i]))
}

/// `l` shifted left (`left`) or right by `sa` bits: a mark moves with its
/// bits, onto both lanes it straddles when `sa` is not a whole byte.
fn shift(l: Lanes, sa: u32, left: bool) -> Lanes {
    let mut out = [None; 4];
    for (i, m) in l.iter().enumerate() {
        let Some(m) = m else { continue };
        let bit = i as i32 * 8 + if left { sa as i32 } else { -(sa as i32) };
        for lane in [bit.div_euclid(8), (bit + 7).div_euclid(8)] {
            if (0..4).contains(&lane) {
                out[lane as usize].get_or_insert(*m);
            }
        }
    }
    out
}

impl PadFlow {
    pub fn new() -> PadFlow {
        PadFlow::default()
    }

    /// Whether the last step left the CPU in an exception handler.
    pub fn in_handler(&self) -> bool {
        self.in_handler
    }

    /// The step `info` just ran; `sio` is the serial port after it.
    pub fn step(&mut self, info: &StepInfo, sio: &Sio0) {
        if info.exception.is_some() {
            // Taken instead of the instruction: nothing else happened.
            self.in_handler = true;
            return;
        }
        let i = info.instruction;
        if i == 0x4200_0010 {
            // rfe
            self.in_handler = false;
            return;
        }
        let (op, rs, rt, rd) = (i >> 26, (i >> 21 & 31) as usize, (i >> 16 & 31) as usize, (i >> 11 & 31) as usize);
        if let Some((addr, reg)) = info.io_load {
            let reg = reg as usize;
            self.regs[reg] = [None; 4];
            let read = sio.last_read.filter(|r| r.is_poll());
            if let (true, Some(r)) = ((JOY_DATA..JOY_DATA + 4).contains(&fold(addr)), read) {
                let mark = Mark { exchange: r.exchange, pos: r.pos, value: r.value };
                // A narrow load's byte lands in lane 0; a word repeats the byte.
                let width = info.mem.map_or(1, |m| m.width as usize);
                for lane in self.regs[reg].iter_mut().take(width) {
                    *lane = Some(mark);
                }
                if !self.in_handler {
                    self.reads.push(PadRead { pc: info.pc, addr: JOY_DATA, marks: vec![mark], in_handler: false });
                }
            }
            return;
        }
        let imm = i & 0xFFFF;
        match op {
            0 => {
                let funct = i & 0x3F;
                let sa = i >> 6 & 31;
                match funct {
                    0x00 | 0x02 | 0x03 => self.set(rd, shift(self.regs[rt], sa, funct == 0)),
                    0x04 | 0x06 | 0x07 => self.set(rd, self.regs[rt]),
                    0x09 | 0x10 | 0x12 | 0x2A | 0x2B => self.set(rd, [None; 4]),
                    0x20..=0x27 => self.set(rd, union(self.regs[rs], self.regs[rt])),
                    _ => {}
                }
            }
            // bltzal, bgezal (taken or not) write ra.
            1 if rt & 0x1E == 0x10 => self.set(31, [None; 4]),
            3 => self.set(31, [None; 4]),
            0x08 | 0x09 | 0x0D | 0x0E => self.set(rt, self.regs[rs]),
            0x0C => {
                let l = self.regs[rs];
                self.set(
                    rt,
                    [
                        (imm & 0xFF != 0).then_some(l[0]).flatten(),
                        (imm >> 8 != 0).then_some(l[1]).flatten(),
                        None,
                        None,
                    ],
                );
            }
            0x0A | 0x0B | 0x0F => self.set(rt, [None; 4]),
            // mfc0, cfc0; mfc2, cfc2.
            0x10 | 0x12 if rs == 0 || rs == 2 => self.set(rt, [None; 4]),
            0x20 | 0x21 | 0x23 | 0x24 | 0x25 => self.load(info, rt),
            0x22 | 0x26 => self.set(rt, [None; 4]),
            0x28 | 0x29 | 0x2B => self.store(info, self.regs[rt]),
            0x2A | 0x2E => self.store(info, [None; 4]),
            _ => {}
        }
    }

    fn set(&mut self, r: usize, l: Lanes) {
        if r != 0 {
            self.regs[r] = l;
        }
    }

    fn load(&mut self, info: &StepInfo, rt: usize) {
        let Some(m) = info.mem else {
            self.set(rt, [None; 4]);
            return;
        };
        let p = fold(m.addr);
        let mut l = [None; 4];
        let mut buffer = Vec::new();
        for (k, lane) in l.iter_mut().enumerate().take(m.width as usize) {
            if let Some(h) = self.ram.get(&(p + k as u32)) {
                *lane = Some(h.mark);
                if h.by_handler {
                    buffer.push(h.mark);
                }
            }
        }
        if !buffer.is_empty() {
            self.reads.push(PadRead { pc: info.pc, addr: shown(p), marks: buffer, in_handler: self.in_handler });
        }
        self.set(rt, l);
    }

    fn store(&mut self, info: &StepInfo, l: Lanes) {
        let Some(m) = info.mem else { return };
        let p = fold(m.addr);
        for k in 0..m.width as usize {
            let a = p + k as u32;
            match l.get(k).copied().flatten() {
                Some(mark) => {
                    let mask = (m.value >> (8 * k)) as u8 ^ mark.value;
                    self.ram.insert(a, Held { mark, by_handler: self.in_handler, mask });
                    if self.in_handler {
                        let kept = self.masks.entry(a).or_insert(Some(mask));
                        if *kept != Some(mask) {
                            *kept = None;
                        }
                    }
                }
                None => {
                    self.ram.remove(&a);
                }
            }
        }
    }

    /// The pad buffer: the RAM bytes an exception handler wrote from the
    /// latest poll it copied, by address. Empty when a handler copied none.
    pub fn buffer(&self) -> Vec<BufferByte> {
        let held = |by_handler| self.ram.iter().filter(move |(_, h)| h.by_handler == by_handler);
        let Some(latest) = held(true).map(|(_, h)| h.mark.exchange).max() else {
            return Vec::new();
        };
        held(true)
            .filter(|(_, h)| h.mark.exchange == latest)
            .map(|(a, h)| BufferByte { addr: shown(*a), pos: h.mark.pos, mask: self.masks.get(a).copied().flatten() })
            .collect()
    }

    /// Bytes a handler stored from the few polls before the buffer's: a
    /// copy the driver makes of a reply a poll later, often the one the
    /// game reads. By address.
    pub fn handler_copies(&self) -> Vec<BufferByte> {
        let Some(latest) = self.ram.values().filter(|h| h.by_handler).map(|h| h.mark.exchange).max() else {
            return Vec::new();
        };
        self.ram
            .iter()
            .filter(|(_, h)| h.by_handler && h.mark.exchange < latest && h.mark.exchange + 4 >= latest)
            .map(|(a, h)| BufferByte { addr: shown(*a), pos: h.mark.pos, mask: self.masks.get(a).copied().flatten() })
            .collect()
    }

    /// Copies of reply bytes the game's main code made (a game latching its
    /// pad into its own variables), by address, from the latest poll copied.
    pub fn copies(&self) -> Vec<BufferByte> {
        let main = || self.ram.iter().filter(|(_, h)| !h.by_handler);
        let Some(latest) = main().map(|(_, h)| h.mark.exchange).max() else {
            return Vec::new();
        };
        main()
            .filter(|(_, h)| h.mark.exchange == latest)
            .map(|(a, h)| BufferByte { addr: shown(*a), pos: h.mark.pos, mask: self.masks.get(a).copied().flatten() })
            .collect()
    }

    /// The reads since the last call.
    pub fn take_reads(&mut self) -> Vec<PadRead> {
        std::mem::take(&mut self.reads)
    }
}

/// Test programs that poll the pad, for psx-core's tests and psx's.
#[doc(hidden)]
pub mod test_program {
    use super::*;
    use crate::Machine;

    // Registers.
    const ZERO: u32 = 0;
    const T0: u32 = 8;
    const T1: u32 = 9;
    const T2: u32 = 10;
    const T3: u32 = 11;
    const T4: u32 = 12;
    const T5: u32 = 13;
    const T6: u32 = 14;
    const S0: u32 = 16;
    const S1: u32 = 17;
    const K0: u32 = 26;

    /// A few MIPS instructions, for test programs.
    pub mod asm {
        pub fn i(op: u32, rs: u32, rt: u32, imm: u32) -> u32 {
            op << 26 | rs << 21 | rt << 16 | (imm & 0xFFFF)
        }
        pub fn r(rs: u32, rt: u32, rd: u32, sa: u32, funct: u32) -> u32 {
            rs << 21 | rt << 16 | rd << 11 | sa << 6 | funct
        }
        pub fn lui(rt: u32, imm: u32) -> u32 {
            i(0x0F, 0, rt, imm)
        }
        pub fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
            i(0x0D, rs, rt, imm)
        }
        pub fn addiu(rt: u32, rs: u32, imm: u32) -> u32 {
            i(0x09, rs, rt, imm)
        }
        pub fn andi(rt: u32, rs: u32, imm: u32) -> u32 {
            i(0x0C, rs, rt, imm)
        }
        pub fn lbu(rt: u32, off: u32, base: u32) -> u32 {
            i(0x24, base, rt, off)
        }
        pub fn lhu(rt: u32, off: u32, base: u32) -> u32 {
            i(0x25, base, rt, off)
        }
        pub fn sb(rt: u32, off: u32, base: u32) -> u32 {
            i(0x28, base, rt, off)
        }
        pub fn sh(rt: u32, off: u32, base: u32) -> u32 {
            i(0x29, base, rt, off)
        }
        pub fn sll(rd: u32, rt: u32, sa: u32) -> u32 {
            r(0, rt, rd, sa, 0)
        }
        pub fn or(rd: u32, rs: u32, rt: u32) -> u32 {
            r(rs, rt, rd, 0, 0x25)
        }
        pub fn nor(rd: u32, rs: u32, rt: u32) -> u32 {
            r(rs, rt, rd, 0, 0x27)
        }
        /// A branch from index `from` to index `to` of the same program.
        pub fn beq(rs: u32, rt: u32, from: usize, to: usize) -> u32 {
            i(0x04, rs, rt, (to as i32 - from as i32 - 1) as u32)
        }
        pub fn j(target: u32) -> u32 {
            0x02 << 26 | (target >> 2 & 0x03FF_FFFF)
        }
        pub fn jr(rs: u32) -> u32 {
            r(rs, 0, 0, 0, 0x08)
        }
        pub fn mfc0(rt: u32, rd: u32) -> u32 {
            0x10 << 26 | rt << 16 | rd << 11
        }
        pub const SYSCALL: u32 = 0x0C;
        pub const RFE: u32 = 0x4200_0010;
        pub const NOP: u32 = 0;
    }
    use asm::*;

    /// How a test program keeps the pad.
    #[derive(Clone, Copy, PartialEq)]
    pub enum Keep {
        /// Each button byte stored on its own.
        Bytes,
        /// The two bytes made a halfword, inverted, and stored.
        Inverted,
        /// Nothing stored: the main code polls and branches on the reply.
        Registers,
    }

    /// A poll of the pad: select port 1, exchange `01 42 00 00 00`, keeping
    /// the button bytes as `keep` says (at 0x80020000 for `Bytes` and
    /// `Inverted`), deselect.
    fn poll(keep: Keep) -> Vec<u32> {
        let mut p = vec![
            lui(S0, 0x1F80),
            ori(S0, S0, 0x1040),
            addiu(T0, ZERO, 0x0D),
            sh(T0, 8, S0),
            addiu(T0, ZERO, 0x88),
            sh(T0, 0xE, S0),
            ori(T0, ZERO, 0x1003),
            sh(T0, 0xA, S0),
            // A read of JOY_DATA makes the receiver wait for the next byte.
            lbu(T0, 0, S0),
            lui(S1, 0x8002),
        ];
        for (pos, tx) in [0x01, 0x42, 0, 0, 0].into_iter().enumerate() {
            p.extend([addiu(T0, ZERO, tx), sb(T0, 0, S0)]);
            let wait = p.len();
            p.extend([lhu(T1, 4, S0), NOP, andi(T1, T1, 2)]);
            p.extend([beq(T1, ZERO, p.len(), wait), NOP, lbu(T2, 0, S0), NOP]);
            match (keep, pos) {
                (Keep::Bytes, 3) => p.push(sb(T2, 0, S1)),
                (Keep::Bytes, 4) => p.push(sb(T2, 1, S1)),
                (Keep::Inverted, 3) => p.push(or(T3, T2, ZERO)),
                (Keep::Inverted, 4) => p.extend([sll(T4, T2, 8), or(T3, T3, T4), nor(T3, T3, ZERO), sh(T3, 0, S1)]),
                (Keep::Registers, 3) => p.push(andi(T5, T2, 0xFF)),
                _ => {}
            }
        }
        p.push(sh(ZERO, 0xA, S0));
        p
    }

    /// A machine with a test program: the poll in the exception handler
    /// (entered by `syscall`), the main code reading what it kept, in a
    /// loop; or, for [`Keep::Registers`], the main code polling itself.
    pub fn machine(keep: Keep, buttons: u16) -> Machine {
        let mut m = if keep == Keep::Registers {
            let mut p = poll(keep);
            p.extend([j(0x8001_0000), NOP]);
            with_main(Keep::Bytes, &p, 0x8001_0000)
        } else {
            let main = [lui(S1, 0x8002), SYSCALL, lbu(T5, 0, S1), lbu(T6, 1, S1), j(0x8001_0004), NOP];
            with_main(keep, &main, 0x8001_0000)
        };
        m.bus.sio0.buttons = buttons;
        m
    }

    /// A machine whose exception handler (entered by `syscall`) polls the
    /// pad, keeping it as `keep` says, and whose main code is `main` at
    /// 0x80010000, starting at `pc`.
    pub fn with_main(keep: Keep, main: &[u32], pc: u32) -> Machine {
        let mut m = Machine::new(vec![0; 0x8_0000]);
        let mut handler = poll(keep);
        handler.extend([mfc0(K0, 14), NOP, addiu(K0, K0, 4), jr(K0), RFE]);
        load(&mut m, 0x8000_0080, &handler);
        load(&mut m, 0x8001_0000, main);
        m.cpu.cop0[crate::cpu::cop0::SR] = 0;
        m.cpu.set_pc(pc);
        m
    }

    /// A machine whose handler copies the last poll's buttons_lo from
    /// 0x80020000 to 0x80020100, then polls into 0x80020000 (`Keep::Bytes`);
    /// its main code never reads.
    pub fn copied_a_poll_later(buttons: u16) -> Machine {
        let mut m = Machine::new(vec![0; 0x8_0000]);
        let mut handler = vec![lui(S1, 0x8002), lbu(T2, 0, S1), NOP, sb(T2, 0x100, S1)];
        handler.extend(poll(Keep::Bytes));
        handler.extend([mfc0(K0, 14), NOP, addiu(K0, K0, 4), jr(K0), RFE]);
        load(&mut m, 0x8000_0080, &handler);
        load(&mut m, 0x8001_0000, &[SYSCALL, j(0x8001_0000), NOP]);
        m.cpu.cop0[crate::cpu::cop0::SR] = 0;
        m.cpu.set_pc(0x8001_0000);
        m.bus.sio0.buttons = buttons;
        m
    }

    /// `code` into RAM at `at`.
    pub fn load(m: &mut Machine, at: u32, code: &[u32]) {
        let at = (at & 0x1F_FFFF) as usize;
        for (k, w) in code.iter().enumerate() {
            m.bus.ram[at + 4 * k..][..4].copy_from_slice(&w.to_le_bytes());
        }
    }

    /// Runs `m` with a [`PadFlow`] until `polls` polls completed.
    pub fn run(m: &mut Machine, polls: usize) -> (PadFlow, Vec<PadRead>) {
        let mut f = PadFlow::new();
        let mut reads = Vec::new();
        m.bus.sio0.polls = Some(Vec::new());
        for _ in 0..2_000_000 {
            let info = m.step();
            f.step(&info, &m.bus.sio0);
            reads.extend(f.take_reads());
            if m.bus.sio0.polls.as_ref().unwrap().len() >= polls {
                break;
            }
        }
        assert!(m.bus.sio0.polls.as_ref().unwrap().len() >= polls, "the program polled too few times");
        (f, reads)
    }
}

#[cfg(test)]
mod tests {
    use super::test_program::*;
    use super::*;
    use crate::sio0::button;

    #[test]
    fn polls_are_logged_with_the_buttons_held() {
        let mut m = machine(Keep::Bytes, button::CROSS | button::START);
        run(&mut m, 2);
        let polls = m.bus.sio0.polls.take().unwrap();
        assert_eq!(polls.len(), 2);
        assert_eq!((polls[0].id, polls[0].buttons, polls[0].sticks), (0x41, button::CROSS | button::START, None));
        assert!(polls[1].exchange > polls[0].exchange);
    }

    #[test]
    fn a_handler_storing_the_bytes_is_the_buffer() {
        let mut m = machine(Keep::Bytes, button::CROSS);
        let (f, reads) = run(&mut m, 3);
        let b = f.buffer();
        let copy = Some(0);
        assert_eq!(
            b,
            [
                BufferByte { addr: 0x8002_0000, pos: 3, mask: copy },
                BufferByte { addr: 0x8002_0001, pos: 4, mask: copy }
            ]
        );
        assert!(f.copies().is_empty());
        // The main code's two loads read it, each poll.
        let main: Vec<_> = reads.iter().map(|r| (r.pc, r.addr, r.marks[0].pos)).collect();
        assert!(main.contains(&(0x8001_0008, 0x8002_0000, 3)), "{main:x?}");
        assert!(main.contains(&(0x8001_000C, 0x8002_0001, 4)), "{main:x?}");
        // The handler's own loads from JOY_DATA are not reads.
        assert!(reads.iter().all(|r| r.addr != JOY_DATA));
    }

    #[test]
    fn a_copy_the_handler_makes_a_poll_later_is_kept_and_its_loads_are_handler_reads() {
        // The handler copies the last poll's buttons_lo to 0x80020100, then
        // polls into 0x80020000; the main code never reads.
        let mut m = copied_a_poll_later(button::CROSS);
        let (f, reads) = run(&mut m, 3);
        assert_eq!(f.buffer().iter().map(|b| b.addr).collect::<Vec<_>>(), [0x8002_0000, 0x8002_0001]);
        assert_eq!(f.handler_copies(), [BufferByte { addr: 0x8002_0100, pos: 3, mask: Some(0) }]);
        assert!(!reads.is_empty() && reads.iter().all(|r| r.in_handler && r.pc == 0x8000_0084), "{reads:x?}");
    }

    #[test]
    fn a_halfword_assembled_and_inverted_keeps_its_bytes() {
        let mut m = machine(Keep::Inverted, button::CROSS);
        let (f, _) = run(&mut m, 3);
        let inverted = Some(0xFF);
        assert_eq!(
            f.buffer(),
            [
                BufferByte { addr: 0x8002_0000, pos: 3, mask: inverted },
                BufferByte { addr: 0x8002_0001, pos: 4, mask: inverted }
            ]
        );
    }

    #[test]
    fn a_reply_kept_in_registers_has_no_buffer_and_its_serial_loads_are_reads() {
        let mut m = machine(Keep::Registers, button::CROSS);
        let (f, reads) = run(&mut m, 2);
        assert!(f.buffer().is_empty() && f.copies().is_empty());
        let positions: Vec<u8> = reads.iter().filter(|r| r.addr == JOY_DATA).map(|r| r.marks[0].pos).collect();
        // The first byte comes before the exchange is known to be a poll.
        assert!(positions.windows(4).any(|w| w == [1, 2, 3, 4]), "{positions:?}");
    }

    #[test]
    fn shifts_move_marks_between_lanes() {
        let m = Mark { exchange: 1, pos: 3, value: 0xFF };
        assert_eq!(shift([Some(m), None, None, None], 8, true), [None, Some(m), None, None]);
        assert_eq!(shift([None, Some(m), None, None], 8, false), [Some(m), None, None, None]);
        assert_eq!(shift([Some(m), None, None, None], 4, true), [Some(m), Some(m), None, None]);
        assert_eq!(shift([None, None, None, Some(m)], 8, true), [None; 4]);
    }
}

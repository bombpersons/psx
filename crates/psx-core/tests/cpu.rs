use psx_core::cpu::cop0;
use psx_core::{Machine, Width};

/// A machine with `code` at 0x80001000 and the CPU about to execute it.
fn machine(code: &[u32]) -> Machine {
    let mut m = Machine::new(vec![0; 16]);
    for (i, w) in code.iter().enumerate() {
        m.bus
            .write(0x8000_1000 + 4 * i as u32, Width::Word, *w)
            .unwrap();
    }
    m.cpu.set_pc(0x8000_1000);
    m.cpu.cop0[cop0::SR] = 0; // BEV off: exceptions go to 0x80000080
    m
}

fn run(m: &mut Machine, n: usize) {
    for _ in 0..n {
        m.step();
    }
}

// Encoders for the handful of instructions the tests need.
fn i_type(op: u32, rs: u32, rt: u32, imm: u16) -> u32 {
    (op << 26) | (rs << 21) | (rt << 16) | imm as u32
}
fn r_type(rs: u32, rt: u32, rd: u32, funct: u32) -> u32 {
    (rs << 21) | (rt << 16) | (rd << 11) | funct
}
const T0: u32 = 8;
const T1: u32 = 9;
const T2: u32 = 10;
const NOP: u32 = 0;

#[test]
fn load_delay_slot_sees_old_value_then_new() {
    let mut m = machine(&[
        i_type(0x23, 0, T0, 0x2000), // lw   t0, 0x2000(zero)
        r_type(T0, 0, T1, 0x21),     // addu t1, t0, zero   (delay slot: old t0)
        r_type(T0, 0, T2, 0x21),     // addu t2, t0, zero   (new t0)
    ]);
    m.bus.write(0x2000, Width::Word, 0x1234_5678).unwrap();
    m.cpu.regs[T0 as usize] = 7;
    run(&mut m, 3);
    assert_eq!(m.cpu.regs[T1 as usize], 7);
    assert_eq!(m.cpu.regs[T2 as usize], 0x1234_5678);
}

#[test]
fn write_in_delay_slot_beats_pending_load() {
    let mut m = machine(&[
        i_type(0x23, 0, T0, 0x2000), // lw    t0, 0x2000(zero)
        i_type(0x09, 0, T0, 5),      // addiu t0, zero, 5  (later write wins)
        NOP,
    ]);
    m.bus.write(0x2000, Width::Word, 99).unwrap();
    run(&mut m, 3);
    assert_eq!(m.cpu.regs[T0 as usize], 5);
}

#[test]
fn branch_delay_slot_executes_and_link_skips_it() {
    let mut m = machine(&[
        0x0C00_0404,            // jal 0x80001010
        i_type(0x09, 0, T0, 1), // addiu t0, zero, 1 (delay slot runs)
        i_type(0x09, 0, T1, 2), // skipped
        NOP,
        i_type(0x09, 0, T2, 3), // 0x80001010
    ]);
    run(&mut m, 3);
    assert_eq!(m.cpu.regs[T0 as usize], 1);
    assert_eq!(m.cpu.regs[T1 as usize], 0);
    assert_eq!(m.cpu.regs[T2 as usize], 3);
    assert_eq!(m.cpu.regs[31], 0x8000_1008);
}

#[test]
fn lwl_lwr_assemble_unaligned_word() {
    let mut m = machine(&[
        i_type(0x26, 0, T0, 0x2001), // lwr t0, 0x2001(zero)
        i_type(0x22, 0, T0, 0x2004), // lwl t0, 0x2004(zero)  merges with in-flight lwr
        NOP,
    ]);
    m.bus.write(0x2000, Width::Word, 0x4433_2211).unwrap();
    m.bus.write(0x2004, Width::Word, 0x8877_6655).unwrap();
    run(&mut m, 3);
    assert_eq!(m.cpu.regs[T0 as usize], 0x5544_3322);
}

#[test]
fn division_edge_cases() {
    let mut m = machine(&[
        r_type(T0, T1, 0, 0x1A), // div t0, t1
        r_type(0, 0, T2, 0x12),  // mflo t2
    ]);
    m.cpu.regs[T0 as usize] = 0x8000_0000;
    m.cpu.regs[T1 as usize] = 0xFFFF_FFFF;
    run(&mut m, 2);
    assert_eq!(m.cpu.regs[T2 as usize], 0x8000_0000);
    assert_eq!(m.cpu.hi, 0);

    let mut m = machine(&[r_type(T0, T1, 0, 0x1A)]); // div by zero, negative numerator
    m.cpu.regs[T0 as usize] = (-5i32) as u32;
    run(&mut m, 1);
    assert_eq!((m.cpu.lo, m.cpu.hi), (1, (-5i32) as u32));
}

#[test]
fn overflow_in_delay_slot_sets_bd_and_epc_of_branch() {
    let mut m = machine(&[
        i_type(0x04, 0, 0, 4),    // b +4
        r_type(T0, T1, T2, 0x20), // add t2, t0, t1  -> overflow in the delay slot
    ]);
    m.cpu.regs[T0 as usize] = 0x7FFF_FFFF;
    m.cpu.regs[T1 as usize] = 1;
    m.cpu.cop0[cop0::SR] = 0b01; // IEc set, so the KU/IE stack shift is visible
    run(&mut m, 2);
    assert_eq!(m.cpu.pc, 0x8000_0080);
    assert_eq!(m.cpu.cop0[cop0::EPC], 0x8000_1000);
    assert_eq!(m.cpu.cop0[cop0::CAUSE] >> 31, 1, "BD");
    assert_eq!((m.cpu.cop0[cop0::CAUSE] >> 2) & 0x1F, 0xC);
    assert_eq!(m.cpu.cop0[cop0::SR] & 0x3F, 0b0100, "IEc pushed to IEp");
    assert_eq!(
        m.cpu.regs[T2 as usize], 0,
        "faulting instruction has no effect"
    );
}

#[test]
fn isolated_cache_drops_stores() {
    let mut m = machine(&[i_type(0x2B, 0, T0, 0x2000)]); // sw t0, 0x2000(zero)
    m.cpu.cop0[cop0::SR] = 1 << 16;
    m.cpu.regs[T0 as usize] = 0xAAAA_AAAA;
    run(&mut m, 1);
    assert_eq!(m.bus.read(0x2000, Width::Word).unwrap(), 0);
}

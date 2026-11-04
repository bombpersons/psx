//! One call of a function, run twice from the same moment, and the effects
//! compared: the original's code, and code the game cannot see loaded at
//! [`crate::bus::HIDDEN`] (an agent's C, docs/decomp-plan.md, step 10).
//!
//! A call runs from its entry until the CPU is back at the return address
//! with `sp` as it was, or a step limit. Its effects are what a caller could
//! see: the RAM and scratchpad bytes it left changed (its own stack frame,
//! below `sp`, is scratch and left out), the return registers it uses (`v0`,
//! and `v1` for a 64-bit value: otherwise `v1` is scratch), `sp` and the
//! registers a call keeps (`s0`..`s7`, `gp`, `fp`), the GTE's registers,
//! the hardware register writes in order, and the functions it called with
//! their first four arguments, in order. What an exception handler does
//! meanwhile is left out, and time is not compared.

use std::collections::BTreeMap;

use crate::Machine;
use crate::cpu::StepInfo;

/// What a call did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// It came back to the return address with `sp` restored.
    pub returned: bool,
    pub steps: u64,
    /// `v0`, `v1`.
    pub ret: [u32; 2],
    pub sp: u32,
    /// `s0`..`s7`, `gp`, `fp`.
    pub kept: [u32; 10],
    /// The GTE's data registers, then its control registers.
    pub gte: Vec<u32>,
    /// Bytes stored (physical address: main RAM folded, or the scratchpad),
    /// each as last stored, outside an exception handler.
    pub stores: BTreeMap<u32, u8>,
    /// The lowest `sp` the call reached: below its entry `sp`, its frame.
    pub low_sp: u32,
    /// Hardware register writes: address, width, value.
    pub io: Vec<(u32, u8, u32)>,
    /// Calls it made: the callee's entry and `a0`..`a3`.
    pub calls: Vec<(u32, [u32; 4])>,
}

/// The names of the registers a call keeps, as [`Effects::kept`] has them.
const KEPT: [&str; 10] = ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "gp", "fp"];

fn physical(addr: u32) -> u32 {
    let p = addr & 0x1FFF_FFFF;
    if p < 0x0080_0000 { p & 0x1F_FFFF } else { p }
}

/// Runs the call `m` is at the entry of (its return address in `ra`, its
/// arguments set) for at most `max_steps` steps, and gives its effects.
pub fn run_call(m: &mut Machine, max_steps: u64) -> Effects {
    let (ra, sp) = (m.cpu.regs[31], m.cpu.regs[29]);
    let mut e = Effects { low_sp: sp, ..Effects::default() };
    let mut in_handler = false;
    // A jal or jalr ran: after its delay slot, the CPU is at the callee.
    let mut calling = 0u8;
    while e.steps < max_steps {
        let info: StepInfo = m.step();
        e.steps += 1;
        if info.exception.is_some() {
            in_handler = true;
            continue;
        }
        if info.instruction == 0x4200_0010 {
            in_handler = false;
            continue;
        }
        if in_handler {
            continue;
        }
        if let Some(acc) = info.mem.filter(|a| a.store) {
            let p = physical(acc.addr);
            if crate::bus::Bus::is_io(acc.addr) {
                e.io.push((p, acc.width, acc.value));
            } else if p < 0x0080_0000 || (0x1F80_0000..0x1F80_0400).contains(&p) {
                for k in 0..acc.width as u32 {
                    e.stores.insert(p + k, (acc.value >> (8 * k)) as u8);
                }
            }
        }
        e.low_sp = e.low_sp.min(m.cpu.regs[29]);
        let i = info.instruction;
        if calling == 1 {
            e.calls.push((m.cpu.pc, [m.cpu.regs[4], m.cpu.regs[5], m.cpu.regs[6], m.cpu.regs[7]]));
        }
        calling = calling.saturating_sub(1);
        if i >> 26 == 3 || (i >> 26 == 0 && i & 0x3F == 9) {
            calling = 1;
        }
        if m.cpu.pc == ra && m.cpu.regs[29] == sp {
            e.returned = true;
            break;
        }
    }
    let r = &m.cpu.regs;
    e.ret = [r[2], r[3]];
    e.sp = r[29];
    e.kept = [r[16], r[17], r[18], r[19], r[20], r[21], r[22], r[23], r[28], r[30]];
    e.gte = (0..32).map(|k| m.cpu.gte.read_data(k)).chain((0..32).map(|k| m.cpu.gte.read_ctrl(k))).collect();
    e
}

/// The effects of the original and of the replacement, and how they differ.
#[derive(Clone, Debug)]
pub struct Compared {
    pub original: Effects,
    pub replacement: Effects,
    /// Each difference, in words; empty when the replacement does as the
    /// original did.
    pub differences: Vec<String>,
}

/// The call `m` is at the entry of, run as the original does it and with
/// the CPU sent to `replacement` instead (code in `build`, loaded at
/// [`crate::bus::HIDDEN`]), each from `m`'s state, for at most `max_steps`
/// steps; the first `returns` of `v0` and `v1` are compared (0 for a
/// function returning nothing, 2 for a 64-bit value). `m` is left as it
/// was; its disc (a save state holds none) is lent to each run in turn, so
/// a call that reads the CD sees the drive the game sees.
pub fn compare(
    m: &mut Machine,
    build: &[u8],
    replacement: u32,
    returns: usize,
    max_steps: u64,
) -> Result<Compared, String> {
    let state = m.save_state(None);
    let mut disc = m.bus.cdrom.disc.take();
    let mut run = |pc: Option<u32>| -> Result<Effects, String> {
        let (_, mut c) = Machine::load_state(&state, m.bus.bios.clone()).map_err(|e| format!("{e:?}"))?;
        c.bus.hidden = Some(build.to_vec());
        c.bus.cdrom.disc = disc.take();
        if let Some(pc) = pc {
            c.cpu.set_pc(pc);
        }
        let e = run_call(&mut c, max_steps);
        disc = c.bus.cdrom.disc.take();
        Ok(e)
    };
    let both = run(None).and_then(|a| Ok((a, run(Some(replacement))?)));
    m.bus.cdrom.disc = disc;
    let (original, replacement) = both?;
    let differences = differences(&original, &replacement, returns, max_steps);
    Ok(Compared { original, replacement, differences })
}

fn hex(v: u32) -> String {
    format!("0x{v:08X}")
}

/// How `b` (the replacement) differs from `a` (the original), comparing
/// the first `returns` return registers.
pub fn differences(a: &Effects, b: &Effects, returns: usize, max_steps: u64) -> Vec<String> {
    let mut out = Vec::new();
    for (e, who) in [(a, "the original"), (b, "the replacement")] {
        if !e.returned {
            out.push(format!("{who} did not return within {max_steps} steps"));
        }
    }
    if !out.is_empty() {
        return out;
    }
    for (k, name) in ["v0", "v1"].iter().enumerate().take(returns) {
        if a.ret[k] != b.ret[k] {
            out.push(format!("{name}: original {}, replacement {}", hex(a.ret[k]), hex(b.ret[k])));
        }
    }
    if a.sp != b.sp {
        out.push(format!("sp: original {}, replacement {}", hex(a.sp), hex(b.sp)));
    }
    for (k, name) in KEPT.iter().enumerate() {
        if a.kept[k] != b.kept[k] {
            out.push(format!("{name}: original {}, replacement {} (a call keeps it)", hex(a.kept[k]), hex(b.kept[k])));
        }
    }
    // Bytes below the entry sp, down to the lower of the two frames, are the
    // calls' own scratch.
    let frame = physical(a.low_sp.min(b.low_sp))..physical(a.sp);
    let left = |e: &Effects| -> BTreeMap<u32, u8> {
        e.stores.iter().filter(|(p, _)| !frame.contains(p)).map(|(p, v)| (*p, *v)).collect()
    };
    let (sa, sb) = (left(a), left(b));
    let addrs: std::collections::BTreeSet<u32> = sa.keys().chain(sb.keys()).copied().collect();
    let mut bytes = Vec::new();
    for p in addrs {
        let (x, y) = (sa.get(&p), sb.get(&p));
        if x != y {
            let show = |v: Option<&u8>| v.map_or("not stored".to_string(), |v| format!("0x{v:02X}"));
            bytes.push(format!("byte {}: original {}, replacement {}", hex(p | 0x8000_0000), show(x), show(y)));
        }
    }
    if bytes.len() > 8 {
        let more = bytes.len() - 8;
        bytes.truncate(8);
        bytes.push(format!("and {more} more bytes"));
    }
    out.extend(bytes);
    if let Some(k) = (0..a.gte.len()).find(|&k| a.gte[k] != b.gte[k]) {
        let (kind, n) = if k < 32 { ("data", k) } else { ("control", k - 32) };
        out.push(format!("GTE {kind} register {n}: original {}, replacement {}", hex(a.gte[k]), hex(b.gte[k])));
    }
    if a.io != b.io {
        let k = a.io.iter().zip(&b.io).take_while(|(x, y)| x == y).count();
        let show = |w: Option<&(u32, u8, u32)>| w.map_or("none".into(), |w| format!("{} = {}", hex(w.0), hex(w.2)));
        out.push(format!(
            "hardware write {}: original {}, replacement {}",
            k + 1,
            show(a.io.get(k)),
            show(b.io.get(k))
        ));
    }
    if a.calls != b.calls {
        let k = a.calls.iter().zip(&b.calls).take_while(|(x, y)| x == y).count();
        let show = |c: Option<&(u32, [u32; 4])>| {
            c.map_or("none".into(), |c| {
                format!("{} ({})", hex(c.0), c.1.iter().map(|a| hex(*a)).collect::<Vec<_>>().join(", "))
            })
        };
        out.push(format!("call {}: original {}, replacement {}", k + 1, show(a.calls.get(k)), show(b.calls.get(k))));
    }
    out
}

/// The function the tests check, for psx-core's tests and psx's.
#[doc(hidden)]
pub mod tests_support {
    use crate::Machine;
    use crate::padflow::test_program::asm::*;
    use crate::padflow::test_program::load;

    const V0: u32 = 2;
    const A0: u32 = 4;
    const T0: u32 = 8;
    const S0: u32 = 16;
    const SP: u32 = 29;
    const RA: u32 = 31;

    pub fn sw(rt: u32, off: u32, base: u32) -> u32 {
        i(0x2B, base, rt, off)
    }
    pub fn lw(rt: u32, off: u32, base: u32) -> u32 {
        i(0x23, base, rt, off)
    }
    pub fn jal(target: u32) -> u32 {
        0x03 << 26 | (target >> 2 & 0x03FF_FFFF)
    }

    /// The original, at 0x80010100: f(a0) stores a0 + 1 at 0x80020000 and
    /// returns a0 * 2, keeping s0 on the stack, and calls 0x80010200
    /// (which returns at once) with a0 = 5.
    pub fn original() -> Vec<u32> {
        vec![
            addiu(SP, SP, 0xFFF0), // 80010100
            sw(RA, 12, SP),
            sw(S0, 8, SP),
            or(S0, A0, 0),
            addiu(T0, S0, 1),
            lui(1, 0x8002),
            sw(T0, 0, 1),
            jal(0x8001_0200),
            addiu(A0, 0, 5),
            sll(V0, S0, 1),
            lw(S0, 8, SP),
            lw(RA, 12, SP),
            NOP, // the load's delay
            jr(RA),
            addiu(SP, SP, 16),
        ]
    }

    /// A machine at the call of the original with a0 = 20 (s0 = 0x5050),
    /// returning to 0x80010000 (a loop).
    pub fn at_call() -> Machine {
        let mut m = Machine::new(vec![0; 0x8_0000]);
        load(&mut m, 0x8001_0000, &[j(0x8001_0000), NOP]);
        load(&mut m, 0x8001_0100, &original());
        load(&mut m, 0x8001_0200, &[jr(RA), NOP]);
        m.cpu.regs[A0 as usize] = 20;
        m.cpu.regs[S0 as usize] = 0x5050;
        m.cpu.regs[SP as usize] = 0x801F_FF00;
        m.cpu.regs[RA as usize] = 0x8001_0000;
        m.cpu.set_pc(0x8001_0100);
        m
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use crate::padflow::test_program::asm::*;
    use crate::padflow::test_program::load;

    const V0: u32 = 2;
    const A0: u32 = 4;
    const T0: u32 = 8;
    const S0: u32 = 16;
    const SP: u32 = 29;
    const RA: u32 = 31;

    /// A replacement assembled to run at HIDDEN, as bytes.
    fn build(code: &[u32]) -> Vec<u8> {
        code.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn check(code: &[u32]) -> Compared {
        compare(&mut at_call(), &build(code), crate::bus::HIDDEN, 1, 10_000).unwrap()
    }

    #[test]
    fn the_disc_is_lent_to_both_runs_and_given_back() {
        let dir = std::env::temp_dir().join(format!("psx-callcheck-disc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let iso = dir.join("blank.iso");
        std::fs::write(&iso, vec![0u8; 2048 * 32]).unwrap();
        let mut m = at_call();
        m.bus.cdrom.disc = Some(psx_media::Disc::open(&iso).unwrap());
        compare(&mut m, &build(&[0]), crate::bus::HIDDEN, 1, 10).unwrap();
        assert!(m.bus.cdrom.disc.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_equivalent_function_passes() {
        // The same, written another way: no frame of its own for s0.
        let c = check(&[
            addiu(SP, SP, 0xFFE8),
            sw(RA, 20, SP),
            sw(A0, 16, SP),
            addiu(T0, A0, 1),
            lui(1, 0x8002),
            sw(T0, 0, 1),
            jal(0x8001_0200),
            addiu(A0, 0, 5),
            lw(T0, 16, SP),
            NOP,
            addiu(V0, T0, 0),
            sll(V0, V0, 1),
            lw(RA, 20, SP),
            NOP,
            jr(RA),
            addiu(SP, SP, 24),
        ]);
        assert!(c.original.returned && c.replacement.returned);
        assert_eq!(c.original.ret[0], 40);
        assert_eq!(c.original.calls, [(0x8001_0200, [5, 0, 0, 0])]);
        assert_eq!(c.differences, Vec::<String>::new());
    }

    fn the_original_but(change: impl FnOnce(&mut Vec<u32>)) -> Compared {
        let mut code = original();
        change(&mut code);
        check(&code)
    }

    #[test]
    fn a_wrong_byte_is_named() {
        let c = the_original_but(|c| c[4] = addiu(T0, S0, 2));
        assert_eq!(c.differences, ["byte 0x80020000: original 0x15, replacement 0x16"]);
    }

    #[test]
    fn a_wrong_return_value_is_named() {
        let c = the_original_but(|c| c[9] = sll(V0, S0, 2));
        assert_eq!(c.differences, ["v0: original 0x00000028, replacement 0x00000050"]);
    }

    #[test]
    fn a_missing_call_is_named() {
        let c = the_original_but(|c| c[7] = NOP);
        assert_eq!(
            c.differences,
            ["call 1: original 0x80010200 (0x00000005, 0x00000000, 0x00000000, 0x00000000), replacement none"]
        );
    }

    #[test]
    fn a_clobbered_saved_register_is_named() {
        // s0 not restored from the stack.
        let c = the_original_but(|c| c[10] = NOP);
        assert_eq!(c.differences, ["s0: original 0x00005050, replacement 0x00000014 (a call keeps it)"]);
    }

    #[test]
    fn a_loop_that_never_returns_is_named() {
        let mut code = original();
        code.extend([NOP; 4]);
        let at = code.len() as u32;
        code.extend([j(crate::bus::HIDDEN + 4 * at), NOP]);
        code[13] = j(crate::bus::HIDDEN + 4 * at);
        let c = check(&code);
        assert_eq!(c.differences, ["the replacement did not return within 10000 steps"]);
    }

    /// A loop calling the test function with a0 = 20, keeping v0 at
    /// 0x80030000, at 0x80010400.
    fn caller() -> Machine {
        let mut m = at_call();
        load(
            &mut m,
            0x8001_0400,
            &[addiu(A0, 0, 20), jal(0x8001_0100), NOP, lui(1, 0x8003), sw(V0, 0, 1), j(0x8001_0400), NOP],
        );
        m.cpu.set_pc(0x8001_0400);
        m
    }

    #[test]
    fn a_redirected_call_returns_at_once_or_runs_its_replacement() {
        // Returned at once, through the run loop's fast path too.
        let mut m = caller();
        m.redirects.insert(0x8001_0100, crate::Redirect::Return(99));
        m.run_frame_steps(5_000);
        assert_eq!(m.bus.ram[0x3_0000], 99);
        assert!(m.redirects.hits(0x8001_0100) > 1);
        assert_eq!(m.bus.ram[0x2_0000], 0, "the original never ran");
        // Sent to a replacement at HIDDEN that returns 7.
        let mut m = caller();
        m.bus.hidden = Some(build(&[jr(RA), addiu(V0, 0, 7)]));
        m.redirects.insert(0x8001_0100, crate::Redirect::To(crate::bus::HIDDEN));
        for _ in 0..200 {
            m.step();
        }
        assert_eq!(m.bus.ram[0x3_0000], 7);
        // Without one, the original runs: 20 * 2.
        let mut m = caller();
        m.run_frame_steps(5_000);
        assert_eq!(m.bus.ram[0x3_0000], 40);
    }

    #[test]
    fn the_hidden_range_is_unmapped_until_used() {
        let mut m = Machine::new(vec![0; 0x8_0000]);
        assert!(m.bus.read(crate::bus::HIDDEN, crate::bus::Width::Word).is_err());
        m.bus.hidden = Some(vec![0x78, 0x56, 0x34, 0x12]);
        assert_eq!(m.bus.read(crate::bus::HIDDEN, crate::bus::Width::Word), Ok(0x1234_5678));
        assert!(m.bus.read(crate::bus::HIDDEN + 4, crate::bus::Width::Word).is_err());
    }
}

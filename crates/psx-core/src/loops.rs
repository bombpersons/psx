//! Exact fast-forward of wait loops that only touch RAM, such as PsyQ's
//! `VSync`, which counts a timeout down while it waits for the vblank
//! interrupt to move a counter on:
//!
//! ```text
//! loop: lw v0, 16(sp); addiu v0, v0, -1; sw v0, 16(sp); ...; bne v0, v1, ...
//!       lw v0, vsync_count; slt v0, v0, a0; bne v0, zero, loop
//! ```
//!
//! No two passes leave the same state, so `Machine::skip_spin` can't skip
//! them. Instead, once two passes have run for real and taken the same
//! path and time, [`evaluate`] runs the pass's instructions on values
//! `base + j * step` (`j` counting passes from now): registers and the RAM
//! words the pass stores each move by a constant step, and RAM it only
//! reads stays still (between events only the CPU writes RAM). It finds
//! how many passes keep every branch and comparison as they are now, and
//! what the registers and stored words are after any number of them.

use crate::bus::Width;

/// `b + j * d` in pass `j` (wrapping, as the CPU's arithmetic).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Aff {
    pub b: u32,
    pub d: u32,
}

impl Aff {
    fn c(v: u32) -> Self {
        Aff { b: v, d: 0 }
    }

    /// The value in pass `j`.
    pub fn at(self, j: u64) -> u32 {
        self.b.wrapping_add(self.d.wrapping_mul(j as u32))
    }

    fn add(self, o: Aff) -> Aff {
        Aff { b: self.b.wrapping_add(o.b), d: self.d.wrapping_add(o.d) }
    }

    fn konst(self) -> Option<u32> {
        (self.d == 0).then_some(self.b)
    }
}

/// A RAM or scratchpad location the pass reads or writes.
struct Cell {
    addr: u32,
    w: Width,
    val: Aff,
    /// The value read before the pass stored here, if it did.
    first: Option<Aff>,
    stored: bool,
}

/// What [`evaluate`] found: the state after the pass, as functions of the
/// pass count, and how many passes (from now) take the same path.
pub(crate) struct Outcome {
    pub regs: [Aff; 32],
    pub load: (usize, Aff),
    /// The stored locations and the value each pass stores.
    pub stores: Vec<(u32, Width, Aff)>,
    /// The locations only read, and their value.
    pub reads: Vec<(u32, Width, u32)>,
    pub passes: u64,
}

/// A value's signed trajectory `(base, step)`, limiting `passes` to those
/// in which it doesn't wrap.
fn signed(a: Aff, passes: &mut u64) -> (i64, i64) {
    let (b, d) = (a.b as i32 as i64, a.d as i32 as i64);
    if d > 0 {
        *passes = (*passes).min(((i32::MAX as i64 - b) / d + 1) as u64);
    } else if d < 0 {
        *passes = (*passes).min(((b - i32::MIN as i64) / -d + 1) as u64);
    }
    (b, d)
}

/// The unsigned trajectory: the signed one, limited to passes in which the
/// sign stays (so the unsigned value doesn't wrap either).
fn unsigned(a: Aff, passes: &mut u64) -> (i64, i64) {
    let (b, d) = signed(a, passes);
    if b < 0 && d > 0 {
        *passes = (*passes).min(((-b + d - 1) / d) as u64);
    } else if b >= 0 && d < 0 {
        *passes = (*passes).min((b / -d + 1) as u64);
    }
    (if b < 0 { b + (1 << 32) } else { b }, d)
}

/// `x < y` now, limiting `passes` to those in which it stays so.
fn less(x: (i64, i64), y: (i64, i64), passes: &mut u64) -> bool {
    let (z, dz) = (x.0 - y.0, x.1 - y.1);
    if z < 0 && dz > 0 {
        *passes = (*passes).min(((-z + dz - 1) / dz) as u64);
    } else if z >= 0 && dz < 0 {
        *passes = (*passes).min((z / -dz + 1) as u64);
    }
    z < 0
}

/// `x == y` now, limiting `passes` to those in which it stays so.
fn equal(x: Aff, y: Aff, passes: &mut u64) -> bool {
    let (x, y) = (signed(x, passes), signed(y, passes));
    let (z, dz) = (x.0 - y.0, x.1 - y.1);
    if dz != 0 {
        if z == 0 {
            *passes = (*passes).min(1);
        } else if z % dz == 0 && -z / dz > 0 {
            *passes = (*passes).min((-z / dz) as u64);
        }
    }
    z == 0
}

fn sext16(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}

/// Run one pass, `trace`'s `(pc, instruction)`s, from registers `regs`,
/// the load in flight `load` and `pc`/`next_pc`, as the CPU executes it
/// (`Cpu::execute`: operands read, the load in flight lands, the result is
/// written). `read` reads RAM or the scratchpad (None elsewhere); `steps`
/// gives the step of each location the pass stores, which a read before
/// the store sees. None when the pass does anything else.
fn run(
    trace: &[(u32, u32)],
    mut r: [Aff; 32],
    mut load: (usize, Aff),
    (mut pc, mut next_pc): (u32, u32),
    read: &impl Fn(u32, Width) -> Option<u32>,
    steps: &[(u32, Width, u32)],
) -> Option<(Outcome, Vec<Cell>, (u32, u32))> {
    let mut passes = u64::MAX;
    let mut mem: Vec<Cell> = Vec::new();
    let cell = |mem: &mut Vec<Cell>, addr: u32, w: Width| -> Option<usize> {
        let len = w as u32;
        for (k, c) in mem.iter().enumerate() {
            if c.addr == addr && c.w == w {
                return Some(k);
            }
            // Only the same access may overlap another.
            if addr < c.addr + c.w as u32 && c.addr < addr + len {
                return None;
            }
        }
        let v = read(addr, w)?;
        let d = steps.iter().find(|s| s.0 == addr && s.1 == w).map_or(0, |s| s.2);
        mem.push(Cell { addr, w, val: Aff { b: v, d }, first: None, stored: false });
        Some(mem.len() - 1)
    };
    for &(at, i) in trace {
        if at != pc {
            return None;
        }
        pc = next_pc;
        next_pc = next_pc.wrapping_add(4);
        let op = i >> 26;
        let (rs, rt, rd) = (((i >> 21) & 31) as usize, ((i >> 16) & 31) as usize, ((i >> 11) & 31) as usize);
        let (s, t, imm) = (r[rs], r[rt], i & 0xFFFF);
        let branch = (pc & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2);
        let rel = pc.wrapping_add(sext16(imm) << 2);
        // The result (register 0 for none), written after the load lands.
        let mut out = (0usize, Aff::c(0));
        let mut target = None;
        let mut new_load = None;
        match op {
            0x00 => {
                let sh = (i >> 6) & 31;
                out = match i & 0x3F {
                    0x00 => (rd, Aff { b: t.b << sh, d: t.d << sh }),
                    0x02 => (rd, Aff::c(t.konst()? >> sh)),
                    0x03 => (rd, Aff::c(((t.konst()? as i32) >> sh) as u32)),
                    0x04 => (rd, Aff::c(t.konst()? << (s.konst()? & 31))),
                    0x06 => (rd, Aff::c(t.konst()? >> (s.konst()? & 31))),
                    0x07 => (rd, Aff::c(((t.konst()? as i32) >> (s.konst()? & 31)) as u32)),
                    0x08 => {
                        target = Some(s.konst()?);
                        (0, Aff::c(0))
                    }
                    0x09 => {
                        target = Some(s.konst()?);
                        (rd, Aff::c(next_pc))
                    }
                    0x20 => (rd, Aff::c((s.konst()? as i32).checked_add(t.konst()? as i32)? as u32)),
                    0x22 => (rd, Aff::c((s.konst()? as i32).checked_sub(t.konst()? as i32)? as u32)),
                    0x21 => (rd, s.add(t)),
                    0x23 => (rd, Aff { b: s.b.wrapping_sub(t.b), d: s.d.wrapping_sub(t.d) }),
                    // A move (OR or XOR with register 0) keeps its trajectory.
                    0x25 | 0x26 if t == Aff::c(0) => (rd, s),
                    0x25 | 0x26 if s == Aff::c(0) => (rd, t),
                    0x24 => (rd, Aff::c(s.konst()? & t.konst()?)),
                    0x25 => (rd, Aff::c(s.konst()? | t.konst()?)),
                    0x26 => (rd, Aff::c(s.konst()? ^ t.konst()?)),
                    0x27 => (rd, Aff::c(!(s.konst()? | t.konst()?))),
                    0x2A => (rd, Aff::c(less(signed(s, &mut passes), signed(t, &mut passes), &mut passes) as u32)),
                    0x2B => (rd, Aff::c(less(unsigned(s, &mut passes), unsigned(t, &mut passes), &mut passes) as u32)),
                    _ => return None,
                };
            }
            0x01 => {
                if (i >> 17) & 0xF == 0x8 {
                    return None;
                }
                let ge = (i >> 16) & 1 != 0;
                let neg = less(signed(s, &mut passes), (0, 0), &mut passes);
                if neg ^ ge {
                    target = Some(rel);
                }
            }
            0x02 => target = Some(branch),
            0x03 => {
                target = Some(branch);
                out = (31, Aff::c(next_pc));
            }
            0x04 | 0x05 => {
                if equal(s, t, &mut passes) == (op == 0x04) {
                    target = Some(rel);
                }
            }
            0x06 | 0x07 => {
                let pos = less((0, 0), signed(s, &mut passes), &mut passes);
                if pos == (op == 0x07) {
                    target = Some(rel);
                }
            }
            0x08 => out = (rt, Aff::c((s.konst()? as i32).checked_add(sext16(imm) as i32)? as u32)),
            0x09 => out = (rt, s.add(Aff::c(sext16(imm)))),
            0x0A => {
                out = (rt, Aff::c(less(signed(s, &mut passes), (sext16(imm) as i32 as i64, 0), &mut passes) as u32))
            }
            0x0B => {
                let k = sext16(imm) as i64;
                out = (rt, Aff::c(less(unsigned(s, &mut passes), (k, 0), &mut passes) as u32))
            }
            0x0D | 0x0E if imm == 0 => out = (rt, s),
            0x0C => out = (rt, Aff::c(s.konst()? & imm)),
            0x0D => out = (rt, Aff::c(s.konst()? | imm)),
            0x0E => out = (rt, Aff::c(s.konst()? ^ imm)),
            0x0F => out = (rt, Aff::c(imm << 16)),
            0x20 | 0x21 | 0x23 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2B => {
                let addr = s.konst()?.wrapping_add(sext16(imm));
                let w = match op & 3 {
                    0 => Width::Byte,
                    1 => Width::Half,
                    _ => Width::Word,
                };
                if addr & (w as u32 - 1) != 0 {
                    return None;
                }
                let k = cell(&mut mem, addr, w)?;
                if op >= 0x28 {
                    // A narrow store of a moving value would wrap.
                    if w != Width::Word && t.d != 0 {
                        return None;
                    }
                    mem[k].val = t;
                    mem[k].stored = true;
                } else {
                    let c = &mut mem[k];
                    if !c.stored && c.first.is_none() {
                        c.first = Some(c.val);
                    }
                    let v = c.val;
                    let v = match op {
                        0x20 => Aff::c(v.konst()? as u8 as i8 as u32),
                        0x21 => Aff::c(v.konst()? as u16 as i16 as u32),
                        _ => v,
                    };
                    new_load = Some((rt, v));
                }
            }
            _ => return None,
        }
        match new_load {
            Some((rt, v)) => {
                // `Cpu::land_unless_reloaded`: a load into the register in
                // flight cancels it.
                if load.0 != rt {
                    r[load.0 & 31] = load.1;
                }
                load = (rt, v);
            }
            None => {
                r[load.0 & 31] = load.1;
                load.0 = 0;
                r[out.0] = out.1;
            }
        }
        r[0] = Aff::c(0);
        if let Some(t) = target {
            next_pc = t;
        }
    }
    let stores = mem.iter().filter(|c| c.stored).map(|c| (c.addr, c.w, c.val)).collect();
    let reads = mem.iter().filter(|c| !c.stored).map(|c| (c.addr, c.w, c.val.b)).collect();
    Some((Outcome { regs: r, load, stores, reads, passes }, mem, (pc, next_pc)))
}

/// Evaluate a pass `trace` that starts and ends at `pc`/`next_pc`, from
/// registers and the load in flight moving as `regs` and `load` do: None
/// unless the pass ends with them moved by one step (and the stored words
/// likewise), so that every later pass repeats it on stepped values.
pub(crate) fn evaluate(
    trace: &[(u32, u32)],
    regs: [Aff; 32],
    load: (usize, Aff),
    pcs: (u32, u32),
    read: impl Fn(u32, Width) -> Option<u32>,
) -> Option<Outcome> {
    let mut steps: Vec<(u32, Width, u32)> = Vec::new();
    // The steps of the stored words: assumed still, then the pass's change
    // to them (or, for a word not read first, the step of what it stores).
    for _ in 0..3 {
        let (out, mem, end) = run(trace, regs, load, pcs, &read, &steps)?;
        let found: Vec<_> = mem
            .iter()
            .filter(|c| c.stored)
            .map(|c| (c.addr, c.w, c.first.map_or(c.val.d, |f| c.val.b.wrapping_sub(f.b))))
            .collect();
        if found != steps {
            steps = found;
            continue;
        }
        // A word read before it is stored moves by its step from pass to pass.
        let settled = mem.iter().all(|c| match (c.stored, c.first) {
            (true, Some(f)) => c.val.d == f.d && c.val.b == f.b.wrapping_add(f.d),
            _ => true,
        });
        let next = |a: Aff, b: Aff| b.b == a.b.wrapping_add(a.d) && b.d == a.d;
        let regs_ok = (1..32).all(|k| next(regs[k], out.regs[k]));
        let load_ok = out.load.0 == load.0 && next(load.1, out.load.1);
        return (end == pcs && settled && regs_ok && load_ok).then_some(out);
    }
    None
}

/// The pass `j` in which `a` is `v` (for an odd step, which reaches every value).
pub(crate) fn pass_of(a: Aff, v: u32) -> Option<u64> {
    if a.d & 1 == 0 {
        return None;
    }
    // The inverse of an odd number mod 2^32, by Newton's iteration.
    let mut inv = a.d;
    for _ in 0..5 {
        inv = inv.wrapping_mul(2u32.wrapping_sub(a.d.wrapping_mul(inv)));
    }
    Some(v.wrapping_sub(a.b).wrapping_mul(inv) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PsyQ's `VSync` wait (Tomb Raider's, at 8005F760).
    const VSYNC: [u32; 13] = [
        0x8FA20010, 0x00000000, 0x2442FFFF, 0xAFA20010, 0x8FA20010, 0x00000000, 0x1443000C, 0x00000000,
        // loc_8005F7AC
        0x3C028007, 0x8C425684, 0x00000000, 0x0044102A, 0x1440FFE8,
    ];

    fn vsync_trace() -> Vec<(u32, u32)> {
        let mut t: Vec<(u32, u32)> =
            VSYNC[..7].iter().enumerate().map(|(k, &i)| (0x8005_F760 + 4 * k as u32, i)).collect();
        t.push((0x8005_F77C, VSYNC[7]));
        t.extend(VSYNC[8..].iter().enumerate().map(|(k, &i)| (0x8005_F7AC + 4 * k as u32, i)));
        t.push((0x8005_F7C0, 0));
        t
    }

    #[test]
    fn vsync_wait_runs_until_the_counter_or_the_timeout() {
        let sp = 0x801F_FF00u32;
        let (count, target, timeout) = (100u32, 101u32, 50u32);
        let read = |addr: u32, _: Width| match addr {
            a if a == sp + 16 => Some(timeout),
            0x8007_5684 => Some(count),
            _ => Some(0),
        };
        let mut regs = [Aff::default(); 32];
        regs[2] = Aff::c(1);
        regs[3] = Aff::c(u32::MAX);
        regs[4] = Aff::c(target);
        regs[29] = Aff::c(sp);
        // The last pass's load of the counter has landed.
        let load = (0, Aff::c(count));
        let out = evaluate(&vsync_trace(), regs, load, (0x8005_F760, 0x8005_F764), read).unwrap();
        // The timeout hits -1 in pass 50 (from 49 passes on), so 50 passes repeat.
        assert_eq!(out.passes, 50);
        assert_eq!(out.stores.len(), 1);
        assert_eq!(out.stores[0].2, Aff { b: 49, d: u32::MAX });
        assert_eq!(pass_of(out.stores[0].2, 40), Some(9));
        assert_eq!(pass_of(Aff { b: 7, d: 3 }, 7 + 3 * 1000), Some(1000));
        // Already timed out: the pass doesn't repeat.
        let read0 = |addr: u32, w: Width| if addr == sp + 16 { Some(0) } else { read(addr, w) };
        assert!(evaluate(&vsync_trace(), regs, load, (0x8005_F760, 0x8005_F764), read0).is_none_or(|o| o.passes == 0));
    }
}

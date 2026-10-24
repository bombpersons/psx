//! A cycle profile for analysis runs: the CPU cycles each instruction took
//! (Mednafen's timing, including time DMA held the CPU), by PC, and the call
//! targets seen. Functions are the call targets (and the exception vector);
//! each PC is charged to the nearest entry at or below it in the same memory,
//! so code reached only by a plain jump counts towards the function before it.

use std::collections::BTreeMap;

use crate::cpu::StepInfo;

const RAM_WORDS: usize = 0x20_0000 / 4;
const BIOS_WORDS: usize = 0x8_0000 / 4;
/// Physical address of the exception vector (0x80000080).
const VECTOR: u32 = 0x80;

fn is_call(i: u32) -> bool {
    i >> 26 == 0x03 || (i >> 26 == 0 && i & 0x3F == 0x09) || (i >> 26 == 1 && (i >> 16) & 0x1E == 0x10)
}

/// Where a physical PC is counted: main RAM (mirrored), the BIOS, or neither.
fn slot(phys: u32) -> Option<(bool, usize)> {
    match phys {
        0..=0x7F_FFFF => Some((true, (phys as usize & 0x1F_FFFF) / 4)),
        0x1FC0_0000..=0x1FC7_FFFF => Some((false, (phys as usize - 0x1FC0_0000) / 4)),
        _ => None,
    }
}

#[derive(Clone, Copy, Default)]
struct Cost {
    cycles: u64,
    instructions: u64,
}

pub struct Profiler {
    ram: Vec<Cost>,
    bios: Vec<Cost>,
    /// Instructions elsewhere (the scratchpad, I/O): one bucket.
    other: Cost,
    /// Call targets (physical) and how often each was called.
    entries: BTreeMap<u32, u64>,
    /// Steps until the callee's first instruction (the delay slot runs first).
    pending_call: u8,
    last_cycles: u64,
    pub total_cycles: u64,
    pub instructions: u64,
}

/// One function's share of a run.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FunctionCost {
    /// Physical entry address.
    pub entry: u32,
    pub cycles: u64,
    pub instructions: u64,
    /// Times it was called (0 for the run's starting code).
    pub calls: u64,
    /// Its hottest instruction and that instruction's cycles.
    pub hottest_pc: u32,
    pub hottest_cycles: u64,
}

impl Profiler {
    /// `cycles`: the CPU's cycle count now, before the first profiled step.
    pub fn new(cycles: u64) -> Self {
        let mut entries = BTreeMap::new();
        entries.insert(VECTOR, 0);
        Profiler {
            ram: vec![Cost::default(); RAM_WORDS],
            bios: vec![Cost::default(); BIOS_WORDS],
            other: Cost::default(),
            entries,
            pending_call: 0,
            last_cycles: cycles,
            total_cycles: 0,
            instructions: 0,
        }
    }

    /// After each step; `cycles` is the CPU's cycle count after it.
    #[inline]
    pub fn after_step(&mut self, info: &StepInfo, cycles: u64) {
        let spent = cycles - self.last_cycles;
        self.last_cycles = cycles;
        self.total_cycles += spent;
        self.instructions += 1;
        let phys = info.pc & 0x1FFF_FFFF;
        let cost = match slot(phys) {
            Some((true, i)) => &mut self.ram[i],
            Some((false, i)) => &mut self.bios[i],
            None => &mut self.other,
        };
        cost.cycles += spent;
        cost.instructions += 1;
        if info.exception.is_some() {
            *self.entries.entry(VECTOR).or_default() += 1;
            self.pending_call = 0;
        } else if self.pending_call > 0 {
            self.pending_call -= 1;
            if self.pending_call == 0 {
                *self.entries.entry(phys).or_default() += 1;
            }
        }
        if is_call(info.instruction) && info.exception.is_none() {
            self.pending_call = 2;
        }
    }

    /// Functions by cycles, most first; `start` (the run's first PC) is an
    /// entry too, so the code before the first call has one.
    pub fn functions(&self, start: u32) -> Vec<FunctionCost> {
        let mut entries = self.entries.clone();
        entries.entry(start & 0x1FFF_FFFF).or_insert(0);
        let mut out: BTreeMap<u32, FunctionCost> = BTreeMap::new();
        let mut charge = |phys: u32, c: &Cost| {
            if c.instructions == 0 {
                return;
            }
            let base = if phys < 0x80_0000 { 0 } else { 0x1FC0_0000 };
            // The nearest entry at or below, in the same memory.
            let entry = entries.range(base..=phys).next_back().map_or(phys, |(&e, _)| e);
            let f = out.entry(entry).or_insert_with(|| FunctionCost {
                entry,
                cycles: 0,
                instructions: 0,
                calls: entries.get(&entry).copied().unwrap_or(0),
                hottest_pc: phys,
                hottest_cycles: 0,
            });
            f.cycles += c.cycles;
            f.instructions += c.instructions;
            if c.cycles > f.hottest_cycles {
                (f.hottest_pc, f.hottest_cycles) = (phys, c.cycles);
            }
        };
        for (i, c) in self.ram.iter().enumerate() {
            charge(i as u32 * 4, c);
        }
        for (i, c) in self.bios.iter().enumerate() {
            charge(0x1FC0_0000 + i as u32 * 4, c);
        }
        let mut v: Vec<FunctionCost> = out.into_values().collect();
        v.sort_by_key(|f| std::cmp::Reverse(f.cycles));
        v
    }

    /// Cycles spent outside RAM and the BIOS.
    pub fn other_cycles(&self) -> u64 {
        self.other.cycles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(p: &mut Profiler, pc: u32, instruction: u32, cycles: &mut u64, cost: u64) {
        *cycles += cost;
        p.after_step(&StepInfo { pc, instruction, ..Default::default() }, *cycles);
    }

    #[test]
    fn cycles_go_to_the_called_function() {
        let mut c = 0;
        let mut p = Profiler::new(0);
        // 0x80010000: jal 0x80020000; nop (delay slot); then the callee runs.
        step(&mut p, 0x8001_0000, 0x0C00_8000, &mut c, 2);
        step(&mut p, 0x8001_0004, 0, &mut c, 1);
        step(&mut p, 0x8002_0000, 0, &mut c, 5);
        step(&mut p, 0x8002_0004, 0, &mut c, 7);
        let f = p.functions(0x8001_0000);
        assert_eq!(f[0].entry, 0x2_0000);
        assert_eq!((f[0].cycles, f[0].instructions, f[0].calls), (12, 2, 1));
        assert_eq!((f[0].hottest_pc, f[0].hottest_cycles), (0x2_0004, 7));
        assert_eq!((f[1].entry, f[1].cycles, f[1].calls), (0x1_0000, 3, 0));
        assert_eq!(p.total_cycles, 15);
    }
}

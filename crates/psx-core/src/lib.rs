//! Native PlayStation emulation core (in development).
//!
//! Stage 1 is the CPU: an R3000A interpreter over a bus with RAM, scratchpad,
//! BIOS and an I/O register file. Devices (interrupt controller, timers, DMA,
//! GPU, CD-ROM, SPU) come next. Correctness is established by lockstep
//! comparison against Mednafen (`psx-oracle lockstep`), not assumed.

pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod disasm;
pub mod dma;
mod loops;
pub mod mdec;
pub mod memcard;
pub mod observe;
pub mod pad;
pub mod profile;
pub mod sio0;
pub mod spu;
pub mod state;
pub mod timers;
pub mod trace;
pub mod video;
pub mod xa;

pub use bus::{Bus, Width};
pub use cpu::{Cpu, StepInfo};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Machine {
    pub cpu: Cpu,
    pub bus: Bus,
    #[serde(skip)]
    spin: Spin,
}

/// The last I/O accesses' states, for [`Machine::skip_spin`].
#[derive(Default)]
struct Spin {
    slots: [Option<SpinSlot>; 4],
    next: usize,
    /// Wait-loop probes that found nothing in a row, and the run ends to let
    /// pass before the next (see `Machine::skip_loop`).
    loop_fails: u32,
    loop_backoff: u32,
    trace: Vec<(u32, u32)>,
    /// The wait loop last found, kept across the events that end its skips.
    wait: Option<Wait>,
}

/// A wait loop's pass as [`loops::evaluate`] found it, pass 0 being the one
/// after the two that ran for real: `regs` and `load` are the CPU's at the
/// start of each pass.
struct Wait {
    pc: u32,
    next_pc: u32,
    key: cpu::SpinKey,
    bus: u64,
    period: u64,
    trace: Vec<(u32, u32)>,
    regs: [loops::Aff; 32],
    load: (usize, loops::Aff),
    out: loops::Outcome,
    /// When the last skip ended: the time the pass after the one then
    /// starting begins, and its number.
    next: Option<(u64, u64)>,
}

struct SpinSlot {
    key: cpu::SpinKey,
    bus: (u8, u64),
    cycles: u64,
    steps: u64,
    breaks: u64,
}

impl Machine {
    /// Power-on state: CPU at the BIOS reset vector.
    pub fn new(bios: Vec<u8>) -> Self {
        Machine { cpu: Cpu::new(), bus: Bus::new(bios), spin: Spin::default() }
    }

    pub fn step(&mut self) -> StepInfo {
        let mut info = StepInfo::default();
        self.step_with(&mut info);
        info
    }

    #[inline(always)]
    fn step_with<I: cpu::StepSink>(&mut self, info: &mut I) {
        if self.bus.obs.bios.is_some() {
            self.observe_bios_call();
        }
        self.cpu.irq_line = self.bus.irq_line();
        // While a manual-mode DMA holds the CPU, it spins in place: the time
        // is the instruction's that started it (or that it interrupted).
        while !self.cpu.step_with(&mut self.bus, info) {
            self.bus.end_instruction();
        }
        self.finish_step();
    }

    /// After an instruction: due events, a frame's end, and the time a DMA
    /// then holds the CPU.
    #[inline(always)]
    fn finish_step(&mut self) {
        self.bus.end_instruction();
        while self.bus.dma.halted && !self.cpu.step_with(&mut self.bus, &mut ()) {
            self.bus.end_instruction();
        }
    }

    /// Step while `go` holds, at most `max_steps` times; the same as
    /// [`Machine::step`] each time, without the record. Between device
    /// events, and with no I/O access, nothing but the CPU changes (as in
    /// Mednafen's `RunReal`, which runs to `next_event_ts`): instructions
    /// then run without handing time to the bus or checking events, until
    /// the CPU's clock reaches [`Bus::deadline`] or an access outside RAM,
    /// BIOS and scratchpad clears it.
    #[inline(always)]
    fn run_steps(&mut self, max_steps: u64, until_cycle: u64, go: impl Fn(&Machine) -> bool) -> u64 {
        let mut n = 0;
        if self.bus.obs.bios.is_some() || self.bus.obs.active() {
            while n < max_steps && go(self) {
                self.step_with(&mut ());
                n += 1;
            }
            return n;
        }
        while n < max_steps && go(self) {
            // `go` can only change at an instruction's ending (a frame's end)
            // or with the CPU's clock, which `until_cycle` bounds.
            if !self.bus.dma.halted {
                // The run's end is a deadline too (finishing an instruction
                // there runs no event that is not due).
                self.bus.deadline = self.bus.next_deadline().min(until_cycle);
                self.cpu.irq_line = self.bus.irq_line();
                self.cpu.refresh_int();
                let start = n;
                n += self.cpu.run_unsynced(&mut self.bus, max_steps - n);
                if n > start {
                    self.cpu.sync(&mut self.bus);
                    if self.cpu.cycles >= self.bus.deadline {
                        let io = self.bus.deadline == 0;
                        self.finish_step();
                        // Not past the run's end (a frame's end, say).
                        if !self.bus.dma.halted && !self.bus.log_io && go(self) {
                            if io {
                                self.skip_spin(&mut n, max_steps, until_cycle);
                            } else {
                                self.skip_loop(&mut n, max_steps, until_cycle);
                            }
                        }
                    }
                    continue;
                }
            }
            // Held by a DMA, or at an event's time: one step as `step` takes it.
            self.step_with(&mut ());
            n += 1;
        }
        n
    }

    /// After an I/O access, skip a polling loop's passes up to the next
    /// event. When the CPU's state (times relative to its clock) and the
    /// CD-ROM index are as at an earlier access, and nothing in between could
    /// have changed anything else ([`Bus::spin_breaks`]: no events, no
    /// interrupts, stores, GTE operations, cache fills or I/O but pure reads),
    /// every pass from here repeats that one exactly, shifted in time, until
    /// an event. Whole passes ending before the next event are skipped by
    /// moving the clocks and the step count on.
    fn skip_spin(&mut self, n: &mut u64, max_steps: u64, until_cycle: u64) {
        let key = self.cpu.spin_key();
        let bus = self.bus.spin_key();
        let (cycles, breaks) = (self.cpu.cycles, self.bus.spin_breaks);
        let found = self.spin.slots.iter().position(|s| s.as_ref().is_some_and(|s| s.key == key));
        if let Some(i) = found {
            let s = self.spin.slots[i].as_mut().unwrap();
            // (`n` counts from each run's start: a slot from an earlier run
            // can be ahead of it.)
            if s.breaks == breaks && s.bus == bus && cycles > s.cycles && *n >= s.steps {
                let (period, steps) = (cycles - s.cycles, *n - s.steps);
                let limit = self.bus.next_deadline().min(until_cycle);
                let mut k = limit.saturating_sub(cycles + 1) / period;
                if let Some(m) = (max_steps - *n).checked_div(steps) {
                    k = k.min(m);
                }
                if k > 0 {
                    self.cpu.skip_cycles(k * period);
                    self.bus.skip_cycles(k * period);
                    *n += k * steps;
                }
            }
            s.cycles = self.cpu.cycles;
            s.steps = *n;
            s.breaks = breaks;
            s.bus = bus;
            return;
        }
        self.spin.slots[self.spin.next] = Some(SpinSlot { key, bus, cycles, steps: *n, breaks });
        self.spin.next = (self.spin.next + 1) % self.spin.slots.len();
    }

    /// After an event, skip a wait loop's passes up to the next one, when
    /// they touch only RAM (`crate::loops`): the loop found before, if the
    /// CPU is still on its course, else one a probe finds. Probes that find
    /// none back off, up to 64 run ends.
    fn skip_loop(&mut self, n: &mut u64, max_steps: u64, until_cycle: u64) {
        if self.spin.wait.is_some() {
            self.resume_loop(n, max_steps, until_cycle);
            return;
        }
        if self.spin.loop_backoff > 0 {
            self.spin.loop_backoff -= 1;
            return;
        }
        match self.probe_loop(n, max_steps, until_cycle) {
            Some(true) => self.spin.loop_fails = 0,
            Some(false) => {}
            None => {
                self.spin.loop_fails = (self.spin.loop_fails + 1).min(6);
                self.spin.loop_backoff = 1 << self.spin.loop_fails;
            }
        }
    }

    /// Set up a run as `run_steps` does; false when an interrupt is due.
    fn loop_run_setup(&mut self, until_cycle: u64) -> bool {
        self.bus.deadline = self.bus.next_deadline().min(until_cycle);
        self.cpu.irq_line = self.bus.irq_line();
        self.cpu.refresh_int();
        !self.cpu.int_pending_cached()
    }

    /// Step until the CPU is at `pcs` (its PC and next PC) again, at most
    /// `max` steps, recording them in [`Spin::trace`]: Some(true) there,
    /// Some(false) when the run ended first (after finishing the step that
    /// ended it), None when it wasn't back within `max`.
    fn loop_steps(&mut self, pcs: (u32, u32), max: usize, n: &mut u64, max_steps: u64) -> Option<bool> {
        let mut r = None;
        for _ in 0..max {
            if *n >= max_steps {
                r = Some(false);
                break;
            }
            let at = self.cpu.pc;
            self.cpu.step_unsynced(&mut self.bus);
            *n += 1;
            self.spin.trace.push((at, self.cpu.last_instr()));
            if self.cpu.cycles >= self.bus.deadline {
                self.cpu.sync(&mut self.bus);
                self.finish_step();
                return Some(false);
            }
            if (self.cpu.pc, self.cpu.next_pc()) == pcs {
                r = Some(true);
                break;
            }
        }
        self.cpu.sync(&mut self.bus);
        r
    }

    /// What a pass's time depends on in the devices: the DMA's cycle stealing.
    fn loop_bus_key(&self) -> u64 {
        self.bus.dma.cycle_steal
    }

    fn read_mem(&self, addr: u32, w: Width) -> Option<u32> {
        let a = cpu::segment_mask(addr);
        if a < 0x0080_0000 {
            Some(self.bus.read_ram(a, w))
        } else {
            bus::scratch_offset(addr).map(|o| self.bus.read_scratch(o, w))
        }
    }

    /// Run two passes of the loop the CPU may be in, then skip the passes
    /// that repeat them. Some(false) when an interrupt is due, None when it
    /// found no such loop (or the run ended first, at an event, an I/O
    /// access or the step limit).
    fn probe_loop(&mut self, n: &mut u64, max_steps: u64, until_cycle: u64) -> Option<bool> {
        const MAX_PASS: usize = 48;
        if !self.loop_run_setup(until_cycle) {
            return Some(false);
        }
        // With the cache isolated, stores reach the instruction cache.
        if self.cpu.cop0[cpu::cop0::SR] & (1 << 16) != 0 {
            return None;
        }
        let pcs = (self.cpu.pc, self.cpu.next_pc());
        self.spin.trace.clear();
        struct Snap {
            key: cpu::SpinKey,
            bus: u64,
            regs: [u32; 32],
            load: (usize, u32),
            cycles: u64,
        }
        let mut snaps = Vec::with_capacity(2);
        for _ in 0..2 {
            // A run that ends first counts as no loop found, so that probes
            // outside loops back off.
            self.loop_steps(pcs, MAX_PASS, n, max_steps).filter(|&back| back)?;
            snaps.push(Snap {
                key: self.cpu.loop_key(),
                bus: self.loop_bus_key(),
                regs: self.cpu.regs,
                load: self.cpu.load_raw(),
                cycles: self.cpu.cycles,
            });
        }
        let trace = std::mem::take(&mut self.spin.trace);
        let steps = trace.len() / 2;
        let (a, b) = trace.split_at(steps);
        let (s2, s1) = (snaps.pop().unwrap(), &snaps[0]);
        let same = a == b && s1.key == s2.key && s1.bus == s2.bus && s2.cycles > s1.cycles;
        let w = same
            .then(|| {
                let regs =
                    std::array::from_fn(|k| loops::Aff { b: s2.regs[k], d: s2.regs[k].wrapping_sub(s1.regs[k]) });
                let load = (s2.load.0, loops::Aff { b: s2.load.1, d: s2.load.1.wrapping_sub(s1.load.1) });
                let out = loops::evaluate(b, regs, load, pcs, |a, w| self.read_mem(a, w))?;
                Some(Wait {
                    pc: pcs.0,
                    next_pc: pcs.1,
                    key: s2.key,
                    bus: s2.bus,
                    period: s2.cycles - s1.cycles,
                    trace: b.to_vec(),
                    regs,
                    load,
                    out,
                    next: None,
                })
            })
            .flatten()
            .filter(|w| self.loop_fetches_alike(w));
        self.spin.trace = trace;
        let mut w = w?;
        w.next = self.skip_passes(&w, 0, n, max_steps, until_cycle);
        self.spin.wait = Some(w);
        Some(true)
    }

    /// Run to the found loop's start and, if the CPU is there as one of its
    /// passes would leave it, skip passes again; else forget the loop.
    fn resume_loop(&mut self, n: &mut u64, max_steps: u64, until_cycle: u64) {
        if !self.loop_run_setup(until_cycle) {
            return;
        }
        let mut w = self.spin.wait.take().unwrap();
        // Where the next pass should start: an event comes first (run on to
        // it and look again), or run there and check the state in place.
        if let Some((t, j)) = w.next {
            if self.cpu.cycles < t && self.bus.deadline <= t {
                self.spin.wait = Some(w);
                return;
            }
            match self.run_to_head(&w, t, n, max_steps) {
                Some(true) if self.loop_key_is(&w) && w.out.passes > j && self.loop_at(&w, j) => {
                    w.next = self.skip_passes(&w, j, n, max_steps, until_cycle);
                    self.spin.wait = Some(w);
                    return;
                }
                Some(_) => return,
                None => {}
            }
        }
        self.spin.trace.clear();
        match self.loop_steps((w.pc, w.next_pc), w.trace.len(), n, max_steps) {
            Some(true) => {}
            Some(false) => {
                self.spin.wait = Some(w);
                return;
            }
            None => return,
        }
        if let Some(j) = self.loop_pass(&w) {
            w.next = self.skip_passes(&w, j, n, max_steps, until_cycle);
            self.spin.wait = Some(w);
        }
    }

    /// Run at full speed to time `t`, before the run's next event, when a
    /// pass of `w` should start (the clock, at the loop's start, saying
    /// nothing else of the CPU's course: its fetches are as they were, since
    /// they take the same time). Some(true) when the CPU is then at the
    /// loop's start, Some(false) when it is elsewhere (the run's step
    /// finished as `run_steps` would), None when it is already past `t`.
    fn run_to_head(&mut self, w: &Wait, t: u64, n: &mut u64, max_steps: u64) -> Option<bool> {
        let deadline = self.bus.deadline;
        if self.cpu.cycles > t {
            return None;
        }
        if self.cpu.cycles < t {
            self.bus.deadline = t;
            *n += self.cpu.run_unsynced(&mut self.bus, max_steps - *n);
            self.cpu.sync(&mut self.bus);
        }
        let io = self.bus.deadline == 0;
        if !io && self.cpu.cycles == t && (self.cpu.pc, self.cpu.next_pc()) == (w.pc, w.next_pc) {
            return Some(true);
        }
        if io || self.cpu.cycles >= deadline {
            self.finish_step();
        }
        Some(false)
    }

    /// The CPU's timing state and the DMA's as when `w` was found.
    fn loop_key_is(&self, w: &Wait) -> bool {
        self.cpu.loop_key_is(&w.key) && self.loop_bus_key() == w.bus
    }

    /// Whether every fetch of a pass of `w` takes the time it took.
    fn loop_fetches_alike(&self, w: &Wait) -> bool {
        w.trace.iter().all(|&(pc, _)| self.cpu.fetches_alike(&self.bus, pc))
    }

    /// Whether the CPU, at the loop's start, and the RAM it reads and
    /// stores are where pass `j` of `w` starts.
    fn loop_at(&self, w: &Wait, j: u64) -> bool {
        let (regs, load) = (&self.cpu.regs, self.cpu.load_raw());
        (1..32).all(|r| regs[r] == w.regs[r].at(j))
            && load.0 == w.load.0
            && load.1 == w.load.1.at(j)
            && w.out.stores.iter().all(|&(addr, width, v)| self.read_mem(addr, width) == Some(v.at(j.wrapping_sub(1))))
            && w.out.reads.iter().all(|&(addr, width, v)| self.read_mem(addr, width) == Some(v))
    }

    /// The pass of `w` the CPU's state (at the loop's start) is the start
    /// of, counting from `w`'s pass 0, if it is one.
    fn loop_pass(&self, w: &Wait) -> Option<u64> {
        if !self.loop_key_is(w) {
            return None;
        }
        if !self.loop_fetches_alike(w) {
            return None;
        }
        let (regs, load) = (&self.cpu.regs, self.cpu.load_raw());
        let mut stored = Vec::with_capacity(w.out.stores.len());
        for &(addr, width, _) in &w.out.stores {
            stored.push(self.read_mem(addr, width)?);
        }
        // The pass number, from something that moves by an odd step; 0 when
        // nothing moves.
        let moving = (1..32).find(|&r| w.regs[r].d != 0).map(|r| (w.regs[r], regs[r], 0));
        let moving = moving
            .or_else(|| (w.load.1.d != 0).then_some((w.load.1, load.1, 0)))
            .or_else(|| w.out.stores.iter().zip(&stored).find(|(s, _)| s.2.d != 0).map(|(s, &v)| (s.2, v, 1)));
        let j = match moving {
            Some((a, v, after)) => loops::pass_of(a, v)?.wrapping_add(after),
            None => 0,
        };
        (j < w.out.passes && self.loop_at(w, j)).then_some(j)
    }

    /// Skip passes of `w` from its pass `j` (the CPU at its start), up to
    /// the next event, the step limit or the last pass that repeats it.
    /// Returns when the pass after the one the CPU is then at the start of
    /// begins, and its number, for [`Machine::run_to_head`].
    fn skip_passes(&mut self, w: &Wait, j: u64, n: &mut u64, max_steps: u64, until_cycle: u64) -> Option<(u64, u64)> {
        let steps = w.trace.len() as u64;
        let limit = self.bus.next_deadline().min(until_cycle);
        let cycles = self.cpu.cycles;
        let k = (limit.saturating_sub(cycles + 1) / w.period).min(w.out.passes - j).min((max_steps - *n) / steps);
        let next = (j + k + 1 < w.out.passes).then_some((cycles + (k + 1) * w.period, j + k + 1));
        if k == 0 {
            return next;
        }
        for (r, a) in self.cpu.regs.iter_mut().zip(w.regs).skip(1) {
            *r = a.at(j + k);
        }
        self.cpu.set_load_value(w.load.1.at(j + k));
        for &(addr, width, v) in &w.out.stores {
            let _ = self.bus.write(addr, width, v.at(j + k - 1));
        }
        self.cpu.skip_cycles(k * w.period);
        self.bus.skip_cycles(k * w.period);
        *n += k * steps;
        next
    }

    fn observe_bios_call(&mut self) {
        let pc = self.cpu.pc & 0x1FFF_FFFF;
        let table = match pc {
            0xA0 => "A0",
            0xB0 => "B0",
            0xC0 => "C0",
            _ => return,
        };
        let r = &self.cpu.regs;
        let function = r[9] & 0xFF;
        *self.bus.obs.bios_counts.entry(format!("{table}_{function}")).or_default() += 1;
        if observe::bios_call_is_quiet(table, function) {
            return;
        }
        let path_call =
            matches!((table, function), ("A0", 0x00) | ("A0", 0x42) | ("A0", 0x51) | ("B0", 0x32) | ("B0", 0x42));
        let path = path_call.then(|| {
            let mut s = String::new();
            for i in 0..128 {
                match self.bus.read(r[4].wrapping_add(i), Width::Byte) {
                    Ok(0) | Err(_) => break,
                    Ok(c) => s.push(c as u8 as char),
                }
            }
            s
        });
        let call = observe::BiosCall {
            frame: self.bus.frames,
            cycle: self.bus.now,
            table,
            function,
            args: [r[4], r[5], r[6], r[7]],
            ra: r[31],
            path,
        };
        if let Some(log) = self.bus.obs.bios.as_mut() {
            log.push(call);
        }
    }

    /// Side-load a PS-X EXE the way PCSX-Redux does: run the BIOS until it
    /// reaches the shell entry (0x80030000), then copy the payload, zero BSS
    /// and jump to the entry point with the header's GP/SP.
    pub fn sideload_exe(&mut self, bytes: &[u8], max_steps: u64) -> Result<(), String> {
        let exe = psx_media::PsxExe::parse(bytes).map_err(|e| e.to_string())?;
        let mut n = 0;
        while self.cpu.pc != 0x8003_0000 {
            self.step();
            n += 1;
            if n > max_steps {
                return Err("BIOS did not reach the shell entry (0x80030000)".into());
            }
        }
        for (i, b) in exe.payload(bytes).iter().enumerate() {
            let a = (exe.text_addr as usize + i) & 0x1F_FFFF;
            self.bus.ram[a] = *b;
        }
        for i in 0..exe.bss_size {
            let a = (exe.bss_addr.wrapping_add(i) as usize) & 0x1F_FFFF;
            self.bus.ram[a] = 0;
        }
        let text = exe.payload(bytes).len();
        self.bus.ram_written_range(exe.text_addr as usize & 0x1F_FFFF, text);
        self.bus.ram_written_range(exe.bss_addr as usize & 0x1F_FFFF, exe.bss_size as usize);
        self.cpu.set_pc(exe.pc);
        if exe.gp != 0 {
            self.cpu.regs[28] = exe.gp;
        }
        if let Some(sp) = exe.initial_sp() {
            self.cpu.regs[29] = sp;
            self.cpu.regs[30] = sp;
        }
        Ok(())
    }

    /// Step until the frame counter changes or `max_steps` instructions have
    /// run; returns the number executed. Equivalent to calling [`Machine::step`]
    /// that many times, for run loops that only act at frame boundaries.
    pub fn run_frame_steps(&mut self, max_steps: u64) -> u64 {
        let frame = self.bus.frames;
        self.run_steps(max_steps, u64::MAX, |m| m.bus.frames == frame)
    }

    /// Step until the next instruction starts at or after `cycle` (CPU
    /// cycles since power-on), or `max_steps` have run; returns the number
    /// executed.
    pub fn run_to_cycle(&mut self, cycle: u64, max_steps: u64) -> u64 {
        self.run_steps(max_steps, cycle, |m| m.cpu.cycles < cycle)
    }

    /// Run until `frames` more vblanks have occurred (or `max_steps`).
    pub fn run_frames(&mut self, frames: u64, max_steps: u64) -> u64 {
        let target = self.bus.frames + frames;
        let mut n = 0;
        while self.bus.frames < target && n < max_steps {
            n += self.run_frame_steps(max_steps - n);
        }
        n
    }

    /// Insert a disc (CUE/BIN/ISO/CHD via `psx-media`); `None` means an empty drive.
    pub fn insert_disc(&mut self, disc: Option<psx_media::Disc>) {
        self.bus.insert_disc(disc);
    }
}

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
}

impl Machine {
    /// Power-on state: CPU at the BIOS reset vector.
    pub fn new(bios: Vec<u8>) -> Self {
        Machine { cpu: Cpu::new(), bus: Bus::new(bios) }
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
            self.step_with(&mut ());
            n += 1;
            if self.bus.dma.halted {
                continue;
            }
            // `go` can only change at an instruction's ending (a frame's end)
            // or with the CPU's clock, which `until_cycle` bounds.
            self.bus.deadline = self.bus.next_deadline();
            self.cpu.irq_line = self.bus.irq_line();
            self.cpu.refresh_int();
            let limit = self.bus.deadline.min(until_cycle);
            let start = n;
            while n < max_steps && self.cpu.cycles < limit {
                self.cpu.step_unsynced(&mut self.bus);
                n += 1;
                if self.cpu.cycles >= self.bus.deadline {
                    break;
                }
            }
            if n == start {
                continue;
            }
            self.cpu.sync(&mut self.bus);
            if self.cpu.cycles >= self.bus.deadline {
                self.finish_step();
            }
        }
        n
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

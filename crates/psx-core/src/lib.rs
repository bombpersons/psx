//! Native PlayStation emulation core (in development).
//!
//! Stage 1 is the CPU: an R3000A interpreter over a bus with RAM, scratchpad,
//! BIOS and an I/O register file. Devices (interrupt controller, timers, DMA,
//! GPU, CD-ROM, SPU) come next. Correctness is established by lockstep
//! comparison against PCSX-Redux (`psx lockstep`), not assumed.

pub mod bus;
pub mod cdrom;
pub mod cpu;
pub mod disasm;
pub mod mdec;
pub mod observe;
pub mod sio0;
pub mod spu;
pub mod timers;

pub use bus::{Bus, Width};
pub use cpu::{Cpu, StepInfo};

pub struct Machine {
    pub cpu: Cpu,
    pub bus: Bus,
    /// Interrupt asserted from outside (lockstep following the oracle), held
    /// until software acknowledges I_STAT.
    pub forced_irq: bool,
}

impl Machine {
    /// Power-on state: CPU at the BIOS reset vector.
    pub fn new(bios: Vec<u8>) -> Self {
        Machine { cpu: Cpu::new(), bus: Bus::new(bios), forced_irq: false }
    }

    pub fn step(&mut self) -> StepInfo {
        if self.bus.obs.bios.is_some() {
            self.observe_bios_call();
        }
        if self.bus.take_irq_ack() {
            self.forced_irq = false;
        }
        self.cpu.irq_line = self.forced_irq || self.bus.irq_line();
        let before = self.cpu.cycles;
        let info = self.cpu.step(&mut self.bus);
        self.bus.tick(self.cpu.cycles - before);
        info
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
        let path_call = matches!((table, function), ("A0", 0x00) | ("A0", 0x42) | ("A0", 0x51) | ("B0", 0x32) | ("B0", 0x42));
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

    /// Run until `frames` more vblanks have occurred (or `max_steps`).
    pub fn run_frames(&mut self, frames: u64, max_steps: u64) -> u64 {
        let target = self.bus.frames + frames;
        let mut n = 0;
        while self.bus.frames < target && n < max_steps {
            self.step();
            n += 1;
        }
        n
    }

    /// Insert a disc (CUE/BIN/ISO via `psx-media`); `None` means an empty drive.
    pub fn insert_disc(&mut self, disc: Option<psx_media::Disc>) {
        self.bus.cdrom = cdrom::Cdrom::new(disc);
    }

    /// Take an external interrupt now and keep IP2 asserted until acknowledged.
    pub fn force_interrupt(&mut self) {
        self.forced_irq = true;
        self.cpu.irq_line = true;
        self.cpu.force_interrupt();
    }
}

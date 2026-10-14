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
pub mod sio0;
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
        if self.bus.take_irq_ack() {
            self.forced_irq = false;
        }
        self.cpu.irq_line = self.forced_irq || self.bus.irq_pending();
        let before = self.cpu.cycles;
        let info = self.cpu.step(&mut self.bus);
        self.bus.tick(self.cpu.cycles - before);
        info
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

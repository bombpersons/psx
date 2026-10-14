//! Native PlayStation emulation core (in development).
//!
//! Stage 1 is the CPU: an R3000A interpreter over a bus with RAM, scratchpad,
//! BIOS and an I/O register file. Devices (interrupt controller, timers, DMA,
//! GPU, CD-ROM, SPU) come next. Correctness is established by lockstep
//! comparison against PCSX-Redux (`psx lockstep`), not assumed.

pub mod bus;
pub mod cpu;
pub mod disasm;

pub use bus::{Bus, Width};
pub use cpu::{Cpu, StepInfo};

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
        self.cpu.step(&mut self.bus)
    }
}

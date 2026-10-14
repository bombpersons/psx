//! Smoke test: start Redux paused, read registers, single-step a few times.
use psx_redux::{gdb::GdbSession, Backend};
use std::path::Path;

fn main() {
    let root = std::env::current_dir().unwrap();
    let backend = Backend::from_project(&root).unwrap();
    let dir = std::env::temp_dir().join(format!("psx-gdb-probe-{}", std::process::id()));
    let mut g = GdbSession::start(&backend, Path::new("bios/scph7502.bin"), &dir).unwrap();
    for i in 0..6 {
        let r = g.registers().unwrap();
        println!("{i}: pc={:08x} sr={:08x} cause={:08x} t0={:08x}", r.pc, r.sr, r.cause, r.gpr[8]);
        let stop = g.step().unwrap();
        println!("   step -> {stop}");
    }
    println!("mem @bfc00000: {:02x?}", g.read_memory(0xbfc00000, 8).unwrap());
}

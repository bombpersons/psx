# psx-oracle

Checks the native core against Mednafen's PlayStation emulation, which psx takes as its ground truth.
Developer tool only: it isn't part of the `psx` toolkit, and it is built only on x86_64 Linux.

```bash
cargo run --release -p psx-oracle -- run "media/Crash Bandicoot.cue" --bios bios/scph7502.bin --frames 4504 --replay .psx/native/replay_start_cross.json
cargo run --release -p psx-oracle -- lockstep --bios bios/SCPH1001.BIN --steps 100000000
cargo run --release -p psx-oracle -- lockstep --bios bios/scph7502.bin --disc "media/Crash Bandicoot.cue" --steps 200000000 --cycles
```

## `run`

`run` plays a disc (`.cue`, `.ccd`, `.toc`, `.m3u`, or `.chd`, which is unpacked once to CUE/BIN), a PS-X
EXE, or the BIOS alone with an empty drive. Frames are vblanks, as in `psx run --frames`. Replays use the
`psx run` format (`frame`, `buttons`, `sticks`, `until`). It writes these files in the run directory:

- `ram.bin` (2 MiB), `scratch.bin` (1 KiB), `vram.bin` (1 MiB) and `spu.bin` (512 KiB), for
  `psx ram-diff` and `psx vram-diff`;
- `screen.png`, the displayed picture as Mednafen outputs it, so its width follows the dot clock (560 for
  Crash's 512-pixel mode);
- `vram.png`;
- `result.json`, the report it also prints;
- `writes.jsonl` with `--watch ADDR[:W]` (CPU stores only; DMA writes aren't seen);
- `frames.csv` with `--frame-log`, each frame's CPU cycles and display size;
- `state.mcs` with `--save-state`, which `--from-state` continues from.

The console's region is the BIOS's (the letter in its version string), whatever the disc's, and
Mednafen's BIOS checks are off. Pad 1 is a digital pad (or a DualShock with `--dualshock`) and pad 2 is
unplugged, as in the native core. There are no memory cards. Runs are deterministic.

## `lockstep`

`lockstep` runs the native core and Mednafen in one process from BIOS reset. The drive is empty unless
`--disc` gives a CUE/BIN image, and `--replay` feeds pad 1 as `run` does. Before
each instruction it compares the PC, the instruction word, r1-r31, HI, LO, the delayed load in flight, and
COP0's SR, Cause, EPC and BadVaddr, and the interrupt line (Cause.IP2), which the core raises itself. After each COP2 instruction or LWC2 it also compares the GTE's
64 registers. Any difference stops the run. It also compares the cycle count (Mednafen's master clock) and
reports the instructions after which the difference changed; `--cycles` stops at the first. The core's CPU,
DMA, CD-ROM, GPU command, timer, controller port, SPU and MDEC timing are Mednafen's. The value of an I/O
load that still differs is taken from Mednafen and counted by register (`io_values_adopted_by_register`)
rather than treated as divergence. Instructions before which the interlaced field being displayed (whose lines the GPU skips in
480-line interlace) differs are counted too (`field_differs`). `--adopt-irqs` takes the interrupt line and
that field from Mednafen instead, as lockstep did before the devices' timing was ported. `--trace-adopted
ADDR` prints each value adopted from one register to stderr (step, frame, PC, cycle, both values) to find
where a device's timing parts.

At every vblank, main RAM, the scratchpad, VRAM and SPU RAM are compared too. These differences are reported
(`memory`: frames differing, the first, and any left at the end) but don't stop the run. The comparison runs
when Mednafen's frame ends, and its last device updates there may not have reached the core yet. At the end
of a run Mednafen also finishes its frame after lockstep stops, so `at_end` is only approximate. VRAM differences while Mednafen's GPU still has queued work are counted
separately. `--dump PREFIX` writes both sides' memory at the end for `psx vram-diff` and `psx ram-diff`.

With `--disc "media/Crash Bandicoot.cue"` and a START/CROSS replay, 1.4 billion instructions (4,562 frames, into
gameplay) match cycle for cycle. Every interrupt comes from the core and no I/O value is adopted. VRAM is identical
at every vblank; RAM differs at 18 of them and SPU RAM at 11, but no load reads a differing value. On SCPH-1001 without a disc,
100 million instructions (441 frames into the shell) match cycle for cycle at about 1.8 million instructions a
second, also adopting nothing, with VRAM and the scratchpad identical at every vblank.

## Layout

- `mednafen/` holds the parts of Mednafen 1.32.1 that the PSX module needs. They come from
  <https://mednafen.github.io/releases/files/mednafen-1.32.1.tar.xz> and are copied by `vendor.sh`. The
  frontend-independent core is there, with the PSX module and the libraries it uses (zstd, tremor, mpcdec,
  trio, quicklz, minilzo). Other systems, the SDL frontend, sound output and the CJK fonts are left out.
- `mednafen/include/config.h` is what `configure --enable-libxxx-mode --disable-nls` (with every system
  but `psx` disabled) made on x86_64 Linux with glibc. `WANT_INTERNAL_CJK` and `ENABLE_LIBXXX_MODE` are
  turned off by hand.
- `harness/driver.cpp` is the frontend: the callbacks Mednafen's core expects (`MDFND_*`) and a small C API
  (`mdh_*`). `src/ffi.rs` wraps the C API; `src/run.rs` and `src/lockstep.rs` are the commands. Mednafen
  keeps its state in globals, so there is one emulator per process.
- `build.rs` compiles all of it with the flags Mednafen's makefiles use, always at `-O2`.

## Changes to Mednafen

Each change is marked with a `psx-mednafen:` comment. `vendor.sh` overwrites the copy, so after an upgrade
put these back:

- `src/psx/gpu.cpp`: counts vblanks (`psxm_vblanks`) and ends `Emulate` at each one. Mednafen normally ends
  a frame near vblank only after scanline 0 has been reached, so its first frame spans two fields.
- `src/psx/cpu.h`: `GetPendingLoad` (the delayed load in flight) and `IsHalted` (DMA holds the CPU; a CPU
  hook then runs again at the same PC without executing it), for lockstep.
- `src/mednafen.cpp`, `src/mednafen-driver.h`: `MDFNI_LoadNoDisc`, a system with its drive closed and
  empty, for BIOS-only runs.

None of these should alter emulation: ending `Emulate` early uses the same exit Mednafen takes near vblank, and only returns to the frontend sooner.

## Licence

GPL-2.0-or-later, as Mednafen is (`mednafen/COPYING`; the bundled libraries keep their own notices beside
their sources). The rest of the workspace is MIT/Apache and doesn't link this crate: `psx-oracle` is its
own program.

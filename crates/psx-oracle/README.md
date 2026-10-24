# psx-oracle

Checks the native core against Mednafen's PlayStation emulation, which psx takes as its ground truth.
Developer tool only: it isn't part of the `psx` toolkit, and it is built only on x86_64 Linux.

```bash
cargo run --release -p psx-oracle -- run "media/Crash Bandicoot.cue" --bios bios/scph7502.bin --frames 4504 --replay .psx/native/replay_start_cross.json
cargo run --release -p psx-oracle -- lockstep --bios bios/SCPH1001.BIN --steps 100000000
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

`lockstep` runs the native core and Mednafen in one process from BIOS reset, with an empty drive. Before
each instruction it compares the PC, the instruction word, r1-r31, HI, LO, the delayed load in flight, and
COP0's SR, Cause (except IP2), EPC and BadVaddr. After each COP2 instruction or LWC2 it also compares the GTE's
64 registers. Any difference stops the run. It also compares the cycle count (Mednafen's master clock) and
reports the instructions after which the difference changed; `--cycles` stops at the first. The core's CPU and
DMA timing are Mednafen's, but other devices' timing isn't yet. So three things come from Mednafen and are
counted (I/O values by register) rather than treated as divergence:

- the values of I/O loads;
- the interrupt line, Cause.IP2;
- the interlaced field being displayed, whose lines the GPU skips in 480-line interlace.

Interrupts then happen at the same instruction in both.

At every vblank, main RAM, the scratchpad, VRAM and SPU RAM are compared too. These differences are reported
(`memory`: frames differing, the first, and any left at the end) but don't stop the run, because they come from
devices whose timing differs. VRAM differences while Mednafen's GPU still has queued work are counted
separately. `--dump PREFIX` writes both sides' memory at the end for `psx vram-diff` and `psx ram-diff`.

On SCPH-1001, 300 million instructions (1,174 frames into the shell) match, cycle for cycle, at about 2 million
instructions a second. RAM, the scratchpad and VRAM are identical at every vblank. Only the SPU's voice capture buffers differ
while sound plays, because they follow the sample clock.

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

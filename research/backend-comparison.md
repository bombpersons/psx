# R1/M1: PCSX-Redux as the first backend

Date: 2026-10-14. Host: Debian 13, AMD Radeon RX 580. No display was used for any run (`DISPLAY` unset).

## Identities

| Item | Value |
|---|---|
| PCSX-Redux | `grumpycoders/pcsx-redux` @ `24f710490572463e1d8752921e5b2b26469d5de6`, built with `make pcsx-redux` (GCC 14.2) |
| Redux binary sha256 | `bc5dfbb293db60eee325a658bd7564c777e13d73a8b1a8688bb9f00fef93b91f` |
| BIOS | `bios/SCPH1001.BIN`, sha256 `71af94d1…99d3` (Redux reports "SCPH-1001 - DTLH-3000 (US)") |
| Probe | `tools/probes/basic` (PSYQo, nugget @ the same Redux commit, `mipsel-none-elf-gcc` 16.2.0) |
| Driver | `tools/probes/m1_experiments.py` → `psx run` → `crates/psx-redux` → `tools/redux/psx_job.lua` |

The probe EXE has known RAM locations, taken from the ELF:

- `g_probe_last_x` @ `0x80014800`
- `g_probe_cross` @ `0x80014804`
- `g_probe_frames` @ `0x80014808`
- `g_probe_magic` @ `0x8001480C`

## How a job runs

- **One Redux process per job.** The job directory holds `params.lua`, a generated `job.lua`, a private
  `config/` (`-portable`) with a seeded `pcsx.json`, and all outputs. Nothing is shared between jobs.
- **Flags:** `-no-ui -run -portable <dir>/config -bios <abs> [-exe|-iso <abs>] -interpreter -debugger -dofile <dir>/job.lua`.
- **Frames** are `GPU::Vsync` events counted from the start of the job, or from `SaveStateLoaded` when a state is loaded.
- **Outputs:**
  - `ram.bin` (2 MiB), `scratch.bin`, `vram.bin` (1 MiB) and `screen.bin`, plus a PNG of the screen and of VRAM
  - registers, CPU cycles and `state.bin`
  - `writes.jsonl` with watch hits: frame, address, width, cycle, `pc`/`ra`/`sp`, and the word before the write

## Pitfalls found while bringing Redux up headless

1. **Relative paths.** Redux runs inside the job directory, so every input path must be absolute.
2. **Startup order.** `-dofile` runs before `-exec`, so `JOB_DIR` has to be baked into the generated script.
3. **`-exe` without `-run`.** The emulator stays paused after the shell hijack loads the EXE.
4. **Audio-clock pacing.** Emulation is paced by the audio sink, and the headless dummy driver barely calls back (about 3 fps).
   - Fix: `{"SPU":{"NullSync":true,"Speed":0}}` in the private `pcsx.json`.
   - `NullSync` is only read when audio starts, so setting it from Lua is too late.
   - `-testmode` implies safe mode, which skips `pcsx.json`, so it is not used.
5. **Save-state timing.** A state loaded during startup is clobbered by boot (the run then hit a reserved-opcode exception).
   - Fix: request the load on the first vsync and start counting on `ExecutionFlow::SaveStateLoaded`.

## Results

| Experiment | Result |
|---|---|
| Probe contract after 120 frames | magic `0x50535831` ✓. Frame counter 16 and `last_x` 32 (16+16) agree, so the probe started on vsync ~104 after BIOS boot |
| Write attribution | Watch on `0x80014808`: 17 hits. Hit 1 is `pc=0x80010044`, which is crt0 `_bss_init: sw zero,0(t0)`. Hits 2–17 are `pc=0x80010380`, which is `sw v0,0x4808(v1)` in `ProbeScene::frame`. Both were checked against `objdump` of `basic.elf` |
| Determinism | Two 300-frame interpreter runs have identical RAM, scratchpad, VRAM, screen, registers and cycle count |
| Observation effect | The same 300 frames with a write watch (197 hits) are identical to the unobserved run in every compared output |
| Save/resume | A straight 300-frame run matches 180 frames → `state.bin` (21 MB) → resume for 120 frames. Inputs: Cross held from frame 150 to 200, so the hold spans the split. Both runs count 50 Cross frames, and all compared outputs are identical |
| Throughput (Redux only, 600 frames including BIOS boot) | Interpreter + debugger: 6.4 s, about 94 fps or 1.6× real time. Dynarec (`--fast`): 1.4 s, about 430 fps or 7× real time |
| Interpreter vs dynarec | Cycle count, VRAM and screen are identical, but the stop PC differs (`0x80011610` vs `0x800122cc`) and 22 kernel RAM bytes around `0xE204` differ. The dynarec can only stop at block boundaries, so captured CPU context is not phase-identical |

## Consequences for the tool

- Redux is a sound first backend for bounded, deterministic, isolated runs with CPU write attribution.
- **Recording the mode is mandatory.** The CPU mode (`interpreter` vs `--fast`) belongs to run identity. States and comparisons must not mix modes.
- **Process start cost** is about 0.3 s, and the BIOS boot is about 100 vsyncs. Stateless `from_state` calls are cheap enough that long-lived sessions are not needed yet.

## Open questions

- **DMA writes.** Do write breakpoints fire for DMA? Probably not, since breakpoints are checked in the CPU memory path. This needs a DMA probe; the expected answer is that DMA writers are invisible to `--watch` (R3).
- **CD loads.** Are CD-ROM DMA loads (overlays) visible? This is the next experiment, using the kernel and CD logging on Croc.
- **Independent oracle.** A screenshot comparison against DuckStation regtest has not been done yet. Mednafen 1.32.1 has replaced Redux as the oracle (`crates/psx-oracle`, built from its sources). On Crash's N. Sanity Beach (scph7502, `replay_start_cross.json`, 4504 frames) it draws the beach without the tearing past x=1023 that the pinned Redux shares with the old `psx-gpu`. After 4504 frames its cycle count is 0.3% above the native core's (3.040 G against 3.031 G), so Crash's pose differs by a few frames.
- **States across builds.** Does a state load across Redux builds? States should be bound to the backend sha256 until proven otherwise.

# Native core (`psx-core`): stage 1, the CPU

## What exists

- **R3000A interpreter.** It implements:
  - load and branch delay slots, including LWL/LWR merging with an in-flight load and the rule that a later write beats a pending load;
  - COP0 exceptions with the KU/IE stack, RFE, and EPC/BD for faults in delay slots;
  - interrupts gated by SR.IEc/IM and Cause.IP2, completing any in-flight load when an exception pre-empts an instruction;
  - cache isolation (stores dropped while SR.IsC is set);
  - overflow, address-error, reserved-instruction and coprocessor-unusable exceptions;
  - COP2 through `psx-gte`.
- **Bus.** RAM with mirrors, scratchpad (not reachable through KSEG1), BIOS ROM, expansion 1 (reads 0xFF), cache control, and an I/O register file
  that logs every access.
- **Devices so far:**
  - Interrupt controller: I_STAT acknowledge semantics, I_MASK, and the IP2 line.
  - DMA controller: starts transfers on CHCR, and implements channel 6 (ordering-table clear) exactly. Other channels are recorded as unmodelled.
- **Reset state.** SR starts at `0x10900000` to match Redux. Most SR bits are undefined on hardware at reset.
- **Not modelled yet:** instruction cache, real timing (one cycle per instruction), the GTE-command vs interrupt quirk, timers,
  GPU, CD-ROM, SPU, MDEC, pads and memory cards.

## Validation: `psx lockstep`

`psx lockstep --bios <bios> --steps N` streams PCSX-Redux's `-trace` output (interpreter + debugger) and,
**before every instruction**, checks:

- the PC;
- the instruction word;
- every source-operand register value that Redux prints.

Two things are injected and counted, never hidden:

1. **Hardware values.** When a register loaded from I/O space is next used, the core adopts Redux's value and writes it into its I/O register file.
   Redux's trace memory column for I/O is a side-effect-free peek, not the value the hardware returned, so the adoption waits until the register is used.
2. **Interrupts.** When Redux enters `0x80000080` or `0xBFC00180` and the core did not, the core takes an external interrupt and holds IP2
   until software acknowledges I_STAT.

`--exact` single-steps Redux over GDB and compares the full register file (pc, r1–r31, hi, lo) after each step.
It runs at about 20 steps/s, so it is for close inspection of a divergence.

## Results (SCPH-7502 BIOS, no disc)

The table records what each run surfaced and the fix it led to.

| Run | Matched instructions | Outcome |
|---|---|---|
| first | 7,814,488 | SPU status read: the trace peek shows `0000` but the register received `0x0800`. Fix: lazy adoption of I/O values |
| second | 24,344,343 | `mfc0 Cause` in the IRQ handler: IP2 was not held after a followed interrupt. Fix: minimal interrupt controller |
| third | 28,953,890 | RAM written by DMA channel 6 (ordering-table clear) was missing. Fix: DMA controller with OTC |
| fourth | **100,000,000**, no divergence | 105,501,945 operand values checked. 681 interrupts followed and 5,787 I/O values adopted. ~530k instructions/s compared |

The only unmodelled DMA in 30M instructions is channel 2 (GPU, RAM→GPU), which writes nothing to RAM.

## GPU wiring: the BIOS boot logo from the native core

The GP0/GP1 ports and DMA channel 2 (block and linked-list) now feed `psx-gpu`. GPUSTAT is plausible but untimed;
in lockstep its reads are adopted from Redux like any other I/O.

After 100M lockstepped instructions (no divergence, `--dump .psx/lockstep/bios100m`), the core's VRAM holds the
**Sony Computer Entertainment logo**. It was drawn entirely by the native CPU, DMA and `psx-gpu` from the real BIOS's commands.
The comparison is against Redux's VRAM at vsync 350, while the same logo is static on screen:

| Region | Pixels differing |
|---|---|
| Texture pages (x ≥ 640) | **0 / 196,608** (uploads are byte-exact) |
| Framebuffer (640×512) | 2,150 / 327,680 (0.66 %) |

Every differing pixel lies inside the **Gouraud-shaded diamond** and is one 5-bit step lower in green, in vertical
stripes. Flat fills, textured text and the logo textures all match exactly. The difference is therefore
colour-interpolation rounding in `psx-gpu`.

The rounding has not been tuned to Redux: Redux's software GPU is another emulator, not hardware truth. The referee should be
a hardware-verified Gouraud test (e.g. JaCzekanski ps1-tests `gpu/`) or DuckStation regtest output.

## Standalone boot (`psx native`)

`psx native --bios <bios> --frames N --dump PREFIX` runs the core on its own, with no oracle. It reports:

- frames reached, speed, and the screen/VRAM;
- **I/O registers read that no device handles yet** (counts by address);
- a profile of the last two frames: hot PCs, I/O traffic, I_STAT/I_MASK/SR.

The profile shows what the software is waiting for.

New devices for this:

- **Timers** (`timers.rs`): sysclock, dotclock, hblank and sysclock/8 sources; target/0xFFFF reset; IRQ modes; reached flags. Sync modes are not modelled yet.
- **VBlank:** PAL 50 Hz / NTSC 59.94 Hz frames on I_STAT bit 0.
- **Timing:** flat per-instruction costs like Redux (2 cycles, +20 in BIOS ROM). Not cycle-accurate.
- **GPUSTAT bit 31:** odd/even line from the current scanline (per field when interlaced), 0 in vblank.
- **SPUSTAT:** mirrors SPUCNT bits 0–5. There is no SPU model yet.

How the profile guided each step of the bring-up (SCPH-7502, no disc):

| Step | What the profile showed | Fix |
|---|---|---|
| 1 | BIOS polled SPUSTAT (~1.8k reads) | SPUSTAT mirrors SPUCNT |
| 2 | Spun on GPUSTAT bit 31 (112k polls/2 frames) | Scanline-derived bit 31 |
| 3 | **The SCE logo is displayed by the native core**, at about 20M instructions/s (1.2–1.7× real time, release build) | none needed |
| 4 | Stays on the logo, polling the CD-ROM status register `0x1F801800` | CD-ROM controller (`cdrom.rs`) |
| 5 | Polls JOY_STAT `0x1F801044` (controller port) | SIO0 with a digital pad in port 1 (`sio0.rs`) |
| 6 | **Empty drive: the BIOS shell main menu (Memory Card / CD Player) runs on the native core** | none needed |
| 7 | **Croc inserted:** BIOS → SYSTEM.CNF → `SLES_005.93`; the loader shows `WARNINGH.IMG` (1,106 native CD sectors) | none needed |
| 8 | Polls the MDEC status register `0x1F801824` (4.2M reads): the FOX logo movie needs the MDEC | MDEC (`mdec.rs`) |
| 9 | **The movie plays on the native core**: the FOX logo FMV at frame ~1300 and Croc's intro FMV at frame 3000 (`CROC.EXE` running) | see the debugging trail below |

| 10 | **Croc is playable natively.** Start presses take the native core through the intro, main menu and island map into level 1-1, and holding Up walks Croc down the path, collecting crystals, to the gate. | none needed |

The level 1-1 frame from the native core matches the one Redux reached earlier (`croc_level1.png`): same geometry, textures, HUD and crystals.
Only moving actors differ, because the timing models differ.

`psx native --replay R.json --dump P --snapshot-every N` drives the pad and saves periodic screens.
`tools/contact_sheet.py` tiles them into one image. One sheet covers the whole boot: Sony logo, licence screen, warning, FOX FMV, Argonaut FMV, title, intro FMV, menu and level.

### Tomb Raider (PAL, German) on the native core

With no device changes needed, the disc boots natively through the logos, warning, Core Design FMV and the intro FMV,
then the passport menu. The "no formatted memory card" notice appears because no cards are inserted.
Two CD-audio gaps then blocked "Spiel Starten". Both were found by comparing Redux's CD command trace (`psx run --trace-cd --replay`) with the native core's.

1. **No CD-DA playback.** The game polls GetlocP every frame while its title music plays.
   CD-DA is now implemented:
   - Play from a track or from Setloc, with the head advancing at 75 sectors/s.
   - Autopause INT4 at the end of a track, and report-mode INT1 every 10 sectors.
   - PCM is queued into the SPU's CD input, honouring SPUCNT bit 0 and the CD volume.
2. **Pregap positions.** After `Setloc 27:51:00` and `SeekP`, Redux saw GetlocP ×10 and then **Play**; the native core polled forever.
   The response bytes (`psx-core` example `iolog`) showed `track 01 index 01`. But 27:51:00 lies in **track 2's pregap**
   (INDEX 00 is at 27:47:11 and INDEX 01 at 27:49:11).
   Positions now attribute pregaps to the following track, with index 0 and the relative time counting down to INDEX 01.
   The game then plays track 2 exactly as it does in Redux.

Result: **Lara in the Caves level, natively**, with the title music (CD-DA) mixed into the captured audio.

### SPU (`spu.rs`) and audio capture

The SPU models:

- 512 KiB sound RAM, with manual and DMA-4 transfers;
- 24 SPU-ADPCM voices, including loop flags and ENDX;
- ADSR envelopes using psx-spx's step/shift formula;
- key on/off, fixed volumes and the IRQ-address check;
- 44.1 kHz stereo mixing.

`psx native --wav PATH` captures the mix.

Not modelled yet: Gaussian interpolation (linear is used), reverb, noise, pitch modulation, volume sweeps, and CD-DA/XA input.
Because XA input is missing, FMV soundtracks are silent.

Observations:

- **BIOS boot (SCPH-7502, no disc):** silence during init, then the boot chime from 3.0 s with a second swell around 5.75 s. It then fades cleanly to silence.
- **Croc, boot to level 1-1:**
  - BIOS chime, then menu sound effects 4 s apart (one per Start press), then the level 1-1 music.
  - **Open:** a constant low voice is held from about 14 to 46 s while the loader shows its screens and movies. It may be legitimate (a voice keyed alongside XA playback, which is not modelled) or an envelope bug; the BIOS-only run does not show it.

### Native backend for `psx run`

`psx run --backend native` runs the same bounded job on psx-core and produces the same report shape as the Redux backend:
RAM, VRAM and screen PNGs, registers, replay, `--symbols`. Its observations come from the emulated hardware rather than from breakpoints:

- **`--watch`:** each CPU store records its PC, width, old value and new value. Each DMA write records its channel and whole range.
- **`--trace-cd`:** writes `cd_native.jsonl`, where every DMA-3 carries the exact LBA and FIFO offset the drive delivered.
  `psx loads` reads it with evidence `native` and needs no content inference.
- **`--trace-bios`:** A0/B0/C0 calls are recorded at the vectors with path arguments. Frequent calls are counted but not recorded.
- **EXE side-loading:** like Redux, the BIOS runs to the shell entry 0x80030000, then the EXE is loaded.
- **Frames** count from power-on, or from the restored state after `--from-state` (Redux counts from SaveStateLoaded).
- **Save states** (`--save-state` / `--from-state`): see below.

Checks:

- **Probe watch:** the native backend reports the same writer PCs as Redux: crt0 BSS clear 0x80010044, and the counter store 0x80010380 with exact old/new values.
- **Croc boot to frame 2400:** 5,129 CD transfers, all `native`, giving the same file→RAM spans and libcd functions as the Redux run.

### Native save states (`state.rs`)

`--save-state` writes `state.bin`; `--from-state` resumes from one. The layout is:

- the `PSXSTATE` magic;
- a JSON header: format version, core version, BIOS SHA-256, media identity, frames and cycles. It can be read without decoding the body (`psx_core::state::read_header`);
- a gzip-compressed postcard encoding of the whole `Machine`: CPU + GTE, RAM, scratchpad, I/O registers, DMA (including a pending MDEC-out transfer), timers, GPU + VRAM + an in-flight VRAM upload, CD drive (queued responses, head position, FIFO, CD-DA buffer), pad port, MDEC, and SPU + sound RAM.

What is not saved, and why:

- **BIOS and disc.** These are reopened from files and checked against the header. A different BIOS or medium is refused with `identity_mismatch`.
  For a side-loaded EXE, the media identity is the file's SHA-256. For a disc, it is a fingerprint: the track layout plus the raw sectors at LBA 0–23 and every 4096th sector, so a 600 MB image isn't hashed on every run.
- **Analysis state.** Watches, traces, I/O and DMA logs, and captured audio belong to the run, not the machine.

Any change to a saved struct's shape must bump `state::FORMAT`; older states are then refused rather than misread.
A native state passed to the Redux backend, or a Redux state passed to the native one, is refused with a hint to switch backend.

Sizes: 0.3 MB (probe), 0.7 MB (Tomb Raider), 1.3 MB (Croc in-game).

Checks: a straight N-frame run must equal a run split at frame K and resumed from the state. The comparison is byte-identical RAM, scratchpad, VRAM, registers, cycles and instruction count, and also the final state file itself, which covers every device.

| Media | Total frames | Split at | Result |
|---|---|---|---|
| probe EXE | 300 | 137 | identical |
| Croc + START replay | 4600 | 3203 (START held) | identical |
| Tomb Raider + CROSS replay | 3000 | 1777 | identical, final full state too |
| Croc + START replay | 4800 | 4111 | identical, final full state too |

`state::tests` repeats the split check on a synthetic BIOS and covers the rejection of a wrong BIOS, garbage and truncated files.

### Instruction trace (`trace.rs`, `psx run --backend native --trace`)

The trace answers what code ran in a window and what it did. It writes three files:

- **`trace.txt`:** one line per instruction: `frame cycle pc word disassembly ; effects`.
  - The cycle is the system clock after the instruction.
  - Effects are the register written with its new value (or HI/LO), plus the data access: `[addr]=value` for a store, or `[addr]=>value` for a load, since the register receives the value after the delay slot.
  - Calls add `-> target Name (from site)` lines, named with `--symbols` and checked against RAM residency.
  - Interrupts and exceptions add `!!` lines.
  - Lines are capped by `--max-events`; coverage and calls continue past the cap.
- **`coverage.txt`:** runs of executed words, with instruction counts, execution counts and the function name at the start. Ready for marking up a Ghidra listing.
- **`calls.json`:** every `jal`/`jalr`/`bltzal`/`bgezal` target with its count, first and last frame, and its top call sites.

The window and filters are:

- `--trace-from-frame F`: start F frames into the run.
- `--trace-start ADDR|Function`: start at the first execution of that PC.
- `--trace-frames N`: stop after N frames.
- `--trace-range LO-HI|Function`: write lines only for those PCs. Repeatable. A function's range ends at the next known symbol. Calls are shown when either the caller or the callee is in range.

Combined with `--from-state`, an agent can jump straight to a moment and trace only the function it cares about.

Example: Croc from the frame-3203 state, 300 frames, took 3.6 s.
- That window ran 101M instructions with 874 interrupts, over 24,713 distinct instructions and 322 call targets.
- The top calls were VSync, CheckCallback and the matrix library.
- The game streams through libcd's low-level calls: CdControlB and CdGetSector, from its own callbacks around 0x80022xxx. It never calls CdRead.

`StepInfo` now carries the instruction's data access (`MemAccess`). The untraced speed is unchanged: 43–44M instructions/s in a side-by-side run against the previous build.
Tracing everything runs at about 28M instructions/s. CPU lockstep still matches over 10M instructions.

### XA-ADPCM audio (`xa.rs`, CD controller)

With Setmode bit 6 set, Form 2 sectors with the audio submode bit go to the XA decoder instead of the data FIFO. With bit 3 set, only the Setfilter file/channel is played. The decoder:

- handles 4- and 8-bit samples, mono and stereo, at 37.8 or 18.9 kHz;
- resamples to 44.1 kHz by linear interpolation (the hardware's zig-zag filter and emphasis are not modelled);
- feeds the same CD input queue as CD-DA.

Both XA and CD-DA now pass through Mute/Demute (commands 0Bh/0Ch), the sound-map ADPCM mute bit, and the CD-to-SPU volume matrix (index 2/3 registers, applied by bit 5). The new fields bumped the save-state format to 2.

`psx xa <image> <path>` lists a stream's XA channels (file, channel, coding, sectors, duration). With `--channel N -o out.wav` it decodes one channel straight from the disc, which is useful on its own for extracting music and voice.

Verification without listening, by comparing the emulator against the offline decode:

- **Croc, FOXLOGO.STR** (boot, frame 1195 onwards): the frames the controller pushes to the SPU are bit-identical to `psx xa`'s decode. 474 of 480 sampled windows matched exactly; the misses were artefacts of the capture method.
  In the final SPU mix the stream correlates at 0.97 with gain 0.505, which is the game's CD volume 0x3FFF/0x8000. The game's own SPU voices make up the rest.
- **Tomb Raider, CORELOGO.FMV** (from 21.16 s emulated): correlation 1.000 with gain 0.386 for the 10 s the movie reads. Nothing else plays.
- **Save states:** a Tomb Raider split run at frame 1380, in the middle of the XA stream, matches the straight run byte for byte, including the final full state.

Croc's earlier "constant drone" in the loader movies is not yet explained. The SPU voices underneath the XA may be it, which is worth a listen to confirm.

### Performance

The baseline was 1.2–1.3× real time, which is comparable to Redux's observed (interpreter + debugger) mode at 1.6×. Each step below was checked:
the Croc boot to frame 3000 must reach the same final PC and 4,192 sectors with byte-identical VRAM, and CPU lockstep must still match.

| Change | Instructions/s | Real time |
|---|---|---|
| baseline | ~18.7M | 1.28× |
| devices advance in 128-cycle batches, always flushed before I/O | ~18.7M | 1.28× (not the bottleneck) |
| buffer the single GPR write instead of copying the register file; RAM/BIOS fast paths | 31.3M | 2.14× |
| incremental triangle stepping (exact quotient/remainder, identical pixels) | 35.0M | 2.39× |
| cached interrupt line, lazy Cause.IP, fat LTO + one codegen unit | **44.2M** | **3.02×** |

A poor man's profiler (gdb stack sampling) guided these changes. The CPU step, execute and bus read dominate.

### Debugging trail: from a kernel overwrite to playing FMVs

This trail shows the toolkit's method on a real bug. Every step used `psx` tools with evidence from both emulators.

1. **Symptom.** `psx native --break` showed execution reaching zeroed memory, with exception vector 0x80000080 = 0.
2. **Who wrote it.** `--watch 0x80000080` caught `sh t1,0(a1)` at `0x8010C374`. The PsyQ symbols name that address `DecDCTvlc+0x268`, which overruns its output buffer past 2 MiB and wraps through the RAM mirror.
3. **Why it overran.** A CD DMA log, compared with Redux's `psx loads` record, showed only 32-byte STR headers transferred and never the data.
   `--break DecDCTvlc` showed it decoding an all-zero ring slot at frame 1225, before any sector had arrived.
4. **Who marked the slot ready.** `--watch` on the ring slot showed `data_ready_callback` (libcd's DMA-3 completion callback) setting status 2 with no DMA.
   The same watch in Redux (`psx run --watch 0x8017ADA0:2`) showed the correct order: DMA, StCdInterrupt=3, data_ready_callback=2.
5. **Root cause.** I/O logs of DICR in both emulators showed stale channel 2/3 flags left by the BIOS CD routine, which writes back 0 to the flag bits.
   Redux raises the DMA master flag when *any* flag is set, so the game's dispatcher acknowledges the stale flags early. psx-spx/DuckStation AND the flags with the enables, so the stale channel-3 flag fired later, when libcd enabled channel 3.
   psx-core now matches Redux. Which rule real hardware follows is an **open question**.
6. **Also fixed on the way.**
   - Flags are only latched with the master enable on.
   - MDEC-out DMA (channel 1) is request-driven, completing only once the decoder has produced the data.
   - CD seek timing follows Redux's distance/speed-change model.

New `psx native` tools from this trail:

- `--break PC --tail N`: stop at a PC with recent history, DMA history and the vector contents.
- `--watch ADDR --watch-after FRAME`: every change of a word, with the instruction or DMA responsible.
- `psx disasm`: symbol-annotated disassembly of an EXE.

The examples `cdlog` and `iolog` log CD transfers and commands, and chosen I/O registers.

CPU lockstep is unaffected: 30M instructions still match after these changes, because lockstep keeps adopting I/O values and following Redux's interrupts.

## Next

- **GPU wiring:** send GP0/GP1 port writes and DMA channel 2 (block and linked-list) into `psx-gpu`, then compare the BIOS screen with Redux's.
- **CD-ROM controller.** Commands, response FIFOs and INT1/2/3/5. Behaviour can be checked against Redux's CD traces and our load-provenance records.
- **Game code:** lockstep from a side-loaded EXE (the probe), then from boot of Croc.

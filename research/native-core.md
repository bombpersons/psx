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

Every differing pixel lay inside the **Gouraud-shaded diamond** and was one 5-bit step lower in green, in vertical
stripes. Flat fills, textured text and the logo textures all matched exactly. **Resolved** (see "GPU accuracy"
below): the old rasteriser floored exact barycentric colours, while the GPU walks 16.16 fixed point and rounds
each span start to nearest. After the fix, Redux's and the native core's VRAM at vsync 350 of a standalone boot
(`psx run --bios ... --frames 350`, both backends) are **identical over all 1024×512 pixels** (2,150 → 0).

## GPU accuracy: rasterisation rules, a probe EXE and silicon values

**Reference.** The pinned PCSX-Redux tree carries MIT-licensed GPU hardware test suites
(`src/mips/tests/gpu-raster-phase1..23`). Their headers record pixel values read back from SCPH-1000/5501/7001
consoles (`HW_VERIFIED`), and Redux's software GPU (`src/gpu/soft`, GPL) was tuned to them. `psx-gpu`'s rules were
re-derived from those values and from the arithmetic Redux documents; no Redux code was copied.
`crates/psx-gpu/tests/hardware.rs` replays the suites' primitives and checks 104 silicon pixels plus the phase-23 row-coverage scans:

| Suite | What it pins | Old `psx-gpu` | Now |
|---|---|---|---|
| phase-22 SG_AB (24 px) | slanted gouraud colour | 13 wrong (1 LSB) | all match |
| phase-22 SG1/SG2/SG_R/SG3 (24 px) | slanted gouraud | match | match |
| phase-17 T1-T8, Q1-Q5 (53 px) | affine UV, quads | T2 wrong (2 px) | all match |
| phase-23 Q2/Q3 | quad size check: v1-v2 diagonal yes, v0-v3 no | Q3 dropped | match |
| phase-23 O1-O3 | vertex + offset wraps to 11 bits after the add | O3 off-screen | match |
| phase-23 R1-R3, phase-10 LG | rect width `& 3FFh`; gouraud line midpoint | match | match |

**Triangle rules now in `raster::triangle`.** Vertices are sorted by y. Rows run from the top vertex down to,
not including, the bottom one. Each row covers pixels `ceil(left) ..= ceil(right) - 1`. Edge X and edge colours
are evaluated per row in 16.16, with one truncating division. Texture coordinates instead advance by a per-row
step that is truncated once. There is one horizontal gradient per attribute, taken from the span through the
middle vertex: its width is clamped to at least 1/16 pixel and the quotient is truncated toward zero. Each span
starts from the left-edge value moved to the first pixel, **plus one half** (round to nearest), then adds the
gradient per pixel. That half is the BIOS-logo green step.

**Probe EXE.** `tools/probes/gpu` (PSYQo) sends a fixed GP0 stream: 468 commands from `gen_stream.py`, the same
words in `gp0_stream.bin`. It draws 24 test cells of 128×128 (`gen_stream.py --list`), covering gouraud and
dither, textures in all depths, the four blend modes, lines and polylines, rects, texture windows, mask bits,
clipping, slivers and VRAM copies. `tools/probes/gpu/compare.sh` runs it on Redux and diffs Redux's VRAM against
`psx gpu-replay gp0_stream.bin` using `psx vram-diff`, with per-cell counts, a channel-delta histogram and a
diff PNG. The native core running the EXE (`psx run --backend native`) gives VRAM identical to the replay.

| Differing pixels vs Redux (393,216 compared) | Before | After |
|---|---|---|
| total | 69,023 | 5,251 |
| gouraud tris/quads, dithered or not, slivers, semi B/2+F/2, B+F, B-F | 9,024 | 0 |
| flat-modulated textured, 15-bit raw textured | 14,132 | 0 |
| textured gouraud, dithered | 9,982 | 0 |
| large primitives clipped on all sides | 13,178 | 597 |
| flat lines | 206 | 4 |

The 5,251 pixels that remain are all deliberate, and each was traced to a Redux shortcut that `psx-gpu` does not
copy:

- **Textured gouraud fast path (cells 7, 19, 22: 2,404 px).** When it is neither dithered, semi-transparent nor
  mask-checked, Redux modulates pixels in pairs with the even pixel's colour: its packed writer takes the second
  colour through an `int16_t`, which drops it. Emulating that removes every one of these pixels, as checked in
  an experiment. The pairing is an emulator artifact, so it is not copied.
- **Gouraud lines drawn right to left (cells 15, 16: about 840 px).** Redux swaps the endpoints and keeps
  colour 0 at the new start, so the gradient runs backwards. `psx-gpu` keeps each colour on its own vertex. The
  Bresenham positions now match exactly.
- **Line clipping (4 px).** Redux's diagonal-line path treats the right and bottom edges of the drawing area as
  exclusive, while its axis-aligned path treats them as inclusive. `psx-gpu` uses inclusive edges throughout.
- **Semi-transparency with dithering (cell 12: 1,355 px).** Redux blends at 8 bits before dithering.
  `psx-gpu` dithers the front colour and then blends at 5 bits, which is the Mednafen/DuckStation order. No
  silicon value decides between them yet.
- **Modulated semi-transparent texels (cell 13: 510 px).** Redux blends B/2+F/2 and B+F/4 with an unsaturated
  modulated F (up to 62). `psx-gpu` saturates F to 31 first. This is also undecided on silicon (phase-12 does
  not probe F > 31).
- **Overlapping VRAM copy (cell 23: 138 px).** Redux copies forward in place, so a copy shifted right smears
  the first pixels. `psx-gpu` reads each row before writing it.
- **Quad halves.** Redux draws (v1,v3,v2) before (v0,v1,v2). `psx-gpu` keeps the psx-spx/Mednafen order (v0,v1,v2)
  then (v1,v2,v3). This only matters for self-intersecting quads, so the probe uses convex quads.

**Other audit changes (psx-spx).** Lines use the documented Bresenham with endpoints included, and mono lines
are dithered too ("LINEs are dithered (no matter if they are mono or do use gouraud shading)"; Redux does not
dither lines). The size check uses pre-offset coordinates and the triangle edges. These were already correct
and now have tests in `tests/rules.rs`: rect X/Y flip, texture windows on rects, check/set mask on rects and
lines, fill ignoring mask and drawing area and wrapping, row-buffered overlapping copy.

**Open.** These have no silicon value yet: dither versus blend order, saturation of F before blending, the draw
order of overlapping quad halves, overlapping copies wider than 128 pixels (Mednafen buffers in chunks of 128),
gouraud line colour direction, and the drawing-area-equals-one-column case. "Draw to displayed field" in
480i (E1 bit 10) is not modelled.

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

At the time, Gaussian interpolation (linear was used), reverb, noise, pitch modulation, volume sweeps, and CD-DA/XA input were missing, so FMV soundtracks were silent.
All of these have since been added; see "SPU completion" below for the current state.

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

`psx str` does the same for whole movies: it reads the BS bitstream into MDEC run-length data in software and decodes it on this MDEC model. Croc's FOX logo and Tomb Raider's Core logo match the emulator's screen bit for bit; see [str-movies.md](str-movies.md).

Verification without listening, by comparing the emulator against the offline decode:

- **Croc, FOXLOGO.STR** (boot, frame 1195 onwards): the frames the controller pushes to the SPU are bit-identical to `psx xa`'s decode. 474 of 480 sampled windows matched exactly; the misses were artefacts of the capture method.
  In the final SPU mix the stream correlates at 0.97 with gain 0.505, which is the game's CD volume 0x3FFF/0x8000. The game's own SPU voices make up the rest.
- **Tomb Raider, CORELOGO.FMV** (from 21.16 s emulated): correlation 1.000 with gain 0.386 for the 10 s the movie reads. Nothing else plays.
- **Save states:** a Tomb Raider split run at frame 1380, in the middle of the XA stream, matches the straight run byte for byte, including the final full state.

**The drone and the lingering boot jingle** (heard in Croc's loader movies and under the Fox intro) were one SPU bug.
Exponential envelope decreases computed `step × level / 0x8000`. Integer division rounds toward zero, so the step became 0 once the level dropped below 0x1000.
Every voice released that way (all 24 of the BIOS boot jingle's) then held at level 4095, about 12% volume, forever.
The hardware shifts instead (`>> 15`, rounding down). With that fix the jingle fades to silence by about 19 s, and the Fox intro's mix is the XA stream alone: correlation 1.0000, gain 0.500.
The new `psx native` report field `spu.active_voices` showed the stuck voices: phase, level, volumes, start address and ADSR.

### Memory cards (`memcard.rs`, SIO0)

Either slot can hold a card. The protocol supports Read (52h), Write (57h) and Get ID (53h), the FLAG byte's "new card" bit (cleared by the first write), checksums (a bad checksum gives end status 4Eh), and invalid sectors.

SIO0 now routes each transfer by its address byte: 01h goes to the pad (port 1), 81h to a card in the selected slot. Empty slots still read FFh with no /ACK.

- **CLI:** `psx run --backend native --memcard1 PATH|new --memcard2 ...`.
  - The image file is never modified. A card the run wrote to is saved as `memcardN.mcd` in the run directory.
  - The report lists sectors written per slot.
  - Card contents are part of the save state (format 3), so `--memcard*` is refused with `--from-state`.
- **`psx memcard <image>`:** lists the saves (product code and name, blocks, size, Shift-JIS title, icon frames) and the free blocks.
- **`Sio0::log`:** a diagnostic byte log `(time, JOY_CTRL, tx, rx, ack)`.

Checks against the SCPH-7502 BIOS's own card driver, in the shell's MEMORY CARD screen:

- A hand-built save (directory entry, "SC" title frame, palette and 16×16 icon) shows its icon and its title "PSX TOOLKIT TEST".
- COPY from CARD1 to an empty CARD2, then confirm: 67 sectors written, and the save's directory entry and block are byte-identical to the source.
- A split run across the copy (1010 + 490 frames against a straight 1500) gives an identical state, card image, RAM and VRAM.
- The shell checks card presence every frame (`81 52`, reads the flag and 5Ah, then deselects). Directory reads end with 47h ("good").
- The shell ignores the pad for a few seconds while it reads both cards. Input at frame 250 is dropped and input at frame 600 works, with or without saves.
- Tomb Raider, with a new card, runs the libcard write test (frame 63, rewritten with its own contents) and then clears the flag.

### SPU volume sweep and current-volume registers

Volume registers with bit 15 set now sweep. This covers voice L/R and main L/R. Bit 14 selects exponential, bit 13 decrease, and bits 0–6 the rate.
Sweeps step every sample through the same rate engine as ADSR (`env_step`), whether the voice is sounding or not. Sweep phase (bit 12, negative output) came later, with "SPU completion".
Before this, a sweeping volume played at full level.

The current-volume registers are now readable: main at `1F801DB8/BA`, voices at `1F801E00+4n`. They report the live level, as DuckStation and Mednafen do. Before, they read back 0, because nothing ever writes them.
This changed Croc's flow at frame 818. Its movie player reads `1F801DB8` and only forces the main volume to 3FFFh/3FFFh when that reads 0 (`0x80108D9C`).
With the live level 0x7FFE it leaves the right channel at the 0x37EF the game set at frame 156, so the Fox intro is slightly quieter on the right. This is now the hardware behaviour.
Save-state format 4.

### DualShock (`pad.rs`)

`--pad dualshock` (native `run` and `psx native`; also the Redux backend, as its analog pad) plugs in a DualShock.
It starts digital (ID 41h) and supports the config commands psx-spx documents: 43h enter/exit, 44h set mode and lock, 45h, 46h, 47h, 48h, 4Ch and 4Dh (rumble mapping). In analog mode it replies with ID 73h and the four stick bytes.
Replay events take `"sticks": [lx, ly, rx, ry]` (128 = centred). Replay parsing is shared by `psx run` and `psx native` (`crates/psx/src/replay.rs`).
The run report shows the pad's final mode, lock and motor values. Pad state is part of the save state (format 5).

Two mistakes in the first version were found by reading psx-spx and Redux's `pad.cc`:
- 45h returned type 03h, which is a DualShock 2. A PSX DualShock is 01h.
- 43h *in config mode* returned button data. It replies with zeros.
The replies now match Redux's analog pad byte for byte.

Ape Escape (PAL, SCES-02029) requires a DualShock.
- With a digital pad the game's pad status reads FFh and it ignores all input. Its attract demo still runs, which can look like progress.
- With a DualShock it probes the pad: 43h, 45h, 4Ch ×2, 47h, 46h ×2, 47h. It then sets analog mode and lock (`44 01 03`), maps the motors (`4D 00 01`), and polls with 42h.
- The game reads buttons every other frame. DOWN arrives as 4000h (PADLdown) in its button word at `0x800E5498`, and presses reset the title's idle timer.
- The title menu then ignored every button. That was **LibCrypt**, not the pad: see below. Redux behaved identically only because it doesn't use SBI data either, so agreement with the oracle was not evidence here.

### LibCrypt (`psx_media::subq`, CD-ROM GetlocP)

PAL LibCrypt discs have about 32 sectors whose subchannel Q was deliberately corrupted at mastering. BIN/CUE rips don't carry subchannel data, so the corruption is distributed separately by redump.org as `.sbi` (Q without CRC) or `.lsd` (Q with CRC) files.
- **Loading:** `Disc::open` picks up `<image>.sbi`/`.lsd` beside the image.
- **Reporting:** `psx disc` reports `subchannel.overridden_sectors`.
- **Identity:** the overrides are part of the disc fingerprint, so a save state refuses a disc with or without the wrong `.sbi`.

**Drive behaviour.** The drive checks each sector's Q CRC and keeps its last *valid* Q when the check fails. On a LibCrypt sector, GetlocP therefore repeats the previous sector's position; the game reads its key bits from *which* sectors do this, not from the corrupted values.
- `.sbi` entries are bad by definition.
- `.lsd` CRCs are checked (CRC-16/CCITT, inverted).
- My first version returned the corrupted Q itself, and the game still refused the disc.

**Ape Escape (Germany, SCES-02029)** is redump [disc 26604](http://redump.org/disc/26604/): CRC-32 b5660f4d matches our image, and "LibCrypt: Yes".
- **The probe:** from frame ~1157 the game issues Setloc to 16 MSF positions between 03:08 and 03:34 (LBA ~14000-16000), each followed by ReadS and GetlocP ×9.
- **Without the `.sbi`:** the game boots and plays its attract demo, but its title menu ignores all input.
- **With the redump `.sbi`** (32 sectors, first at LBA 14335): START on the title starts a new game. Spike walks with the left stick, and walking into a trigger opens the professor's tutorial text.

### More discs (Castlevania SOTN, Mega Man 8, Crash Bandicoot; all PAL)

With no emulator changes, all three boot on the native core (SCPH-7502) and reach gameplay with START/CROSS replays within 8000 frames. None issue unknown CD commands or unmodelled DMA.

| Game | Reached | XA sectors | Speed |
|---|---|---|---|
| Castlevania: SOTN (SLES-00524) | Prologue: Richter on the castle stairs | 1733 (in-game music) | 1.42× |
| Mega Man 8 (SLES-01064) | Intro stage, weapon menu | 401 (+ FMV) | 1.48× |
| Crash Bandicoot (SCES-00344) | N. Sanity Beach | 0 | 1.20× |

These games are heavier than Croc (3.0×), which makes them good profiling targets.

### GPU frame capture (`psx_gpu::capture`, `--gpu-frame N`)

`psx run --backend native --gpu-frame N` logs every GP0 command of frame N of the run. `capture::decode` then replays them against a tracked draw state (E1h–E6h), so each primitive gets:
- absolute VRAM vertices (drawing offset applied), UVs and colours;
- its texture page (position, depth, blend) and CLUT position;
- gouraud, raw, semi-transparency and texturing flags;
- rect, fill, copy and upload areas.

Outputs:
- **`gpu_frame.json`:** a summary (counts by kind, texture pages and CLUTs with use counts, draw state at frame start) followed by the full command list.
- **`gpu_frame_before.png` / `gpu_frame_after.png`:** VRAM at the frame start and end.
- **`gpu_frame_wire.png`:** the after image with every primitive outlined. Polygons are green, rects yellow, lines cyan, fills red, and uploads/copies magenta.

Examples:
- **Crash:** 1,939 commands, all `poly3` (the engine emits only triangles). 1,577 are textured, 305 semi-transparent, over 62 CLUTs. It draws into the right buffer (area x 512–1023, offset 768,128).
- **SOTN:** 416 commands. The background is a grid of 189 16×16 textured rects, and characters, HUD and effects are 97 quads. There are 49 texpage/texwindow switches, all 4-bit CLUT pages at x 512–1023, with side-by-side double buffering at x 0 and 256.

`psx textures <run_dir>` follows a capture. It decodes every texture page/CLUT pair the frame sampled from the run's VRAM into PNGs, with 0000h transparent:
- `page_X_Y_Dbpp_clut_CX_CY.png` is the whole 256×256 page.
- `..._used.png` is cropped to the UV bounds the frame actually sampled.

`--page X,Y,DEPTH --clut X,Y` decodes a single page by hand. On SOTN's prologue this gives 22 pairs, including the castle tileset (bricks, railings, carpet) and Richter's sheet. A full page decoded through one palette shows tiles that use other CLUTs in the wrong colours, so the cropped images are the ones to use.

### SPU reverb

The reverb follows psx-spx's "SPU Reverb Formula":
- **Input:** voices with their EON bit set (`1F801D98/9A`), plus CD audio when SPUCNT bit 2 is set, scaled by vLIN/vRIN.
- **Processing, at 22.05 kHz:**
  - same-side and cross-side reflections through the IIR (vIIR, vWALL);
  - four comb taps;
  - two all-pass filters (dAPF/vAPF).
- **Buffer:** a ring in sound RAM from mBASE to the end. It is written only with SPUCNT bit 7 (master) set. Writing mBASE resets the buffer address.
- **Output:** scaled by vLOUT/vROUT and mixed before the main volume.
- **Simplification (since replaced, see "SPU completion"):** the hardware's 39-tap FIR resampler was replaced by averaging each input pair and holding the output for two samples.

Save-state format 6.

**Checks:**
- A unit test routes an impulse through a configured delay line. It must come back after exactly mLSAME + dAPF1 + dAPF2 steps (72), and nothing may be written with the master enable off.
- The boot chime: the BIOS sends its startup sound through heavy reverb. RMS rises from 3949 to 4672 over the first 10 s, and that is where the "before/after" difference is largest.
- Croc in-game routes all 24 voices with output volume 1024, a subtle room. SOTN and Mega Man 8 enable the master at the title; Tomb Raider and Crash leave it off there.
- A split run during the reverberant boot chime (600 frames, split at 301) gives an identical final state.

The `psx native` report's `spu.reverb` shows the master/CD enables, the EON mask, the work area and the output volume.

### 3D capture: lifting triangles back to view space

During a `--gpu-frame` capture, the GTE logs every RTPS/RTPT vertex: its SXY result and its view-space position (`TR + RT·V`). Logging starts the frame before, because packets are often built a frame ahead.
- **Matching:** each polygon vertex's screen position (minus the drawing offset) is matched to SXY results. Among the candidates, the set closest together in the log is taken. A polygon's vertices are projected together, so a spread over 64 entries is treated as a coincidental match and the polygon isn't lifted.
- **Outputs:** lifted vertices carry `view` in `gpu_frame.json`, and the summary reports H/OFX/OFY and the match counts.
- **Scene:** `psx textures` exports lifted polygons as `mesh3d` elements with the projection.

Crash, N. Sanity Beach:
- 9,534 projections logged; 1,860 of 1,939 polygons lifted; 5 rejected for spread.
- Before the spread rule, 5,594 of 5,595 lifted vertices re-projected within 1 px of where the game drew them.
- H = 288, and the screen centre comes from the GPU drawing offset (GTE OFX/OFY = 0).
- The template rebuilds the frame as Mesh3d geometry, recognisably the same view.

Known difference: the PS1 draws in painter's order without depth. Under Bevy's depth test, the sky backdrop panels that Crash draws first (commands 8–16, z ≈ 900–1500) appear in front of the far jungle that later polygons paint over them. The template's `PsxSceneDepth::Painter` mode (viewer `--painter`) handles this. It draws lifted elements in capture order with no depth test, as transparent-pass items sorted by a per-material depth bias of element index × 8192. A synthetic scene of 12 overlapping bands, each later band placed farther away, renders in capture order in painter mode and as the nearest band under depth testing. Against the emulator's frame, the mean absolute difference drops from 26.5 (depth-tested) to 10.4 (painter) per channel on 0–255.

The bright green and yellow wedges at the screen edges were a different bug. Commands 16, 1536 and 1581 cross the viewport edge, and with `@interpolate(linear)` Mesa llvmpipe extrapolated their UVs and Gouraud colours (sky sampled bright green, sand at (255,255,99)). The shader now sends each affine attribute as A·w with perspective-correct interpolation and divides by the interpolated w. That is exactly screen-linear and does not depend on how the driver clips. Remaining difference in painter mode: some foliage that the game draws opaque appears faint. The likely cause, not yet verified, is that a semi-transparent polygon is blended as a whole, while on the PS1 only texels with the STP bit set blend.

Confirmed and fixed (2026-10-15): `psx textures` now writes PNG alpha 128 for texels with the STP bit (0 = 0000h, 255 = opaque), scenes are format 2, and the template blends only STP texels of semi-transparent elements (`PsxStp::Texel`; `Subtract` draws as an STP pass plus an opaque-texel pass). Crash painter mode against the emulator frame: mean absolute difference 10.4 → 9.0, pixels off by more than 48 from 3.6% → 2.0%; the foliage is now solid. The largest remaining region is the subtractive shadow under Crash, which comes out weaker: with a 3D camera, Bevy composites in linear light, while the GPU subtracts gamma-encoded 5-bit values. Captures now also record `dither` per polygon (E1h bit 9 and shading/modulation), which the scene export passes through; the SOTN capture predates it and dithering is assumed there, although SOTN's frame shows none (with `--no-dither` the SOTN diff drops from 3.7 to 3.0 inside the draw area).

Template fix found on the way: `PsxMaterial`'s shader imported Bevy's `fog` binding unconditionally. That binding only exists for cameras with `DistanceFog`, so any fog-less 3D camera failed to compile the shader. Fog is now gated with `@if(DISTANCE_FOG)`.

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

A poor man's profiler (gdb stack sampling) guided these changes.

Later, on Crash Bandicoot (the most polygon-heavy disc), sampling put about half the time in the CPU interpreter and about a third in GPU texture sampling.
The triangle rasteriser now walks only each row's covered span. Each edge function is linear in x, so the span bounds are solved per row and the attribute steppers jump to the span start in one step.
Results:
- Crash: 27.2 → 31.2M instructions/s.
- Croc: +5%.
- VRAM and RAM are byte-identical on Crash, Croc, Tomb Raider, SOTN and Mega Man 8.

A fixed-size VRAM array, to drop bounds checks, measured as noise and was not kept. The CPU step, execute and bus read dominate.

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

### SPU completion: noise, pitch modulation, Gaussian interpolation, reverb FIR, envelope edge cases

Everything below follows psx-spx's SPU chapter. Where psx-spx is silent, the choice is named with its source.

- **Noise** (`NON`, `1F801D94/96`): one shared generator, clocked every sample from SPUCNT bits 8–13 (step 4–7, shift 0–15). Each sample the timer drops by the step; on underflow the level shifts left with parity `bit15^bit12^bit11^bit10^1` and the timer reloads by `20000h >> shift` (twice if still negative). A noise voice replaces its interpolated ADPCM sample with this level, then goes through ADSR and volume as usual. It still walks its ADPCM stream for IRQs and ENDX. End-without-repeat flags don't mute a noise voice; that detail is DuckStation's behaviour, not psx-spx's.
- **Pitch modulation** (`PMON`, `1F801D90/92`; bit 0 ignored): voice N's step is `((s16)pitch × (out[N-1] + 8000h)) >> 15`, masked to 16 bits and then clipped to 4000h. `out[N-1]` is the previous voice's sample after its envelope, from the same tick.
- **Gaussian interpolation:** the 512-entry table is transcribed from psx-spx. A test checks the table against psx-spx's own claim that every four-tap sum lies in 7F7Fh–7F81h. The index is pitch-counter bits 4–11. The taps are the newest sample and the three before it, with the previous block's last three kept across block boundaries.
- **Reverb FIR:** the reverb now computes left on one 44.1 kHz sample and right on the next. Each side's input is decimated through the 39-tap filter (`>> 15`, DC gain 7FFEh/8000h). Each side's output is zero-stuffed back to 44.1 kHz and filtered again (`>> 14`, since half the slots are zero), then scaled by vLOUT/vROUT. This replaces the old pair-average-and-hold.
- **One envelope engine** for ADSR and sweeps, using psx-spx's counter form (`Counter += 8000h >> max(0, shift-11)`, step when bit 15 is set):
  - sweep phase (bit 12): an inverted increase heads to -8000h, an inverted decrease clamps at 0, and exponential decrease ignores the phase;
  - exponential increase above 6000h quarters the step (shift < 10), halves step and rate (shift 10), or quarters the rate (shift ≥ 11);
  - all-ones rates never step. That means 7Fh for attack, sustain and sweep, and release shift 1Fh. Other slow rates clamp the increment to 1;
  - exponential decrease scales the step by `level >> 15`.
- **Voice edge cases:**
  - key off releases from any phase, including attack;
  - ADSR register writes apply on the next envelope tick;
  - key on clears ENDX and resets the ADPCM history and interpolation history;
  - an end-without-repeat block (code 1) jumps to the repeat address, sets ENDX, and mutes the voice (envelope 0);
  - all voices keep walking after they go silent ("all voices are permanently reading data from SPU RAM"), so silent voices still set ENDX and trap the IRQ address;
  - a CPU write to the repeat address makes loop-start flags stop overwriting it until the next key on. This is Redux's and Mednafen's `IgnoreLoop`; psx-spx only says writes are useful to redirect one-shots.
- **IRQ address:** checked on voice block fetches, FIFO/DMA writes **and DMA reads**, capture-buffer writes, and reverb RAM accesses. It is only armed with SPUCNT bits 15 and 6 both set.
  - psx-spx doesn't know which reverb accesses trap. Here all of them do, but only while the reverb master enable is on. A game that never set up reverb (mBASE 0, so the ring sweeps all of sound RAM) would otherwise take stray interrupts.
- **Capture buffers:** CD left/right (before volume) and voices 1 and 3 (after the envelope) are written to 000h/400h/800h/C00h at one halfword per sample. SPUSTAT bit 11 shows the half being written. Capture IRQs need RAM_CTRL (`1F801DAC`) bits 2–3, as psx-spx says.
- **Mixing:** products use `>> 15` instead of `/ 8000h`. SPUCNT enable/unmute silence the voices only; psx-spx says they are "don't care" for CD audio. The main volume applies after the mix is clamped.
- **Registers:** offsets 260h–3FFh no longer alias the voice registers in the plain register store.
- **Report:** the `psx native` report gains `spu.noise_voices_seen`, `spu.pmon_voices_seen` (every voice ever given the feature) and `spu.irqs`.
- **Save-state format 7.**

**What the games use** (iolog of `1F801D90–9A`/`1F801DA4` over frames 0–5000, plus the new report fields over 7000–8000 frames):

| Game | NON | PMON | EON | SPU IRQs |
|---|---|---|---|---|
| Croc | never | never | all 24 voices in-level | 0 |
| Tomb Raider | never | never | 0 | 0 |
| Symphony of the Night | never | never | 0 | 0 |
| Mega Man 8 | never | never | 0 | 0 |
| Crash | never | never | voices 16–17 | 0 |
| Ape Escape | never | never | voices 2, 3, 5–7, 11, 13, 18, 19, 22, 23 (`CC28ECh`) | 2 |

- None of these windows writes NON or PMON, so noise and pitch modulation are verified only by unit tests.
- Ape Escape sets IRQA = 0 with IRQ9 enabled and takes 2 interrupts. Its run is unchanged by them: same instruction count at frame 7000, same final PC, same VRAM. Spyro's image isn't a whole number of sectors and wasn't tested.

**Checks:**
- **Unit tests (18 SPU tests):**
  - the Gaussian table: sample entries, the 4-tap sums, monotonicity;
  - interpolation of a constant and across block history;
  - the noise LFSR, bit by bit, against a model, plus its rates;
  - a noise voice through the mixer;
  - PMON step maths and a modulated voice in the mixer;
  - FIR tap sums, left/right alternation, the DC level through both FIRs and the impulse spread;
  - envelope rates: the counter form, the exponential >6000h cases, exponential decrease, frozen rates;
  - sweep phase, including Wipeout 3's frozen inverted sweep;
  - ADSR phase transitions and mid-envelope writes;
  - key off during attack;
  - one-shot end jumping to a redirected repeat address and trapping an IRQ while silent;
  - `IgnoreLoop`;
  - IRQ on DMA read, capture-buffer IRQ and SPUSTAT bit 11;
  - IRQ on reverb access.
- **XA path:** Croc's FOXLOGO stream in the mix correlates 0.9899 at gain 0.467, the same as before the change (0.9899, 0.466). The gain is below 0.5 because of the right main volume 37EFh (see "current-volume registers"). Windows that are pure CD audio (Tomb Raider, SOTN, Mega Man 8 after their intros) correlate 1.000 before/after.
- **Sound changes:**
  - The boot chime keeps its RMS, but its spectral centroid drops from about 1.6–3.3 kHz to 0.8–2.0 kHz. The Gaussian filter rolls off the top of each sample's band where linear interpolation passed images.
  - The reverberant tail (12–18 s) is about 10% louder with the FIR.
  - Croc in-level and Crash's title (SPU music) correlate 0.87–0.99 with the old output.
- **Save states:** split runs match the straight run byte for byte, including the final state: BIOS 600/301, Croc 4800/4111 (START replay), Tomb Raider 3000/1380 (inside the XA stream).
- **Other:**
  - CPU lockstep matched over 2M steps;
  - speed is unchanged within noise (Croc 3000 frames with WAV capture, alternating runs: 35.0–39.0M instructions/s before, 36.2–37.1M after);
  - Croc's frame-5600 VRAM is identical before and after.

**Open:**
- psx-spx doesn't settle whether the ADPCM filter's `(old*f0 + older*f1 + 32) / 64` rounds toward zero (as written; kept) or is an arithmetic shift.
- Key on/off are applied at the write, in write order. The hardware latches them per sample, and its key-on startup delay isn't modelled.
- Which reverb accesses trap the IRQ is a guess (see above).
- Redux's "IRQ on the loop address when a block has the end flag" special case isn't modelled.

## Next

- **GPU wiring:** send GP0/GP1 port writes and DMA channel 2 (block and linked-list) into `psx-gpu`, then compare the BIOS screen with Redux's.
- **CD-ROM controller.** Commands, response FIFOs and INT1/2/3/5. Behaviour can be checked against Redux's CD traces and our load-provenance records.
- **Game code:** lockstep from a side-loaded EXE (the probe), then from boot of Croc.

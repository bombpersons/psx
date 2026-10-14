# psx toolkit plan (draft, 2026-10-14)

Response to `MDX_AND_PS1_RESEARCH.md`. Findings come from a source read of the
PCSX-Redux and Beetle PSX HEADs and the repository metadata of other candidates,
done on 2026-10-14. Nothing below has been built or run yet; items marked
**(probe)** are hypotheses the first experiments must confirm.

## 1. Recommendation in one paragraph

Build `psx`: a Rust CLI and MCP tool that reuses mdx's envelope, artifact, context
and MCP layers. It drives **PCSX-Redux as an out-of-process worker**: one Redux
process per run, launched headless with the interpreter and debugger enabled, and
controlled through a Lua bridge script that we write. Static disc and EXE
inspection is pure Rust and needs no emulator. Use an **unmodified
DuckStation** (`duckstation-regtest`) and/or Mednafen only as independent
*oracles* for frozen screenshot and RAM gates, mirroring how mdx uses Genesis Plus GX.
Defer a home-grown Rust core (an "mdcore for PS1") until the Redux adapter shows
which observations it cannot provide.

## 2. Backend findings that drive the choice

| Candidate | License | Observability | Embedding / isolation | Role |
|---|---|---|---|---|
| **PCSX-Redux** @24f7104 | GPL-2.0 root. `src/mips` (nugget/OpenBIOS/PSYQo) is MIT | Lua `addBreakpoint(addr, Exec/Read/Write, width)`, `getRegisters`, `getMemPtr`, `getVRAM`, save-state create/load from slice, GPU logger with VRAM-rect breakpoints, web API (RAM/VRAM raw, state, execution-flow, cd), GDB stub | Global singletons (`g_emulator`, `_PC_` macros), so not a library and no multi-instance. Separate processes are fine. `-no-ui` stub UI; their CI still uses xvfb | **Primary backend** |
| Mednafen / Beetle PSX | GPL-2.0(+) | Mednafen has an interactive debugger with no remote API. The libretro API only gives SYSTEM_RAM and serialize, with no hooks | Heavy globals, one instance per loaded `.so` | Possible oracle (Mednafen binary). Not the primary backend |
| DuckStation | CC BY-NC-ND 4.0 | GDB stub, headless `duckstation-regtest` | Modified versions may not be published | **Unmodified oracle only**. Never patch or bundle it |
| StarPSX (Rust) | GPL-3.0 | Separate `core` crate. DMA/CDROM/SPU are partial | Embeddable | Watch. A fork candidate if we go in-process later |
| Trapezoid (Rust) | MIT | CLI debugger | Dormant since 2025-03 | Reference reading only |

Consequences:

- **Licensing:** Out-of-process control keeps the `psx` crate under mdx's MIT/Apache
  terms. The bridge Lua script and any private Redux patches are GPL-2 and
  live in their own directory with their own notice.
- **Isolation:** R5 and R7 are answered by design: each run is a process. Concurrency equals
  the number of worker processes, capped by a semaphore, as in mdx's `mcp.rs`.
- **Instruction trace:** A Lua Exec breakpoint per instruction will be too slow
  **(probe)**. We expect to need a small private Redux patch: a ring-buffer trace hook in
  `psxinterpreter.cc` and DMA-writer attribution. That is acceptable under the GPL
  as long as patched builds are identified by hash, as mdx already does for its observer builds.

## 3. What we reuse from mdx

From the mdx survey:

- **Platform-neutral already:** `envelope.rs`, `artifacts.rs`, `context.rs`,
  `mcp.rs`. They have no mdcore references; only the `mdx.` / `.mdx/` names and the
  instructions text change.
- **Generic but hard-coded:** `operations.rs` uses string `match` arms plus
  per-op path-field and artifact-field tables. Replace these with an
  `Operation` trait (`Args`, `Data`, `PATH_FIELDS`, `ARTIFACT_FIELDS`, `run`) and a
  registry.
- **Strategy:** **copy** those four files into `psx/crates/opkit` first and
  refactor them there. Once psx stabilises, move mdx onto the shared crate in a
  separate, reviewable change. mdx is not touched until then.
- **Keep:** plain-string optional path schemas (`portable_schema`), the 1 MiB arg
  cap, the 64 KiB response-to-artifact spill, per-call `project_root`, versioned
  state formats, and the `jsonschema` contract test plus the stdio MCP test.
- **Change:**
  - The 64 MiB artifact cap stays for captures. Disc images become external
    **media references** (path + streamed SHA-256 + size) instead of artifacts.
  - Save states are Redux's opaque blobs wrapped in our own envelope. That envelope binds the Redux
    build hash, settings, BIOS hash, media manifest hash and memory-card hashes.

## 4. Architecture

```
psx (CLI)  ─┐
psx mcp    ─┴─> opkit (envelope, artifacts, context, registry, MCP)
                  │
                  ├── psx-media   pure Rust: CUE/BIN, ISO9660, SYSTEM.CNF, PS-X EXE,
                  │               MIPS R3000 disassembler, streaming hashes
                  └── psx-redux   worker supervisor: spawn, handshake, budgets,
                                  timeout/kill, temp dirs for cards/states
                                     │ local socket / stdio, JSON lines
                                     ▼
                                 pcsx-redux -no-ui -interpreter -debugger
                                   -bios … -iso … -dofile psx_bridge.lua
                                 (pinned commit, optional private patch set)
```

Bridge protocol **(probe)**: the Lua script opens a channel to the supervisor
(stdin/stdout via `-lua_stdout`, Redux's libuv-backed networking, or the
`/api/v1/lua` web endpoint). The R1 probe decides which. It then serves a small set of
commands:

- `run_frames`
- `step`
- `regs`
- `read`
- `vram`
- `screenshot`
- `save_state`
- `load_state`
- `set_pad`
- `watch(range)`, which collects `{pc, regs, addr, width, old, new, frame}`
- `gpu_log_start` / `gpu_log_stop`

## 5. Milestones

> **Revised priority order (2026-10-14).** mdx is a reference, not a template.
> Build what pays off for PS1 reverse engineering first:
>
> 1. Static disc to files to RAM: **done** for CUE/BIN, ISO9660+XA, SYSTEM.CNF and PS-X EXE (`psx disc|ls|extract|exe|sector|addr`).
> 2. Redux headless worker plus Lua bridge (M1).
> 3. Load provenance: log CD `Setloc`/read and BIOS A0/B0/C0 calls (open/read/LoadExec) so RAM regions map back to disc files and LBAs. This is how overlays and packed archives are found.
> 4. PsyQ library signature matching to name functions, then export symbols to Ghidra.
> 5. VRAM/texture viewer and GPU packet log tied to the submitting code.
> 6. Watch/trace, deterministic continuation, MCP and an artifact store.
>
> **Reconstruction side (done 2026-10-14):**
> - `templates/psx-bevy` + `psx template create`: GPU-based Bevy materials give the PS1
>   look without PS1 resource limits. Game code stays idiomatic Bevy, with 2D and 3D.
> - `psx-gpu` (CPU, faithful) is kept for evidence work: replaying and diffing GP0 streams and decoding VRAM.
> - `psx-gte` supports exact GTE comparisons.
>
> **Next steps:**
> - TIM/TMD loaders that produce Bevy `Image`/`Mesh`.
> - Optional 15-bit quantisation pass for sprites.
> - Palette (CLUT) swap material for 2D.


### M0: Environment (≈1 day)

- Build PCSX-Redux at a pinned commit (Linux Makefile).
- Install a `mipsel-none-elf` toolchain for nugget/PSYQo. PSn00bSDK is stalled
  (last release 2023), so prefer nugget, which is MIT, active and the same code as Redux's `src/mips`.
- Supply a BIOS (see open questions).
- Init a git repo with `.gitignore` covering `media/`, `bios/`, `.psx/`.

### M1: Backend probes (R1 + R5 core)

Write `tools/probes/` with a PSYQo homebrew EXE that writes a known counter, draws a
rectangle and reads the pad. Then demonstrate, under `-no-ui` without xvfb **(probe)**:

1. Two identical runs give identical RAM, VRAM and pixels (determinism).
2. Registers and PC at frame N.
3. A Lua write watchpoint attributes the counter write to the correct PC.
4. Save at N, resume to M equals an uninterrupted run to M.
5. Enabling observation does not change results.
6. Throughput with and without an Exec hook, to decide whether the trace patch is needed.

Spot-check against unmodified DuckStation regtest screenshots. Output:
`research/backend-comparison.md`.

### M2: Static media (R2), parallel with M1, no emulator needed

- `psx-media` crate covering:
  - CUE parsing: Tomb Raider is 1 data track + ~50 audio tracks, Croc is single-track
  - Mode 2 sector handling
  - ISO9660
  - `SYSTEM.CNF`
  - PS-X EXE header/load mapping
  - a MIPS disassembler with delay-slot annotation
- Ops: `disc.inspect`, `disc.files`, `disc.read`, `exe.inspect`, `disasm`.
- Media manifest: streamed hashes per file and per track, plus logical-disc identity.
- Fixtures: authored homebrew disc (via `mkpsxiso`) + the two local games
  (hashes only in the repo).

### M3: First useful tool (milestone B)

- `opkit` extracted from mdx.
- Ops: `boot` (frames, replay, screenshot, RAM capture, `save_state`/`from_state`),
  `ram.dump`, `watch`, `memory.read/search/diff`, `artifact.*`, `schema`, `survey`.
- Run CLI and MCP stdio parity tests, plus two concurrent projects.
- Observation contract written down (R3):
  - frames vs fields
  - PAL/NTSC
  - virtual address + resolved region
  - termination reasons

### M4: Trace and provenance (R3/R4)

- `trace`, with a private Redux patch if M1 shows it is needed.
- Code-generation IDs for overlays.
- `gpu.trace` from the GPU logger.
- `graphics`: VRAM view, texture decode with the packet's CLUT/tpage.
- DMA-vs-CPU writer attribution.

### M5: Audio, then reconstruction (R6/R8)

- SPU voice/register trace and mixed PCM.
- First game case study: an input-driven RAM variable → its writer → a native Rust demo.
  - Croc is the suggested first target: 3D/GTE, a simpler disc.
  - Tomb Raider for CD-DA/streaming.

## 6. Local state

Layout mirrors `megadrive/mdx`: this folder (`psx/psx/`) is the tool project; the
brief `../MDX_AND_PS1_RESEARCH.md` stays one level up so its relative links work.

- **Both local discs are PAL (European) releases:**
  - Croc is `SLES_005.93`. That file is a 124 KB loader at 0x80100000 that presumably
    chain-loads `CROC.EXE` (0x80010000). Assets live in `CROCFILE.1` with a
    `CROCFILE.DIR` index; 33 of the 41 files are XA/STR streams.
  - Tomb Raider is `SLES_004.86`, with `PSXDATA/LEVEL*.PSX` level files, `.FMV` streams and 56 CD-DA tracks.
  - **Use a PAL BIOS:** `SCPH1002.bin`, `scph5502.bin` or `scph7502.bin`.
- `media/` (ignored), with SHA-1 verified against archive.org's `_files.xml`:
  - `Tomb Raider.cue/.bin`, 615,040,944 B: 1 MODE2/2352 data track + ~50 CD-DA tracks.
    The `.bin` has SHA-256 `fcdcbf0f…0d3`.
  - `Croc - Legend of the Gobbos.cue/.bin`, 698,838,000 B. The `.bin` has SHA-256 `53ed8404…8cc`.
- `bios/` (ignored): 64 files from archive.org `the-ultimate-playstation-1-bios`,
  all SHA-1 verified. `_files.xml` holds the archive's published hashes.
  - The DESR-* files are PSX-DVR (PS2-based) dumps and are not used.
- Not installed: PCSX-Redux, Mednafen, DuckStation, any MIPS cross-compiler.
  cargo and cmake are present.

## 7. Decisions (2026-10-14)

1. **BIOS:** supplied (see above). OpenBIOS is used only as a homebrew cross-check.
2. **Backend:** Redux adapter first. An in-house Rust core is an optional later
   project if Redux observations prove insufficient, with Redux then becoming the oracle.
3. **DuckStation:** an unmodified local install may be used for reference captures only.
4. **Shared code:** copy mdx's envelope/artifacts/context/MCP into `crates/opkit`.
   mdx is under active development and is not modified. Reconcile later.

## 8. Own-emulator effort estimate

A PS1 core is a larger job than mdcore, but not a different kind of job.

- **CPU:** the R3000 itself is simpler than the 68000; it has a regular encoding and no complex addressing modes.
- **Where the effort is:**
  - GTE fixed-point accuracy
  - GPU rasterisation rules (dithering, semi-transparency, texture windows)
  - the CD-ROM controller's command/IRQ timing, which many games are sensitive to
  - MDEC
  - SPU (ADPCM, envelopes, reverb)
  - DMA and timing
- **Size:** a careful, observable interpreter-only core is roughly 2–4× mdcore's ~12.7k lines.
- **Reasons to defer:** PS1 test ROM suites are thinner (amidog's CPU/GTE tests, ps1-tests by
  JaCzekanski) than 68000 SingleStepTests, and getting commercial games to boot is
  dominated by CD timing.

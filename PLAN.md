# psx toolkit plan (draft, 2026-10-14)

> **Update (2026-10-15):** The first Ghidra integration is implemented on
> `feat/ghidra-integration`: a single CLI/MCP call imports an EXE and returns
> pseudocode and callers. See [research/ghidra.md](research/ghidra.md) for validation
> and [docs/ghidra-integration.md](docs/ghidra-integration.md) for the remaining
> snapshot/annotation/symbol milestones. Much of the original list below has shipped;
> treat it as historical context rather than the current task queue.

> **Template update (2026-10-15):** `feat/headless-template` adds offscreen GPU
> rendering (`--mcp --headless --software`) and paused reflected component-field
> edits (`game_set_component`) to the shared starter. See
> [research/template-agent-tools.md](research/template-agent-tools.md) for verification.

> **GPU capture update (2026-10-15):** `feat/gpu-next-draw` adds bounded next-draw
> selection to native CLI/MCP runs, with selected relative/absolute frame identity
> and frozen per-frame artifacts. See [research/gpu-next-draw.md](research/gpu-next-draw.md).

## Current toolkit queue (agreed 2026-10-15)

Items 1–5 and the two agent-usability slices below are implemented.
This queue takes precedence over the historical
milestones below. Scope is the shared toolkit and starter template; use the game
studies as validation inputs.

1. **Sector-indexed archive support — done (2026-10-15).** Add a validated reader for archives such
   as King's Field's `COM/*.T`: a little-endian u16 entry count followed by
   count+1 u16 offsets in 2048-byte sectors. Check table bounds, ordered offsets
   and the final file boundary; equal offsets represent empty entries. Make
   `assets scan` and extraction entry-aware, retaining the archive entry number
   and byte offset in provenance. Verify malformed tables with authored fixtures
   and exercise the existing local FDAT/RTMD/RTIM/MO/ITEM0 archives.
   The [archive survey](research/archive-survey.md) records documented reuse and
   candidate follow-up readers; Armored Core's demo also documents this layout.
   Implemented with shared Croc/DOT-T entries, bounded scanning/conversion,
   CLI/MCP raw entry extraction and authored malformed fixtures. The five local
   archives and Croc comparisons pass; see [validation](research/archive-support.md).
2. **Preprocessed TMDs and raw texture-upload streams — done (2026-10-15).** Build on archive entries
   with explicit, validated format support or reusable decoding APIs. King's
   Field's RTMD uses a nonstandard header and vertex/normal byte offsets in
   place of indices; RTIM contains repeated upload rectangles and pixel data,
   including palettes. Preserve strict standard TIM/TMD detection. Validate
   normalized geometry and decoded uploads against the existing runtime evidence.
   **Progress (2026-10-15):** chose agent-supplied executables for game-specific
   formats. Shared archive/model/asset-wrapper/video-frame contracts, CLI/MCP
   integration and `template parser --kind archive|model|asset|video` starters are
   implemented. King's Field RTMD is a worked external parser, preserving strict
   standard TMD detection. Ordered raw VRAM uploads now have a validated shared
   document/API, asset scan/extract integration and `--texture-parser` for model
   texture sources. The external RTIM example parses all 72 nonempty banks;
   86,032 opening-region words match captured VRAM exactly. Three model texture
   keys remain uncovered in the selected banks; runtime differences outside
   those opening regions are documented rather than hidden. See
   [external parsers](docs/external-parsers.md) and
   [model validation](research/parser-extensions.md) /
   [upload validation](research/raw-texture-uploads.md).
3. **Optional reciprocal depth cue in the template — done (2026-10-15).** Add an authored
   `PsxMaterial` parameter using camera depth and per-vertex colour modulation.
   Preserve ordinary `DistanceFog` behavior when the option is disabled. Use
   King's Field's recorded cue and large-wall comparisons to check fidelity.
   Implemented configurable bias/scale/far colour, a near/far constructor,
   camera-space per-vertex shading and unchanged default DistanceFog behavior.
   The authored large-wall example and headless GPU/MCP checks cover coloured
   targets, texture modulation, perspective/orthographic cameras and fog override.
   The recorded King's Field curve is checked against an actual light-preset
   setting and software-GTE reference values; fixed-point precision limits and
   whole-game comparison scope are explicit in
   [validation](research/template-depth-cue.md).
4. **Ghidra G2: runtime code and names back into traces — done (2026-10-16).** Accept RAM snapshots
   and candidate code ranges, keep same-address overlays distinct, support
   renames/comments and export byte-bound symbols for residency-checked traces.
   Follow the detailed scope and gates in
   [docs/ghidra-integration.md](docs/ghidra-integration.md#g2-runtime-code-overlays-and-names-back-into-traces).
   Implemented content-bound RAM queries, explicit ranges/seeds/GP, canonical RAM
   alias queries, staged rename/comment revisions and hashed function-body symbol
   exports. Native traces and watch events verify names live; final-only evidence
   is marked as a candidate. Same-address replacement and the Mega Man 8 floor
   writer roundtrip pass. See [validation](research/ghidra-runtime.md).
5. **Template input rebinding and example controls UI — done (2026-10-16).** Add a rebinding API to
   the template's input controller so players can change keyboard/gamepad
   bindings while game code continues to consume logical controller actions.
   Provide a reusable, PS1-style example UI that displays current bindings and
   lets players select an action and assign a new button/key. Include cancel,
   restore-defaults, clear conflict handling and persistence of chosen bindings.
   Document how to reuse the example; the game implementor decides where to
   integrate it into the finished game's menus after reverse engineering.
   Implemented keyboard/gamepad mappings, transactional conflict swaps, draft
   editing, restore/clear/discard and validated atomic profile saves. The reusable
   PS1-style menu captures input and suspends gameplay until physical release;
   developer MCP input/steps respect the same boundary. Tests cover keyboard and
   gamepad flows, malformed profiles, failed saves, focus loss and reconnects;
   offscreen GPU/MCP verification checks the actual rendered menu. See
   [validation](research/template-controls.md) and
   [integration](templates/psx-bevy/README.md#rebinding-and-the-example-controls-menu).

The first three items come from the King's Field study's `PSX_FEEDBACK.md`
(local sibling project `../external_agent_tests/kings_field/`). Keep its game
implementation separate from these shared changes.

**Session handoff (2026-10-16):** completed queue items 1–5 and the agent
usability slices are on `master`, including runtime Ghidra, input rebinding,
project isolation and embedded guidance (`46a9d43`, `ef3e1db`). The Cyber Egg
follow-up below adds host directory asset sources, conversion diagnostics,
shared signature configuration and clearer GPU search budgets. Continue using
external parsers for niche formats and add built-ins when reuse and validation
justify them.

A quick post-G2 project review is recorded in
[research/project-review-2026-10-16.md](research/project-review-2026-10-16.md).

### Agent usability (2026-10-16)

The [Tomb Raider/interface review](research/agent-usability-review.md) informs this
work. Its agent notes predate G2; retain the review's corrections about VAB
validation, video modes, sector data and transfer-size arithmetic.

The first slice implements `context`, local BIOS inventory/import and explicit
hash-verified regional downloads, shared storage, saved project defaults and native
as the default backend. Native checkpoints select their exact BIOS identity.
CLI/MCP initialization and run reports identify the source build. Compact MCP
responses attach requested screenshots; function disassembly exposes heuristic
boundary/search/listing limits and emits its discovered extent by default. CD
command history records command-time frames rather than reconstructing them from
the final video mode. See [validation](research/agent-usability-first-use.md) and
[BIOS setup](docs/bios-setup.md).

The second slice implements bounded `artifact_read` inspection with filters,
projection and continuation cursors; typed inline controller states with validated
press/release timing shared with replay files; query pagination and explicit scan/
recording coverage; and cancellable, deadline-bound same-analysis Ghidra queues.
Foreign/GUI locks and external-edit detection remain protected. Compact text
responses omit duplicate detailed arrays. See [workflow contracts](docs/agent-workflow.md)
and [validation](research/agent-usability-workflow.md). These four follow-ups are
complete, as is template input rebinding. Backend pacing/fidelity investigation
remains a separate correctness workstream.

### External-test feedback follow-up (2026-10-16)

The [Tomb Raider/WipEout review](research/tomb-raider-wipeout-feedback-review.md)
separates current gaps from older builds and unsupported diagnoses. The first
addition is the read-only CLI/MCP `guide`: embedded overview and focused setup,
input, code, assets, Bevy and troubleshooting guidance, usable without a skill
loader. Replay-file wording and PAL template suggestions are corrected. MCP tools
now accept isolated per-call project roots; asset extraction returns bounded
previews or brief summaries backed by complete content-bound manifests. Native
input application logs and GPU content classifications/draw filters expose what
was actually applied or captured. See [validation](research/feedback-improvements.md).
Backend pacing and optional screenshot normalization remain separate correctness
work. Optional extraction layout/retention controls are not implemented here.
Skill/plugin packaging is deferred until after the feedback improvements.

### Cyber Egg feedback follow-up (2026-10-16)

Reviewed the sibling study's `research/PSX_TOOL_FEEDBACK.md` against current
source. [Review and validation](research/cyber-egg-feedback-review.md) separates
implemented fixes from remaining interface work:

- Host-directory asset scan/extract now expands regular files recursively in
  sorted order, preserving sibling archive/sound-bank lookup and per-file
  provenance. The same path handling applies to texture sources.
- Starter instructions explicitly migrate demo-dependent tests when replacing
  `demo/`, including the ignored GPU/MCP test; ignored tests still compile.
  Build guidance explains feature switching and recommends consistent `dev-mcp`
  use during agent iteration.
- TMD extraction warns about omitted primitives and reports skipped counts in
  full and brief summaries. Gs-style preprocessed primitive runs remain an
  explicit external-parser task; no speculative auto-detection was added.
- `PSX_SIGS_DIR` provides a shared signature default; explicit `sigs` wins.
  `context.signatures` exposes its resolved path and directory presence. BIOS
  guidance explains importing checkout-local dumps into the existing shared store.
- GPU capture search metadata exposes the effective window within `frames` and
  whether the run budget truncates it.

Workspace selection (2026-10-16): no server-side session handle, since stdio MCP
has no thread identity and subagents share one connection. The default root is now
`--root`/`PSX_ROOT`, else the client's single MCP root (refreshed on
`roots/list_changed`), else the working directory; other roots run in the isolated
worker. `mcp --require-project-root` refuses unscoped calls, and the documented
registration no longer pins the toolkit checkout. MCP roots are deprecated by
SEP-2577, so the working-directory default is the durable path. Named workspace
aliases remain unbuilt unless feedback asks again.

Trajectory comparison (2026-10-17): the template's `game::testing::trajectory`
replays `psx run --replay` files tick by tick (`Alignment`: fields per tick and
start row), loads `samples.csv`, records the rebuild in the original's units and
`compare`s per column with tolerances. Failures report first divergence, nearby
values and a one/two-tick shift hint; edges inside a multi-field tick are refused.
Validated on Mega Man 8's held-out `mixed` scenario recorded directly with
`--sample`: 101 rows, four columns, exact.

**Next feature, on hold (2026-10-17): audio** (original M5). Start with a short
design pass covering: an SPU voice trace (key-on/off per voice with sample address,
pitch, volume, ADSR and frame, so sounds tie to gameplay events); SEQ/SEP sequence
decoding to MIDI or an equivalent event list; and template playback of decoded
VAB samples with PS1 character (pitch, ADSR, reverb). Validate against captured
`native wav` output.

Still open: shorter MCP descriptions/shared schema guidance; typed sample
arguments; an optional run summary mode with fewer artifact descriptors; and
measuring/aligning starter dependency features to reduce duplicate compilation.
Keep per-call project isolation and strict standard-format parsing intact.

## Original plan (historical)

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
> - ~~TIM/TMD loaders that produce Bevy `Image`/`Mesh`.~~ Done: `psx-assets`, `psx assets scan/extract`, template `TimLoader`/`TmdLoader` ([research/assets.md](research/assets.md)).
> - Done 2026-10-15: 15-bit screen pass (`PsxLook::quantize`), CLUT palettes and
>   blend modes for 2D (`PsxSprite`, `PsxMaterial2d`), per-texel STP in captured scenes.
> - Gamma-space blending when a 3D camera shares the screen (Bevy's 3D path composites linearly).


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

0. **Native emulator (decided):** grow an in-house Rust core (`psx-core`) incrementally behind a
   `Backend` trait, and keep PCSX-Redux as the working backend and differential oracle.
   - Order: first finish the Redux path and its oracle harness. Then build the CPU, memory, DMA, timers and IRQs, then boot the BIOS, the probe EXE and Croc.
   - Gates: lockstep register and RAM comparison on short windows, plus per-frame RAM, VRAM and pixel diffs against Redux. DuckStation screenshots serve as an independent check.
   - The default backend switches per game only once the evidence matches. Every result records its backend identity.
   - Biggest risk: CD-ROM controller timing.

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

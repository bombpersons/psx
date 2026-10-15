# psx: reference for agents

The full command reference for the psx PlayStation reverse-engineering toolkit.
The human introduction is [README.md](README.md); direction and status are in [PLAN.md](PLAN.md).

**The goal.** Use the toolkit to gather evidence (rules, constants, data formats, assets) from the
original game, then write the game again as ordinary modern Rust/Bevy in
[`templates/psx-bevy`](templates/psx-bevy/README.md). The result must not be an emulator in
disguise: someone should be able to pick it up and change it with normal Rust/Bevy techniques. Keep exact
integer behaviour only where the game's feel depends on it, and cite the evidence. Captures
(`.psxscene`) are for reference and verification, never the shipped level. The template's
[AGENTS.md](templates/psx-bevy/AGENTS.md) has the guide, "From evidence to idiomatic code".

## Status

`psx-media` does static inspection of discs and executables. It is pure Rust and needs no emulator.

| Command | What it gives you |
|---|---|
| `psx disc <image.cue> [--hash]` | Tracks, volume, file counts, SYSTEM.CNF and boot EXE header |
| `psx ls <image>` | Every ISO9660 entry: LBA, size, CD-XA attributes, streamed flag |
| `psx extract <image> <path>\|--all -o DIR` | Files as Form 1 data. Form 2/interleaved files are written as raw `.raw` 2352-byte sectors |
| `psx exe <file>\|<image.cue>::/PATH` | PS-X EXE header, load range, region marker and hashes |
| `psx xa <image> <path> [--channel N -o out.wav]` | List a stream's XA-ADPCM channels, or decode one channel to a 44.1 kHz WAV |
| `psx str <image>::/PATH -o DIR [--frames A..B --channel N --y4m]` | Decode an STR movie offline (MDEC bitstream v1/v2/v3, FF7's prefixed frames): `frame_NNNNN.png`, `audio.wav` (same decoder as `psx xa`), `contact.png`, `frames.json` and a JSON summary (size, frame count, fps, BS versions, XA channels). Polygon streams, iki, v0, EA and encrypted variants are detected and refused. Pixels come from the native core's MDEC model and match the emulator bit for bit; see [research/str-movies.md](research/str-movies.md) |
| `psx textures <run_dir> [--page X,Y,DEPTH --clut X,Y] [--vram FILE]` | Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT pair a `--gpu-frame` capture used, full and cropped to the UVs sampled, or one page by hand. A capture is decoded from `gpu_frame_vram.bin`, the VRAM at the end of the captured frame (games that stream sprite frames into VRAM every tick need this), falling back to the run's final `vram.bin` |
| Frame → Bevy | `psx textures` also writes `textures/scene.psxscene`; the template's `PsxSceneRoot` (and `cargo run -- --scene ...`) spawns it as sprites and meshes. Mirrored quads become flipped sprites with their texture rect moved one texel, as the PS1 samples them |
| `psx gpu-replay <stream.bin> -o vram.bin` / `psx vram-diff a.bin b.bin` | Replay a GP0 stream into `psx-gpu`; diff two VRAM dumps per pixel (per-cell counts, channel deltas, diff PNG). `tools/probes/gpu/compare.sh` diffs `psx-gpu` against Redux on the GPU probe EXE |
| `psx assets scan <image.cue>[::/PATH] \| <file> [--json]` | Find TIM, TMD, VAB (VH+VB, or a `.VH` with its `.VB`) and VAG data in disc files, game archives and RAM dumps (`ram.bin`: relocated TMDs resolve at 0x80000000) by signature + full validation. A bare disc image scans every file; `::/DIR/` a directory. Hits inside an archive with a Croc-style `.DIR` index name their entry |
| `psx assets extract <source> -o DIR [--kind K] [--offset HEX] [--textures SRC]` | TIM → PNG per CLUT, TMD → glTF (texture pages decoded from the TIMs in the same file or `--textures` sources, through a virtual VRAM), VAB/VAG → WAV per waveform (+ programs/tones JSON); the original bytes are written next to each. See [research/assets.md](research/assets.md) |
| `psx memcard <image.mcd>` | List the saves on a memory card image: product code, blocks, title, icon frames |
| LibCrypt discs | Put the redump `.sbi`/`.lsd` beside the `.cue`; `psx disc` reports it and the native core's GetlocP honours it |
| `psx sector <image> <lba> [--count N]` | Sector form, XA subheader, owning file, hex preview |
| `psx addr 0x80010000` | Segment, memory region and folded offset |
| `psx run <disc\|exe> --bios B --frames N [...]` | Bounded run in an isolated PCSX-Redux process (default), or on the native core with `--backend native` — the backend for game RE. The native core gives exact observations, native save states, `--trace` for windowed instruction traces with coverage and call summaries, `--memcard1/2 PATH|new` for memory cards (written cards are saved to the run directory, never to your file), `--gpu-frame N` to decode one frame's GPU commands (primitives, texture pages, CLUTs, VRAM at the end of that frame, its drawing area, before/after/wireframe PNGs), and `--pad dualshock` for analog games (replay events take `"sticks": [lx, ly, rx, ry]`; Redux gets its analog pad). Captures RAM, VRAM, screen, registers and an optional state. `--watch ADDR[:W]` reports CPU/DMA writers with PC and value; `--trace-cd` and `--trace-bios` log CD and BIOS activity; `--replay`, `--from-state`/`--save-state`; `--fast` uses the dynarec (no observation). The full report is always written to `<run_dir>/report.json`; `--brief` prints a summary |
| `psx run --backend native --sample ADDR:FMT[:NAME]` | Per-frame values: `samples.csv` with `run_frame,frame,<columns>`, one row per vblank (`--sample-every N`), read byte-exact from RAM/scratchpad as `u8 i8 x8 u16 i16 x16 u32 i32 x32 fxI.F` (e.g. `fx16.16`). Row N is memory after N frames of the run |
| `psx run --backend native --poke ADDR=VALUE[:W\|:FMT][@F\|@A..B\|@A..]` | Write memory at the start of run frame F (or every frame of a range) to test a hypothesis; listed under `pokes` in the report |
| `psx run --backend native --watch-reads LO-HI\|ADDR[:W]` | CPU loads from a range: `reads.jsonl` and the top reader PCs (who reads a variable, which script bytes an interpreter consumes). `--watch-changes-only` drops stores that leave a watched value unchanged |
| `psx run --backend native --snapshot-every N --save-state-at F` | Screens every N frames (`screens/fNNNNNN.png`) and states at chosen frames (`state_fNNNNNN.bin`) in one boot |
| `psx ram diff A B [--width 1\|2\|4 --range LO-HI --min-delta N --signed]` | Units that differ between two `ram.bin` snapshots (run directories or files), with deltas and clusters of nearby changes |
| `psx ram search RUN... --pred increased [--values 441,*,376 --between LO..HI --save F --candidates F]` | Cheat-finder search over ordered snapshots: `changed`, `unchanged`, `increased`, `decreased`, `delta=N`, `any` per consecutive pair; narrow over rounds with `--save`/`--candidates` |
| `psx ram view RUN... --addr X --len N --as fx16.16 [--text]` | A struct from several snapshots side by side, differing rows marked |
| `psx disasm <exe\|--ram RUN> [--addr X --count N \| --function X] [--text]` | MIPS disassembly from an EXE or a run's RAM (overlays, runtime code; warns where RAM differs from the EXE). `--function` takes any address inside a function and finds its bounds (jal targets, `addiu sp,sp,-N` prologues, the final `jr ra`). `--text` prints an assembler listing with labels and resolved `lui`/`gp` addresses (`; load 0x8015E8A8`); JSON lines otherwise |
| `psx xrefs ADDR\|LO-HI [--source exe] [--ram RUN] [--gp X]` | Every static reference: jal/j/branch targets, `lui`+`addiu/ori/load/store` pairs, `gp`-relative accesses and pointer words, grouped by enclosing function |
| `psx loads <run_dir> --disc <cue>` | Turns a `--trace-cd` run into disc file → RAM spans, verified by content |
| `psx symbols <exe\|disc::/PATH>` | Name PsyQ library functions by signature (needs a local `external/psx_psyq_signatures`); Ghidra and `.sym` output. Pass the `.symbols.json` to `run`/`loads --symbols` to name PCs, checked against RAM residency |
| `psx ghidra decompile --source <exe\|disc::/PATH> --addr HEX` | One-call Ghidra pseudocode for the enclosing function, with body ranges, callers/callees and diagnostics. Automatically extracts, imports, analyses and caches; returns an `analysis` manifest for follow-up queries. Optional `--symbols FILE` seeds names. See below |
| `psx ghidra functions\|xrefs --analysis MANIFEST [...]` | Filter/paginate the function index (`--filter`, `--offset`, `--limit`) or references to/from `--addr`, without starting Ghidra again. `--source` also works and analyses automatically |
| `psx template create DIR [--name N] [--region ntsc\|pal] [--fields-per-tick N]` | Native Bevy game starter (see below) |
| `psx mcp [--jobs N]` | Every command above as an MCP tool over stdio (see below) |

Input images can be `.cue`, a raw `.bin` (single data track) or a cooked `.iso`.

`psx native` is an emulator diagnostic (how far the core boots, missing hardware, a profile of the
last frames). For game work use `psx run --backend native`, which has everything `native` does for
captures plus states, watches, traces, samples and pokes.

### Project root

`bios/`, `media/`, `external/` and `.psx/` live in one project root (the main checkout; they are not
committed). `psx --root DIR ...` or `PSX_ROOT=DIR` makes psx change into that directory before running
any command, so relative paths in every argument resolve against it and outputs go to its `.psx/`.
That lets a build in a git worktree (or anywhere else) work without `cd`:

```bash
export PSX_ROOT=/path/to/psx          # main checkout
target/release/psx run "media/Mega Man 8.cue" --bios bios/scph7502.bin --backend native \
  --from-state .psx/mm8-study/s3900/state.bin --frames 62 --replay cross.json \
  --sample 8015E8AC:fx16.16:y --sample 8015E89E:u8:state --brief
```

Paths outside the root must then be absolute (`--replay /abs/cross.json`).

### Frame numbers

A run's own frame numbers count from the run start: power-on, or the restored state with
`--from-state`. These are `--frames`, replay events, `--poke`, `--gpu-frame`, `--trace-from-frame`,
`--save-state-at`, and every `run_frame` field. The `frame` fields in `writes.jsonl`, `reads.jsonl`,
`cd_native.jsonl`, `bios.jsonl`, `samples.csv`, `calls.json` and the first column of `trace.txt` are absolute vblanks
since power-on, as the core counts them. The report gives `frames.start`, and `frame = run_frame + frames.start`.
Native logs also carry `address_hex`/`pc_hex`/`value_hex` next to the numbers, and `value_signed`/`old_signed` by store width.

Other crates:

- **`psx-gpu`:** a software PS1 GPU. It has VRAM, a streaming GP0/GP1 decoder and hardware pixel
  rules, and logs GP0 commands. It is meant for replaying and diffing captured
  draw streams and decoding VRAM, not for game code.
- **`psx-gte`:** a register-accurate GTE (COP2) with typed helpers and libgte-style math.
- **`psx-assets`:** TIM/TMD/VAB/VAG parsing, SPU-ADPCM (shared with the native core's SPU),
  the signature scanner and TMD → glTF. `tim.rs`/`tmd.rs` are copied verbatim into the template.

Backend findings are in [research/backend-comparison.md](research/backend-comparison.md); a worked
provenance example (Croc's boot chain) is in [research/load-provenance.md](research/load-provenance.md).
The first game case study, which traces Croc's player movement from RAM to its writers and then to the native Bevy demo
[`examples/croc-movement`](examples/croc-movement/README.md), is in [research/case-study-croc.md](research/case-study-croc.md).
A 2D capture-to-template study with a rebuilt mechanic (Mega Man 8's walk and jump, verified tick-exact
against RAM) is in [research/case-study-megaman8.md](research/case-study-megaman8.md) and
[examples/megaman8-movement](examples/megaman8-movement/README.md).
`psx run` needs `external/pcsx-redux` built at the pinned commit (or `PSX_REDUX_BIN`).

## MCP server

`psx mcp` serves every command as an [MCP](https://modelcontextprotocol.io) tool over
stdin/stdout, so an agent can drive the toolkit directly. Start it **in the project
root** (the directory holding `bios/`, `media/`, `external/`), or point it there with
`--root`/`PSX_ROOT`: relative paths in tool arguments resolve there and outputs go to
its `.psx/`. With Claude Code:

```bash
cargo build --release -p psx
claude mcp add psx -- /path/to/psx/target/release/psx --root /path/to/psx mcp
```

Tools: `disc`, `ls`, `extract`, `exe`, `sector`, `addr`, `xa`, `str`, `assets_scan`,
`assets_extract`, `memcard`, `textures`,
`run`, `native`, `loads`, `symbols`, `disasm`, `xrefs`, `ram_diff`, `ram_search`,
`ram_view`, `lockstep`, `template_create`, `gpu_replay`, `vram_diff`,
`ghidra_decompile`, `ghidra_functions`, `ghidra_xrefs`, `ghidra_analyze`, `ghidra_doctor`.

- Arguments are the CLI's long flags with `_` for `-` (`trace_cd`, `gpu_frame`,
  `memcard1`, `from_state`); positional arguments are named fields (`image`, `source`,
  `run_dir`); repeatable flags are arrays (`watch`, `symbols`, `trace_range`). Unknown
  arguments are rejected.
- A tool's `structuredContent` is exactly the JSON the CLI prints for the same command
  (`tests/mcp_stdio.rs` checks this); errors are `{"error": {"code", "message"}}`
  with `isError: true`. A partial run or a lockstep divergence is a result, not an error.
  The one exception is `text: true` (`disasm`, `xrefs`, `ram_view`): the CLI prints only
  the listing, while the tool returns the JSON with the listing in its `text` field.
- Emulation tools return paths (`run_dir`, `files`) rather than RAM, VRAM or traces.
  Any result over 256 KiB (e.g. `ls` of a large disc) is written to `.psx/mcp/` and
  replaced by `result_file` plus a summary of its small fields.
- `run` and `native` take `return_screen: true` to attach the final screen PNG as
  image content.
- Timeouts: `run` keeps its `timeout` (default 120 s). Over MCP, `native` and
  `lockstep` default to `timeout: 300` and stop with status `timeout`. Each call also
  has a hard cap (its `timeout` + 60 s, otherwise 15 min). A call that overruns the
  cap, or is cancelled, returns an error. Ghidra workers and their decompiler
  children are stopped; other tools' worker threads are left to finish. Emulation,
  `symbols`, `loads` and Ghidra queries share `--jobs` slots (default 2).
- Logs go to stderr; stdout carries only the protocol.

## Ghidra: one call from a writer PC to pseudocode

Call `ghidra_decompile` with just an executable source and an address inside the
function. No preliminary `ghidra_analyze`, GUI session or custom scripts are needed:

```json
{"source":"media/Croc - Legend of the Gobbos.cue::/CROC.EXE","addr":"0x800286B0"}
```

Equivalent CLI:

```sh
psx ghidra decompile --source 'media/Croc - Legend of the Gobbos.cue::/CROC.EXE' --addr 800286B0
```

The result includes `pseudocode`, `function` (entry, name, signature, body ranges),
paginated `callers`/`callees`, `diagnostic`, `warnings`, a cached JSON `artifact`,
and an `analysis` manifest path. Use `analysis` instead of `source` on later calls.
`ghidra_functions` accepts `filter`, `offset`, `limit`; `ghidra_xrefs` accepts
`addr`, `offset`, `limit`. Both read exported indexes without launching a JVM.
`ghidra_analyze` is optional prewarming. `ghidra_doctor` diagnoses installation/Java.

The first call imports/analyzes and can take minutes; `timeout` defaults to 300 s,
and `decompile_timeout` defaults to 30 s per function. Completed queries are cached;
timeouts and failed decompilations are not. `status: partial` and `decompile_status`
identify a missing function or a decompiler failure while retaining the analysis
handle. Callers list resolved call references; unresolved indirect calls remain unknown.

Requires a user-installed Ghidra and its Java prerequisite. Discovery checks
`ghidra_home`, then `PSX_GHIDRA_HOME`, `analyzeHeadless` on PATH, then Ghidra
directories in `~/tools`, `/opt`, and `external`. Tested with Ghidra 12.1.4 / Java 21
on Linux. Worker supervision uses Unix process groups. Other psx commands do not
require Ghidra. A `.sym` or `.symbols.json` file can be supplied in `symbols` to
seed function names; no local PsyQ signature database is required for decompilation.

Metadata, immutable input copies, logs and indexes live under `.psx/ghidra/`.
Ghidra databases live under ignored `ghidra-projects/`, because Ghidra 12 rejects
hidden directory components in project paths. Use a project root without hidden
parent directories. The returned `project` opens in Ghidra's GUI. Close it before
using headless queries. External project edits invalidate caches; `reanalyze: true`
with `source` creates a fresh project and preserves the edited one. Importing GUI
annotations back into psx is future work.

This first integration accepts PS-X EXEs, not runtime RAM snapshots. Generic
MIPS pseudocode is useful for ordinary game code; PS1 load-delay and COP2/GTE
semantics need checking against instructions/runtime observations. Snapshot
analysis, annotation/type editing and verified symbol round trips are planned in
[docs/ghidra-integration.md](docs/ghidra-integration.md).

## Native game template

```bash
psx template create ../my-game   # standalone Bevy 0.20 project
psx template create ../croc --region pal --fields-per-tick 2   # 25 Hz logic, like Croc PAL
```

[`templates/psx-bevy`](templates/psx-bevy/README.md) is idiomatic Bevy with a
PS1 look: `PsxMaterial` gives vertex snapping, affine textures, Gouraud
lighting, 15-bit dither, depth cue and the four blend modes. It also provides a
low-resolution `PsxView` target, 2D sprites, DualShock input, fixed 50/60 Hz
ticks, and a feature-gated developer MCP for stepping, input and screenshots.
Extracted original `.tim` files load as Bevy `Image`s and `.tmd` models as
`PsxModel`s (`PsxModelRoot`); `cargo run -- --model x.tmd --texture x.tim` views one.

## Local data (never committed)

- `media/`: disc images
- `bios/`: BIOS dumps
- `.psx/`: run outputs

## Tests

```bash
cargo test
cargo test -p psx-media -- --ignored   # needs media/ discs
PSX_ROOT=/path/to/psx cargo test -p psx --release --test native_observe -- --ignored   # samples/pokes on the Mega Man 8 study state
```

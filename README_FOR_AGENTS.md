# psx: reference for agents

The full command reference for the psx PlayStation reverse-engineering toolkit.
The human introduction is [README.md](README.md); direction and status are in [PLAN.md](PLAN.md).

**The goal.** Use the toolkit to gather evidence (rules, constants, data formats, assets) from the
original game, then write the game again as ordinary modern Rust/Bevy in the
[template workspace](templates/psx-bevy/README.md).
The result must not be an emulator in
disguise: someone should be able to pick it up and change it with normal Rust/Bevy techniques. Keep exact
integer behaviour only where the game's feel depends on it, and cite the evidence. Captures
(`.psxscene`) are for reference and verification, never the shipped level. The template's
[AGENTS.md](templates/psx-bevy/AGENTS.md) has the guide for writing the port, and `guide`
topic `port` the whole-port workflow.

## Status

`psx-media` does static inspection of discs and executables. It is pure Rust and needs no emulator.

Call MCP `guide({})` for a built-in workflow overview, or
`guide({"topic":"input"})` for focused help. CLI equivalents are `psx guide` and
`psx guide --topic input`. Topics: setup, input, run, fields, states, cases, code,
assets, bevy, port and troubleshooting. Guidance ships in the executable and needs no skill loader,
checkout or network; it does not run an investigation or change project settings.

Start with MCP `context({})` or `psx context --image 'media/Game.cue'` to discover
the running build, workspace, BIOS candidates and backend capabilities. One-time
setup uses `bios_list` / `bios_install`, or `psx bios list` / `psx bios install`.
Import a local dump with `--from PATH`, or explicitly request a checksum-verified
download with `--download --region pal|ntsc-u|ntsc-j`. `context --save` persists
resolved media/BIOS/backend defaults. Runs default to native, discover local BIOSes
by region, and require the exact BIOS hash for native checkpoints. Inspection and
runs never download. See [BIOS setup](docs/bios-setup.md) for storage and selection rules.

| Command | What it gives you |
|---|---|
| `psx guide [--topic TOPIC]` | Embedded workflow guidance, topic navigation, build identity and the current path-resolution root; MCP `guide` exposes the same documentation |
| `psx artifact-read PATH [--pointer /rows --offset N --limit N]` | Read artifact directories, JSON, logs, samples, text, raw bytes or PNGs. MCP `artifact_read` attaches PNGs automatically. Stream filters and cursors keep evidence bounded; see [agent workflow](docs/agent-workflow.md) |
| `psx disc <image.cue> [--hash]` | Tracks, volume, file counts, SYSTEM.CNF and boot EXE header; `--hash` adds file hashes and `track_sha256`, per-track hashes of the sectors as read, which match across image formats |
| `psx ls <image> [--filter NAME --offset N --limit N]` | Paginated ISO9660 entries: LBA, size, CD-XA attributes, streamed flag; full totals and index coverage |
| `psx extract <image> <path>\|--all -o DIR` | Files as Form 1 data. Form 2/interleaved files are written as raw `.raw` 2352-byte sectors |
| `psx exe <file>\|<image.cue>::/PATH` | PS-X EXE header, load range, region marker and hashes |
| `psx xa <image> <path> [--channel N -o out.wav]` | List a stream's XA-ADPCM channels, or decode one channel to a 44.1 kHz WAV |
| `psx str <image>::/PATH -o DIR [--frames A..B --channel N --y4m]` | Decode an STR movie offline (MDEC bitstream v1/v2/v3, FF7's prefixed frames): `frame_NNNNN.png`, `audio.wav` (same decoder as `psx xa`), `contact.png`, `frames.json` and a JSON summary (size, frame count, fps, BS versions, XA channels). Polygon streams, iki, v0, EA and encrypted variants are detected and refused. Pixels come from the native core's MDEC model and match the emulator bit for bit; see [research/str-movies.md](research/str-movies.md) |
| `psx textures <run_dir> [--page X,Y,DEPTH --clut X,Y] [--vram FILE]` | Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT pair a `--gpu-frame` capture used, full and cropped to the UVs sampled, or one page by hand. A capture is decoded from `gpu_frame_vram.bin`, the VRAM at the end of the captured frame (games that stream sprite frames into VRAM every tick need this), falling back to the run's final `vram.bin` |
| Frame → Bevy | `psx textures` also writes `textures/scene.psxscene`; the template's `PsxSceneRoot` (and `cargo run -- --scene ...`) spawns it as sprites and meshes. Mirrored quads become flipped sprites with their texture rect moved one texel, as the PS1 samples them |
| `psx gpu-replay <stream.bin> -o vram.bin` / `psx vram-diff a.bin b.bin` | Replay a GP0 stream into `psx-gpu`; diff two VRAM dumps per pixel (per-cell counts, channel deltas, diff PNG). `tools/probes/gpu/compare.sh` diffs `psx-gpu` against Mednafen on the GPU probe EXE |
| `psx assets scan <image.cue>[::/PATH] \| <file-or-directory> [--json]` | Find standard TIM/TMD/VAB/VAG assets by signature + full validation. A bare disc scans every file; `::/DIR/` selects an ISO directory. Host directories expand recursively in sorted order without following symlinks, retaining sibling `.DIR`/`.VB` lookup; `ram.bin` maps relocated TMDs at 0x80000000. DOT-T and Croc `.DIR` archives expose all entries, including empty entries and archives without known assets; hits carry entry indices, names where available and byte provenance. See [archive support](research/archive-support.md) |
| `psx assets extract <source> -o DIR [--kind K] [--offset HEX] [--textures SRC]` | TIM → PNG per CLUT, TMD → glTF (texture pages decoded from the TIMs in the same file or `--textures` sources, through a virtual VRAM), VAB/VAG → WAV per waveform (+ programs/tones JSON); the original bytes are written next to each. See [research/assets.md](research/assets.md) |
| `psx assets extract <source> --entry N -o DIR` | Extract one zero-based archive entry as raw bytes, including sector padding or an empty entry. Requires one source file; excludes `--kind`, `--offset` and `--textures`. Both asset commands accept `--archive dot-t` for explicit validation or `--archive none` for a flat scan |
| `psx template parser DIR --kind archive\|model\|asset\|video` | Generate an executable Python parser, authored sample and protocol docs. Use `--archive-parser`, `--model-parser`, `--asset-parser` on asset commands or `--frame-parser` on `str`. Any language can implement the same stdin-bytes/stdout-JSON contract; see [external parsers](docs/external-parsers.md) |
| `psx memcard <image.mcd>` | List the saves on a memory card image: product code, blocks, title, icon frames |
| LibCrypt discs | Put the redump `.sbi`/`.lsd` beside the `.cue` or `.chd`; `psx disc` reports it and the native core's GetlocP honours it |
| `psx sector <image> <lba> [--count N]` | Sector form, XA subheader, owning file, hex preview |
| `psx addr 0x80010000` | Segment, memory region and folded offset |
| `psx run [disc\|exe] [--bios B] --frames N [...]` | Bounded run on the native core, which gives exact observations, native save states, `--trace` for windowed instruction traces with coverage and call summaries, `--memcard1/2 PATH|new` for memory cards (written cards are saved to the run directory, never to your file), `--gpu-frame N` to decode one frame's GPU commands (primitives, texture pages, CLUTs, VRAM at the end of that frame, its drawing area, before/after/wireframe PNGs), and `--pad dualshock` for analog games (replay events take `"sticks": [lx, ly, rx, ry]`). Captures RAM, VRAM, screen, registers and an optional state. `--watch ADDR[:W]` reports CPU/DMA writers with PC and value; `--trace-cd` and `--trace-bios` log CD and BIOS activity; `--trace-spu` logs sound voice key on/off with pitch, volume, ADSR, storing PC and a sample hash that matches `assets extract`'s per-waveform `adpcm_sha256`, plus sound RAM uploads (`spu.jsonl`; the report groups key ons by sample); `--audio` records the mixed sound output (voices, reverb, CD/XA audio) to `audio.wav`; `--profile` writes CPU cycles by function to `profile.json` (with `--symbols` for names); `--replay`, `--from-state`/`--save-state`. The full report is always written to `<run_dir>/report.json`; `--brief` prints a summary with artifact descriptors, `--summary` only status, paths and counts for the requested observations |
| `psx handoff [disc] --request TEXT [--from-state S] [--detach]` / `psx handoff-status DIR [--wait N]` | Ask the person at the machine to play to a point: a window shows the game with the request in a side panel (never over the game); keyboard and gamepad with a rebinding menu (F1 or Start+Select), sound, rewind (F2), quick save/load, start over, marks (F6: state, RAM and screen of a moment, labelled, with RAM diffs between marks in the report), and hand back with a note. Writes `state.bin`, `ram.bin`, `input.json` (their input as a `--replay` from the handoff start), `screen.png`, `marks/<n>/` and `report.json`. MCP `handoff` always detaches; poll `handoff_status`. See `guide --topic input` |
| `psx handoff ... --expect NAME=VALUE` | The end state asked for, in `--until`'s syntax on field map names; the result says whether the person's state meets it |
| `psx states make NAME --frames N --description TEXT [--from STATE] [--input ...\|--replay R] [--until ...]` | Run from a named state (or power-on) and name the state it ends in. Each state (`.psx/states/NAME.bin`) has a chain of runs from power-on in `notes/states/NAME.json` |
| `psx states save SOURCE NAME --description TEXT` / `psx states list` | Name a state an earlier run or handoff wrote (its chain comes from the runs' reports); list the named states and whether each is present |
| `psx states rebuild [NAMES...]` | Replay each chain from power-on on this build and compare the state it makes; run after updating psx. See `guide --topic states` |
| `psx case record NAME --from-state S --frames N --subject SUBJECT [--sample ...] [--spec SPEC] [--role R] [--compare COL[=TOL]] [--coverage SPEC:COLS[:ROWS]]` | A case: the subject's fields every field from a start state with input. Writes `specs/cases/NAME.json`, its observation CSV (with the tick column and per-row timing) and a recipe in `notes/recordings/` |
| `psx case check [NAMES...] [--write]` | Record cases again from their recipes on this build and compare with their CSVs; `--write` replaces those that differ. See `guide --topic cases` |
| `psx inventory [MEDIA] [--ram RUN]...` | Every function of every PS-X EXE on the disc and of the overlays RAM snapshots hold, with instructions, basic blocks and how each was found, the gaps no function covers, and the disc's file list. Written to `.psx/inventory.json`, each run's totals to `inventory_history.jsonl` |
| `psx coverage RUN... [--inventory F]` | What traced runs (`run --trace`) executed, per inventory function: blocks executed and not, calls, and the runs reaching it, in `coverage.json` beside the inventory |
| `psx run --gpu-next-draw [--gpu-frame F --gpu-search-frames N]` | Capture the first completed frame with draw commands, starting at F (default 0), searching at most N frames (default 60, range 1..=600) within `--frames`. Reports selected relative and absolute frame numbers, or an explicit miss. See below |
| `psx run --map samples.toml --sample player --sample camera_x` | A field map names the game's memory once: globals, struct layouts, subjects (one struct at an address) and arrays of them. Samples then take names (`player`, `player.x`, `placement[3].yaw`), and `samples.csv` carries names only. `samples.toml` in the project root is read without `--map`. With `[tick]` in the map, a `tick` column counts the game's logic ticks. See `guide --topic fields` |
| `psx run --find-tick` | Every CPU store to main RAM counted by PC and address and grouped by the fields it happens in: the candidates for the store that ends one game tick, for the map's `[tick]` |
| `psx run --until NAME=VALUE` | Stop at the first vblank where named samples hold (also `!=`, `<`, `<=`, `>`, `>=`; repeatable, all must hold). `--frames` caps the wait; `until.met` says whether it came |
| `psx run --sample ADDR:FMT[:NAME]` | Per-frame values: `samples.csv` with `run_frame,frame,<columns>`, one row per vblank (`--sample-every N`), read byte-exact from RAM/scratchpad as `u8 i8 x8 u16 i16 x16 u32 i32 x32 fxI.F` (e.g. `fx16.16`) or a watch-style width `1/2/4` (unsigned). MCP also takes typed `{"addr", "format", "name"}` entries. Row N is memory after N frames of the run |
| `psx run --poke ADDR=VALUE[:W\|:FMT][@F\|@A..B\|@A..]` | Write memory at the start of run frame F (or every frame of a range) to test a hypothesis; listed under `pokes` in the report |
| `psx run --watch-reads LO-HI\|ADDR[:W]` | CPU loads from a range: `reads.jsonl` and the top reader PCs (who reads a variable, which script bytes an interpreter consumes). `--watch-changes-only` drops stores that leave a watched value unchanged |
| `psx run --snapshot-every N --save-state-at F` | Screens every N frames (`screens/fNNNNNN.png`) and states at chosen frames (`state_fNNNNNN.bin`) in one boot |
| `psx ram diff A B [--width 1\|2\|4 --range LO-HI --min-delta N --signed]` | Units that differ between two `ram.bin` snapshots (run directories or files), with deltas and clusters of nearby changes |
| `psx ram search RUN... --pred increased [--values 441,*,376 --between LO..HI --save F --candidates F]` | Cheat-finder search over ordered snapshots: `changed`, `unchanged`, `increased`, `decreased`, `delta=N`, `any` per consecutive pair; narrow over rounds with `--save`/`--candidates` |
| `psx ram view RUN... --addr X --len N --as fx16.16 [--text]` | A struct from several snapshots side by side, differing rows marked |
| `psx disasm <exe\|--ram RUN> [--addr X --count N \| --function X] [--text]` | MIPS disassembly from an EXE or run RAM. Raw listings default to 32 instructions. `--function` emits its discovered extent; `--function-budget` bounds heuristic search (default 4096). Function metadata includes boundary evidence, stop reason and truncation; an explicit `--count` caps only the listing and supplies `listing.next_addr`. Warnings also appear in `--text` assembler listings with labels and resolved `lui`/`gp` addresses. Use Ghidra for analyzed function bodies |
| `psx xrefs ADDR\|LO-HI [--source exe] [--ram RUN] [--gp X]` | Every static reference: jal/j/branch targets, `lui`+`addiu/ori/load/store` pairs, `gp`-relative accesses and pointer words, grouped by enclosing function |
| `psx loads <run_dir> --disc <cue>` | Turns a `--trace-cd` run into disc file → RAM spans, verified by content |
| `psx symbols <exe\|disc::/PATH>` | Name PsyQ library functions by signature (needs a local `external/psx_psyq_signatures`); Ghidra and `.sym` output. Pass the `.symbols.json` to `run`/`loads --symbols` to name PCs, checked against RAM residency |
| `psx ghidra decompile --source <exe\|disc::/PATH> --addr HEX` | One-call Ghidra pseudocode for the enclosing function, with body ranges, callers/callees and diagnostics. Automatically extracts, imports, analyses and caches; returns an `analysis` manifest for follow-up queries. Optional `--symbols FILE` seeds names. See below |
| `psx ghidra functions\|xrefs --analysis MANIFEST [...]` | Filter/paginate the function index (`--filter`, `--offset`, `--limit`) or references to/from `--addr`, without starting Ghidra again. `--source` also works and analyses automatically |
| `psx template create DIR [--name N] [--region ntsc\|pal] [--fields-per-tick N]` | Native Bevy port workspace: `psx-bevy`, `game`, `tools/import` (see below) |
| `psx template update [--dry-run]` | Replace the project's untouched `psx-bevy/` and `vendor/` files with this build's; `.psx-new` beside edited ones; report changed seeded files |
| `psx template adopt DIR [--name N] [--region R] [--fields-per-tick N]` | Start tracking a template workspace that has no `.psx-template.toml` |
| `psx port init DEST --media DISC [--name PKG] [--region R] [--fields-per-tick N]` | A port's workspace: the template, `goal.toml`, `catalogue/`, the disc and BIOS linked in, `.mcp.json` for psx, git. See [Ports](#ports) |
| `psx port confirm [--yes]` / `port status [--plain]` / `port next` | The goal's list and its closure (with `--yes`, the person's confirmation); every feature by stage; the one item due now |
| `psx mechanic check [ID]` | Check `interfaces/` and each mechanic's `mechanic.toml`, `binding.toml` and traces ([docs/mechanics.md](docs/mechanics.md)) |
| `psx mechanic capture ID --name N --from-state S --frames F [--input/--replay]` | Run on the native core, bracket the mechanic's step as its binding says, write `mechanics/ID/traces/N.trace.jsonl` with the read audit and branch coverage; reports external changes and drafts `capture.explained`; refusals name reads that are not of the field's type |
| `psx scenario record ID --name N --frames F [--input/--replay]` | Run on the native core from the scenario's state and write `scenarios/ID/N.jsonl`: the compared glossary quantities at the end of each logic tick ([docs/scenarios.md](docs/scenarios.md)) |
| `psx scenario check [ID...]` | The determinism check: reach each scenario again with idle fields before its chain's last run, record its recordings again and compare values per tick (a different tick schedule is a note) |
| `psx port check [FEATURE]` | At a clean commit: run the game's `tests/mechanics.rs` and `tests/scenarios.rs` and write the features' replay and scenario verdicts |
| `psx mcp [--jobs N]` | Every command above as an MCP tool over stdio (see below) |

Input images can be `.cue`, a raw `.bin` (single data track), a cooked `.iso` or a CD `.chd`
(MAME compressed hunks, as made by `chdman createcd`). A `.chd` reads back the same sectors as the
`.cue`/`.bin` it was made from, so `disc --hash` track hashes and native save states carry over between
them.

`psx native` is an emulator diagnostic (how far the core boots, missing hardware, a profile of the
last frames). For game work use `psx run`, which has everything `native` does for
captures plus states, watches, traces, samples and pokes.

### Project root

`bios/`, `media/`, `external/` and `.psx/` live in one project root (the main checkout; they are not
committed). `psx --root DIR ...` or `PSX_ROOT=DIR` makes psx change into that directory before running
any command, so relative paths in every argument resolve against it and outputs go to its `.psx/`.
That lets a build in a git worktree (or anywhere else) work without `cd`:

```bash
export PSX_ROOT=/path/to/psx          # main checkout
target/release/psx run "media/Mega Man 8.cue" --bios bios/scph7502.bin \
  --from-state .psx/mm8-study/s3900/state.bin --frames 62 --replay cross.json \
  --sample 8015E8AC:fx16.16:y --sample 8015E89E:u8:state --brief
```

Paths outside the root must then be absolute (`--replay /abs/cross.json`).

Over MCP, every tool also accepts `project_root` for one isolated call. Set it to
an existing directory (absolute or relative to the server default); keep it on
subsequent calls. Defaults and outputs stay in that study, and returned next calls
retain the selection. The server's default and concurrent calls are unaffected.
See [project isolation](docs/agent-workflow.md#isolate-game-studies).

### Agent investigation calls

`run` accepts typed inline `input`, e.g.
`[{"frame":0,"buttons":["CROSS"],"until":5}]`; CLI `--input '[...]'`.
`until` releases at its exclusive end; empty buttons release all; frames are
strictly increasing within the run budget. Replay files use the same validated
contract. Runs return artifact descriptors for `artifact_read`, which can filter
CSV/JSONL by frame/PC and project columns, or follow JSON pointers and cursors.

`ls`, asset scans, xrefs, RAM queries, loads and Ghidra indexes expose shared page
metadata: total, offset, returned, limit, truncation/completeness and next offset.
Coverage describes the inspected scope independently. Streaming artifact totals
can be unknown until the entire source is inspected. Ghidra requests for the same
analysis queue within their deadlines; foreign/GUI locks remain errors.
See [contracts and examples](docs/agent-workflow.md).

### Frame numbers

A run's own frame numbers count from the run start: power-on, or the restored state with
`--from-state`. These are `--frames`, replay events, `--poke`, `--gpu-frame`, `--trace-from-frame`,
`--save-state-at`, and every `run_frame` field. The `frame` fields in `writes.jsonl`, `reads.jsonl`,
`cd_native.jsonl`, `bios.jsonl`, `samples.csv`, `calls.json` and the first column of `trace.txt` are absolute vblanks
since power-on, as the core counts them. The report gives `frames.start`, and `frame = run_frame + frames.start`.
Native logs also carry `address_hex`/`pc_hex`/`value_hex` next to the numbers, and `value_signed`/`old_signed` by store width.

`native`'s `cdrom.command_history` records command-time absolute `frame` and
`cycle`, plus `last_frame`/`last_cycle` for aggregated repeats. A missing frame is
unknown; it is never estimated by dividing past cycles by the current video mode.
`video.region_at_capture` describes the final GPU mode, which can differ from the
disc region or earlier boot frames. Screen dimensions and interlace are reported
as captured; they are not forced to a standard 320×240 size.

### Capture the next drawing frame

For games that draw only every second or third vsync, use `--gpu-next-draw`:

```bash
psx run media/game.cue --bios bios/scph7502.bin --from-state state.bin \
  --frames 120 --gpu-frame 110 --gpu-next-draw --gpu-search-frames 6 --brief
```

This searches run frames 110 through 115 inclusively and captures the first completed
frame containing GP0 polygons, lines, rectangles/sprites or VRAM fills. State changes,
NOPs, uploads, downloads and VRAM copies alone do not qualify. A submitted draw counts
even if it is offscreen or leaves pixels unchanged; this selects drawing work rather
than detecting visible changes. Without `--gpu-next-draw`, `--gpu-frame F` retains exact
capture, including empty frames.

The search never extends `--frames`, and selection does not stop the run. The selected
frame's VRAM, display settings and GTE observations are frozen for `textures`, even if
later frames draw again. Its report, including `--brief`, gives `status: "captured"`,
`run_frame`, `absolute_frame`, `complete`, `draw_commands` and `search.frames_inspected`.
For compatibility, GPU capture's legacy `frame` field remains an alias of `run_frame`.
The same metadata appears in `gpu_frame.json` under `summary`.

`search.run_budget_frames` records the requested run length;
`search.effective_max_frames` is `min(max_frames, max(0, frames - from_run_frame))`.
`search.truncated_by_run_budget` marks a window cut short by the run budget.
A one-frame run can inspect only one completed field, even with a 60-frame search
window. Increase `--frames` to allow an alternate non-draw field to pass.
Use `--gpu-draw-filter primitives|textured` with next-draw mode to skip fill-only
frames or require textured drawing; `gpu_frame.content` reports what was captured.

A miss returns `gpu_frame.status: "not_found"`, an `error` message and `reason` of
`search_limit`, `run_end` or `timeout`. Only completed frames count as inspected, so a
partially drawn frame at timeout is never selected by next-draw mode. The overall
run status still describes emulation completion; inspect `gpu_frame.status` separately.
With MCP, set `backend: "native"`, `gpu_next_draw: true` and optionally `gpu_frame`
and `gpu_search_frames` on the existing `run` tool. No extra tool call is required.

Other crates:

- **`psx-gpu`:** a software PS1 GPU. It has VRAM, a streaming GP0/GP1 decoder and hardware pixel
  rules, and logs GP0 commands. It is meant for replaying and diffing captured
  draw streams and decoding VRAM, not for game code.
- **`psx-gte`:** a register-accurate GTE (COP2) with typed helpers and libgte-style math.
  The template vendors it as `psx_bevy::gte`.
- **`psx-fixed`:** numbers for ports. A mechanic is written once, generic over a `Mode`:
  `Fixed<W, F>` computes as the original's compiled C does (checked against the native
  core's R3000), `f32` from the same code. It has angles, vectors, the named integer
  operations and real-number asset fields. The template vendors it as `psx_bevy::num`.
- **`psx-mechanics`:** the platform-free mechanic, interface, binding and trace formats, and the
  glossary, scenario and recording formats, with their checks and JSON Schemas
  (`docs/mechanics.md`, `docs/scenarios.md`); psx's R3000 binding vocabulary is
  `crates/psx/src/mechanic.rs`. The template vendors it for step replay (`psx_bevy::replay`)
  and scenarios (`psx_bevy::scenario`).
- **`psx-assets`:** TIM/TMD/VAB/VAG parsing, SPU-ADPCM (shared with the native core's SPU),
  the signature scanner and TMD → glTF. `tim.rs`/`tmd.rs` are copied verbatim into the template.

Archive reader planning and documented format frequencies are in
[research/archive-survey.md](research/archive-survey.md), with a source-linked
game catalogue and local validation candidates. Implemented archive support is
documented in [research/archive-support.md](research/archive-support.md): DOT-T and
Croc indexes, bounded asset scanning, full entry listings even without asset hits,
and `assets extract SOURCE --entry N -o DIR` for a complete raw entry. Both asset
commands accept `--archive dot-t` to force validated parsing or `--archive none`
to disable indexing. Other families in the survey describe prospective coverage.

Prefer explicit external parsers for game-specific formats. Both asset commands
accept `--archive-parser EXEC` for an index of unchanged source spans,
`--model-parser EXEC` for validated JSON geometry, or `--asset-parser EXEC` for
one standard TIM/TMD/VAB/VAG payload after unpacking a wrapper. `str` accepts
`--frame-parser EXEC` for a normalized BS frame. Repeated `--*-parser-arg=VALUE`
passes literal arguments; `--parser-timeout` defaults to 30 seconds per call.
For raw VRAM streams, asset parsers can return ordered rectangle/word uploads.
`assets extract MODEL --textures BANK --texture-parser EXEC` loads those words
before resolving model page/CLUT references, alongside `--model-parser` if needed.
The worked King's Field RTIM/common-texture parser and runtime comparisons are in
[raw-texture-uploads.md](research/raw-texture-uploads.md).
Original bytes, offsets, parser output and hashes remain inspectable. Generate
starters with `psx template parser DIR --kind KIND` (MCP `template_parser_create`).
The [protocol reference](docs/external-parsers.md) defines limits and geometry,
and includes a worked external King's Field RTMD decoder. Core support should
grow for proven reuse or shared operations, rather than adding every game format.

TMD extraction reports parsed polygon and skipped primitive counts under `details`;
`summary.skipped_primitives` and `summary.warnings` remain visible with `--brief`.
Skipped packets mean the conversion omits geometry. Gs-style preprocessed primitive
runs require an explicit parser; the standard scanner may not discover them.
For runtime FIXP models, `size: 0` means pointers are relocated and no contiguous
original-file extent is known, rather than an empty model.

PsyQ signature lookup uses explicit `symbols --sigs DIR` / MCP `sigs`, then
`PSX_SIGS_DIR`, then the selected project's `external/psx_psyq_signatures`, then the
installer's shared copy in the user data directory (`~/.local/share/psx/psyq_signatures`,
or `%LOCALAPPDATA%\psx\psyq_signatures` on Windows).
Set `PSX_SIGS_DIR` to an absolute checkout path in the server environment to share
one index across game projects. `context.signatures` reports the default path,
selection reason and directory presence; it does not validate or download the index.

Backend findings are in [research/backend-comparison.md](research/backend-comparison.md); a worked
provenance example (Croc's boot chain) is in [research/load-provenance.md](research/load-provenance.md).
The first game case study, which traces Croc's player movement from RAM to its writers and then to the native Bevy demo
[`examples/croc-movement`](examples/croc-movement/README.md), is in [research/case-study-croc.md](research/case-study-croc.md).
A 2D capture-to-template study with a rebuilt mechanic (Mega Man 8's walk and jump, verified tick-exact
against RAM) is in [research/case-study-megaman8.md](research/case-study-megaman8.md) and
[examples/megaman8-movement](examples/megaman8-movement/README.md).
Runs use the in-tree native core and a locally available BIOS; static media/asset tools need
neither. Mednafen is only a test oracle for developing `psx` itself: see
[Developing psx](#developing-psx). Ghidra operations require a local installation
as described below.

## MCP server

`psx mcp` serves every command as an [MCP](https://modelcontextprotocol.io) tool over
stdin/stdout, so an agent can drive the toolkit directly. Relative paths in tool
arguments resolve against the default project root and outputs go to its `.psx/`.
That root is, in order: `--root`/`PSX_ROOT` when given; otherwise the client's MCP
root when it reports exactly one local directory (refreshed when the client reports
a change); otherwise the server's working directory. Register it once without
`--root` so each Claude Code session uses the study it was opened in:

```bash
claude mcp add --scope user psx -- ~/.local/bin/psx mcp
```

(`psx` from the [installer](README.md#get-started); for a source checkout use
`target/release/psx` after `cargo build --release -p psx`, with `-e PSX_SIGS_DIR=...` if the
signatures are not in either default location.)

`psx mcp --require-project-root` rejects any call without `project_root`, for
clients that work across several studies and must not fall back to a default.

Tools: `guide`, `context`, `bios_list`, `bios_install`, `artifact_read`,
`disc`, `ls`, `extract`, `exe`, `sector`, `addr`, `xa`, `str`, `assets_scan`,
`assets_extract`, `memcard`, `textures`,
`run`, `handoff`, `handoff_status`, `native`, `loads`, `symbols`, `disasm`, `xrefs`,
`ram_diff`, `ram_search`, `ram_view`, `template_create`, `template_update`,
`template_parser_create`, `gpu_replay`, `vram_diff`,
`ghidra_decompile`, `ghidra_functions`, `ghidra_xrefs`, `ghidra_analyze`, `ghidra_doctor`,
`ghidra_annotate`, `ghidra_export_symbols`,
`states_make`, `states_save`, `states_list`, `states_rebuild`, `case_record`, `case_check`,
`inventory`, `coverage`, `port_init`, `port_status`, `port_next`. (`port confirm` is
command line only: confirming the goal is the person's.)

- Tool descriptions and parameter descriptions are deliberately short (parameters keep
  their first sentence; the CLI's `--help` has the full text). The `guide` tool's topics
  hold argument rules and recipes. A unit test keeps the published definitions under
  65.7 KB.
- Arguments are the CLI's long flags with `_` for `-` (`trace_cd`, `gpu_frame`,
  `memcard1`, `from_state`); positional arguments are named fields (`image`, `source`,
  `run_dir`); repeatable flags are arrays (`watch`, `symbols`, `trace_range`). Unknown
  arguments are rejected.
- Without `project_root`, a tool's `structuredContent` is the JSON the CLI prints
  for the same command (`tests/mcp_stdio.rs` checks this). Scoped calls, and calls
  whose client-root default differs from the server's working directory, run in an
  isolated worker and additionally identify `project_root` in results/continuation
  calls; `context` then also reports `project_root_source: "client_roots"`.
  Errors include `{"error": {"code", "message"}}`
  with `isError: true`. Common codes: `invalid_arguments` (a missing, conflicting or
  out-of-range argument), `malformed_input` (an input file's contents), `io_error`
  (with `path`), `not_found`, `unsupported_format` (CLI exit code 2), `parser_failed`
  (an external parser), `bios_required` and `media_required` (with next steps),
  `timeout` and `cancelled`. A partial run is a result, not an error.
  The one exception is `text: true` (`disasm`, `xrefs`, `ram_view`): the CLI prints only
  the listing, while the tool returns text plus metadata. Disassembly/xref text
  responses omit duplicate instruction/reference arrays. Asset scans without
  `json: true` similarly return a compact summary and coverage.
- Emulation tools return paths (`run_dir`, `files`) rather than RAM, VRAM or traces.
  Any result over 256 KiB (e.g. `ls` of a large disc) is written to `.psx/mcp/` and
  replaced by `result_file`, an artifact descriptor, a summary and an
  `artifact_read` next call. The artifact reader supports JSON pointers and pages
  and does not recursively spill its own results.
- `run` and `native` take `return_screen: true` to attach the final screen PNG as
  image content. `artifact_read` of a PNG attaches image content automatically.
- Timeouts: `run` keeps its `timeout` (default 120 s). Over MCP, `native`
  defaults to `timeout: 300` and stop with status `timeout`. Each call also
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
annotations back into psx is future work. Explicit rename/comment patches are supported below.

Runtime code is also supported. Give `--ram` a run directory or exactly 2 MiB
main-RAM image, with explicit candidate code ranges and function entry seeds:

```sh
psx ghidra decompile --ram RUN --range 80106E2C-80106E4C --entry 80106E2C --addr 80106E40
```

Ranges are end-exclusive; `--range` and `--entry` are repeatable. In MCP these
are `ranges: ["LO-HI"]` and `entries: ["ADDR"]`. Bare RAM accepts `--gp HEX`;
it also overrides a run's captured GP. Choose exactly one of `source`, `ram`, or
`analysis`. All query tools accept these selectors. Snapshot options require
`ram`; seed symbols require `source` or `ram`, rather than an existing handle.

The rest of RAM remains initialized data context, without being executable.
Snapshots are identified by content, mapping, seeds, settings and available
capture identity; changing code at the same address creates a separate program.
Reports/registers/frame/media identity are attached only if `report.json` hashes
match the captured RAM. Legacy directories without reports remain usable.
Query aliases in KUSEG/KSEG0/KSEG1 and the four RAM mirrors fold to canonical
`0x80000000..0x80200000`; results retain the requested address. Scratchpad, BIOS
and MMIO never fold into RAM. Only canonical KSEG0 is mapped in Ghidra: static
references using other aliases may remain unresolved.

Rename a function and add its evidence comment with a JSON patch:

```json
{"functions":[{"addr":"80106E2C","name":"snap_to_floor","comment":"Observed integer Y writer at 80106E40."}]}
```

```sh
psx ghidra annotate ANALYSIS --file patch.json
psx ghidra export-symbols ANALYSIS
```

MCP: `ghidra_annotate` takes `analysis` and `file`; `ghidra_export_symbols` takes
`analysis` and optional `out`. Patch addresses must be function entries, names
ASCII identifiers of at most 128 characters, comments at most 16 KiB. Either
field is optional; an empty comment clears it. User names take precedence over
generated names, with displacements reported. Colliding user/imported names fail
before editing. Name swaps work. Export requires unique names without whitespace.

Annotation publishes a new revision from a staged project copy, retains prior
projects and portable patch history, re-exports indexes and invalidates cached C.
Failure, timeout or cancellation leaves the published revision intact. GUI edits
still require explicit reanalysis; types and GUI annotation refresh remain future work.

Export returns a `.symbols.json` path for `run`, `loads`, `disasm` and `xrefs`.
Optional `--out FILE.symbols.json` chooses a destination; existing different
content is never overwritten. The default export is revision-specific and reusable.
Format 2 binds every name to an immutable input copy, input hash, mapping, and
hashed function body ranges. Keep that input copy with the export; moving the
bundle requires updating `input` to its new path. Names appear only within actual
bodies and only when **all body bytes** match the tool's current code image.
Native trace call labels and watch/read event names check live RAM (`body_match`);
replacement code at the same address receives no old name. Historical aggregates,
`loads` has only final RAM: they expose a `candidate_name`
with `check: "final_ram_only"`, never assert earlier snapshot residency.
Plain `.sym` files remain unverified; legacy EXE symbol reports retain their
local-byte checks and now reject changed source hashes when supplied. When used
as Ghidra naming seeds, legacy reports require the original entire EXE payload
to match the mapped input; format 2 seeds check function bodies.

Generic MIPS pseudocode is useful for ordinary game code; PS1 load-delay and
COP2/GTE semantics still need checking against instructions/runtime observations.
See [runtime validation](research/ghidra-runtime.md) and the remaining G3 scope in
[docs/ghidra-integration.md](docs/ghidra-integration.md).

## Native game template

The Bevy template supports optional `PsxMaterial::depth_cue` with configurable
reciprocal coefficients and far vertex colour, or `PsxDepthCue::from_distances`.
It uses camera depth per vertex before texture modulation and overrides camera
fog for that material; `None` preserves ordinary `DistanceFog`. See the template
[usage](templates/psx-bevy/psx-bevy/README.md#3d) and
[validation/example](research/template-depth-cue.md).

```bash
psx template create ../my-game   # standalone Bevy 0.20 workspace
psx template create . --region pal --fields-per-tick 2   # into the game folder; 25 Hz logic, like Croc PAL
psx template update --dry-run    # in a project: what a newer psx would change
```

The [workspace](templates/psx-bevy/README.md) is built on `psx-bevy/`, the
PS1 look, ticks, input, menus and launcher. It is the project's copy, meant
to be changed to suit the game (a port that needs the original's rendering
tricks makes them there), with each change listed in `docs/template.md`.
`template update` replaces its untouched files and writes `.psx-new` beside
edited ones. `game/` is the port (a demo to replace: one plugin per
feature, rules apart from presentation, level and tuning in RON files) and
`tools/import/` holds the importers. `template create` can write into the
game folder: it never overwrites files and keeps an existing `AGENTS.md`.
For a whole port, `psx port init` writes the workspace (see [Ports](#ports)).

[`psx-bevy`](templates/psx-bevy/psx-bevy/README.md) is idiomatic Bevy with a
PS1 look: `PsxMaterial` gives vertex snapping, affine textures, Gouraud
lighting, 15-bit dither, depth cue and the four blend modes. It also provides a
low-resolution `PsxView` target, 2D sprites, DualShock input, fixed 50/60 Hz
ticks, and a feature-gated developer MCP for stepping, input, screenshots and
paused component-field edits (`game_set_component`). `--headless --software`
renders through the GPU without a desktop; use screen screenshots in that mode.
`ControllerBindings` provides keyboard/gamepad rebinding while gameplay reads
logical `ControllerState`. The reusable `ControlsPlugin` opens with F1 or
Start+Select in the demo (`--controls` opens it at startup; `--bindings PATH`
selects its settings file). Draft edits apply only after a successful explicit
save; the menu pauses gameplay and gates input until physical release. Finished
games choose where to integrate it. See
[reuse/API](templates/psx-bevy/psx-bevy/README.md#rebinding-and-the-example-controls-menu)
and [validation](research/template-controls.md).
The original's files are converted by the project's importers (`tools/import`) into
ordinary formats such as PNG and glTF, which load like any Bevy asset.
When replacing `game/src/demo/`, migrate its tests too: `game/tests/player.rs`
and the player edit/step test in `game/tests/dev_mcp.rs` depend on the demo.
[game/AGENTS.md](templates/psx-bevy/game/AGENTS.md#project-layout) lists the
required changes and compilation check. Keep `dev-mcp` enabled across the
agent iteration cycle to reduce dependency rebuilds when switching features.

## Ports

A port is one workspace and one agent, which investigates the original with psx and writes
the port. `goal.toml` names the features the person wants; `catalogue/F-NNNN.md` describes
each feature of the game (id, title, area, the features it depends on, and a paragraph in game
terms). The person confirms the goal's list with `psx port confirm --yes`. `port next` gives
the one item due now, and `port status` every feature of the goal by stage. Verdicts from
psx's checks live in `.psx/verdicts/` and count only for the commit they judged. The design
is [docs/port-design.md](docs/port-design.md); `guide` topic `port` holds the procedure.
Mechanics are captured with `psx mechanic capture` on the native core, and their files are
defined in [docs/mechanics.md](docs/mechanics.md). Scenarios are recorded with `psx scenario
record` and checked for determinism with `psx scenario check`
([docs/scenarios.md](docs/scenarios.md)).

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

## Developing psx

`psx-oracle` (`crates/psx-oracle`, not published or exposed over MCP, x86_64 Linux only) checks the
native core against Mednafen 1.32.1, which is the ground truth for psx. Mednafen's sources are copied
into the crate with a few marked changes (its README lists them), and the crate is GPL like them; nothing
else links it. Run it from the project root:

- `cargo run --release -p psx-oracle -- run [disc|exe] --bios B --frames N [--out DIR] [--watch ADDR[:W]]
  [--replay R] [--dualshock]` writes `ram.bin`, `scratch.bin`, `vram.bin`, `spu.bin`, `screen.png` and
  `vram.png` for `psx ram-diff` / `psx vram-diff` against a native run with the same BIOS and replay.
  Frames are vblanks, as in `psx run`. The console region is the BIOS's. A `.chd` is unpacked once to
  CUE/BIN under `.psx/cache/discs/`. Without media it boots the BIOS with an empty drive.
- `cargo run --release -p psx-oracle -- lockstep --bios B [--steps N] [--cycles] [--fast] [--dump PREFIX]` runs
  the native core and Mednafen side by side from BIOS reset. Before each instruction it compares PC,
  the instruction, every register, the delayed load, COP0 and (after GTE instructions) the GTE, and it
  stops at the first divergence. It also compares the cycle count and reports where it changes;
  `--cycles` stops there. The core raises its own interrupts and lockstep compares the interrupt line.
  An I/O load value that still differs is taken from Mednafen
  and counted by register. RAM, the scratchpad, VRAM and SPU RAM are compared at every
  frame end where both are at the same instruction, and reported without stopping; `--dump` writes both
  sides' memory at the end. `--fast` runs frames at full speed and compares at each frame end (about 15M
  instructions/s, eight times faster); a frame that differs is rerun instruction by instruction from the
  last checkpoint to find the instruction. Use it for long replays.
- `cargo run --release -p psx-oracle -- suite` runs the six lockstep checks in
  `crates/psx-oracle/suite/suite.json` (BIOS, Crash, Croc, FF7, Resident Evil, Tomb Raider) with `--fast`;
  each must match with nothing adopted. Run it before a release; about 10 minutes.
- `tools/probes/gpu/compare.sh PSX PSX_ORACLE OUT_DIR` diffs `psx-gpu` against Mednafen on the GPU probe.

The native core's video, CPU, DMA, CD-ROM, GPU command, timer, controller port, SPU and MDEC timing are Mednafen's:
- scanlines, fields and vblank follow the GPU clock (49.76 Hz frames on a PAL console, 59.83 Hz on NTSC);
- instructions cost what Mednafen charges (instruction cache, load latency, bus wait states,
  multiply/divide and GTE latencies);
- DMA runs on Mednafen's 128-clock grid and holds the CPU as Mednafen does;
- the CD-ROM controller (`cdrom.rs`) is a port of Mednafen's `cdc.cpp`, with its command phases, seek times,
  sector pipeline, random delays (Mednafen's PRNG) and XA/CD-DA audio, and it clocks the SPU as Mednafen's does;
- GP0 words go through Mednafen's command FIFO and wait for its drawing-time estimate, so GPUSTAT's busy
  and ready bits and the GPU's DMA follow Mednafen's (`psx-gpu`'s `Gpu::timed`; tools that replay GP0
  words use the untimed `Gpu::new`, which runs each command as soon as it is complete);
- the timers (`timers.rs`, Mednafen's `timer.cpp`) catch up at their own events, register accesses and the
  GPU's line phases, and take dots, retraces and vblank from the video timing;
- the controller port (`sio0.rs`, Mednafen's `frontio.cpp`) shifts bytes a bit at a time through the pad
  (`pad.rs`, `gamepad.cpp`/`dualshock.cpp`) and memory cards (`memcard.rs`, `memcard.cpp`), with their DSR
  delays;
- the SPU (`spu.rs`) is a port of Mednafen's `spu.cpp` and `spu_reverb.inc`, sample for sample;
- the MDEC (`mdec.rs`) is a port of Mednafen's `mdec.cpp`: its FIFOs, decoding clocks and DMA output order.

The BIOS, and Crash Bandicoot booting from its disc, run cycle for cycle as Mednafen's
(`psx-oracle lockstep --disc`), raising every interrupt at the same instruction and reading the same value from every I/O register.

### Formatting

Code is rustfmt-formatted (`rustfmt.toml`: 120 columns). Run `cargo fmt --all` at the root and in
`templates/psx-bevy` before committing; the Format workflow
([.github/workflows/fmt.yml](.github/workflows/fmt.yml)) and the release checks fail otherwise.
After formatting `crates/psx-media` or `crates/psx-assets`, copy them to the template's `vendor/`
(a test checks the copies match).

### Releases

Pushing a version tag builds and publishes a GitHub Release
([.github/workflows/release.yml](.github/workflows/release.yml)): set `[workspace.package] version`
in `Cargo.toml`, commit, then `git tag v0.2.0 && git push origin v0.2.0`. The workflow checks the
tag matches the version, runs Clippy and the tests, builds `psx` (profile `dist`) for Linux
x64/arm64, macOS arm64/x64 and Windows x64, and attaches `psx-<target>.tar.gz`/`.zip` with
`.sha256` files and `SHA256SUMS`. `install.sh` and `install.ps1` download from the latest
release; rerun the workflow by hand (workflow_dispatch with the tag) to rebuild a release.

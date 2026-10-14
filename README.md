# psx

PlayStation reverse-engineering toolkit for agents and people. Sibling of
`../../megadrive/mdx`; see [PLAN.md](PLAN.md) for direction and status.

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
| `psx textures <run_dir> [--page X,Y,DEPTH --clut X,Y]` | Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT pair a `--gpu-frame` capture used, full and cropped to the UVs sampled, or one page by hand |
| Frame → Bevy | `psx textures` also writes `textures/scene.psxscene`; the template's `PsxSceneRoot` (and `cargo run -- --scene ...`) spawns it as sprites and meshes |
| `psx gpu-replay <stream.bin> -o vram.bin` / `psx vram-diff a.bin b.bin` | Replay a GP0 stream into `psx-gpu`; diff two VRAM dumps per pixel (per-cell counts, channel deltas, diff PNG). `tools/probes/gpu/compare.sh` diffs `psx-gpu` against Redux on the GPU probe EXE |
| `psx memcard <image.mcd>` | List the saves on a memory card image: product code, blocks, title, icon frames |
| LibCrypt discs | Put the redump `.sbi`/`.lsd` beside the `.cue`; `psx disc` reports it and the native core's GetlocP honours it |
| `psx sector <image> <lba> [--count N]` | Sector form, XA subheader, owning file, hex preview |
| `psx addr 0x80010000` | Segment, memory region and folded offset |
| `psx run <disc\|exe> --bios B --frames N [...]` | Bounded run in an isolated PCSX-Redux process, or on the native core with `--backend native`. The native core gives exact observations, native save states, `--trace` for windowed instruction traces with coverage and call summaries, `--memcard1/2 PATH|new` for memory cards (written cards are saved to the run directory, never to your file), `--gpu-frame N` to decode one frame's GPU commands (primitives, texture pages, CLUTs, VRAM before/after/wireframe), and `--pad dualshock` for analog games (replay events take `"sticks": [lx, ly, rx, ry]`; Redux gets its analog pad). Captures RAM, VRAM, screen, registers and an optional state. `--watch ADDR[:W]` reports CPU/DMA writers with PC and value; `--trace-cd` and `--trace-bios` log CD and BIOS activity; `--replay`, `--from-state`/`--save-state`; `--fast` uses the dynarec (no observation) |
| `psx loads <run_dir> --disc <cue>` | Turns a `--trace-cd` run into disc file → RAM spans, verified by content |
| `psx symbols <exe\|disc::/PATH>` | Name PsyQ library functions by signature (needs a local `external/psx_psyq_signatures`); Ghidra and `.sym` output. Pass the `.symbols.json` to `run`/`loads --symbols` to name PCs, checked against RAM residency |
| `psx template create DIR [--name N]` | Native Bevy game starter (see below) |
| `psx mcp [--jobs N]` | Every command above as an MCP tool over stdio (see below) |

Input images can be `.cue`, a raw `.bin` (single data track) or a cooked `.iso`.

Other crates:

- **`psx-gpu`:** a software PS1 GPU. It has VRAM, a streaming GP0/GP1 decoder and hardware pixel
  rules, and logs GP0 commands. It is meant for replaying and diffing captured
  draw streams and decoding VRAM, not for game code.
- **`psx-gte`:** a register-accurate GTE (COP2) with typed helpers and libgte-style math.

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
root** (the directory holding `bios/`, `media/`, `external/`): relative paths in tool
arguments resolve there and outputs go to its `.psx/`. With Claude Code:

```bash
cargo build --release -p psx
cd /path/to/psx          # project root
claude mcp add psx -- "$PWD/target/release/psx" mcp
```

Tools: `disc`, `ls`, `extract`, `exe`, `sector`, `addr`, `xa`, `memcard`, `textures`,
`run`, `native`, `loads`, `symbols`, `disasm`, `lockstep`, `template_create`,
`gpu_replay`, `vram_diff`.

- Arguments are the CLI's long flags with `_` for `-` (`trace_cd`, `gpu_frame`,
  `memcard1`, `from_state`); positional arguments are named fields (`image`, `source`,
  `run_dir`); repeatable flags are arrays (`watch`, `symbols`, `trace_range`). Unknown
  arguments are rejected.
- A tool's `structuredContent` is exactly the JSON the CLI prints for the same command
  (`tests/mcp_stdio.rs` checks this); errors are `{"error": {"code", "message"}}`
  with `isError: true`. A partial run or a lockstep divergence is a result, not an error.
- Emulation tools return paths (`run_dir`, `files`) rather than RAM, VRAM or traces.
  Any result over 256 KiB (e.g. `ls` of a large disc) is written to `.psx/mcp/` and
  replaced by `result_file` plus a summary of its small fields.
- `run` and `native` take `return_screen: true` to attach the final screen PNG as
  image content.
- Timeouts: `run` keeps its `timeout` (default 120 s). Over MCP, `native` and
  `lockstep` default to `timeout: 300` and stop with status `timeout`. Each call also
  has a hard cap (its `timeout` + 60 s, otherwise 15 min). A call that overruns the
  cap, or is cancelled, returns an error but its worker thread is not killed. Emulation,
  `symbols` and `loads` share `--jobs` slots (default 2).
- Logs go to stderr; stdout carries only the protocol.

## Native game template

```bash
psx template create ../my-game   # standalone Bevy 0.20 project
```

[`templates/psx-bevy`](templates/psx-bevy/README.md) is idiomatic Bevy with a
PS1 look: `PsxMaterial` gives vertex snapping, affine textures, Gouraud
lighting, 15-bit dither, depth cue and the four blend modes. It also provides a
low-resolution `PsxView` target, 2D sprites, DualShock input, fixed 50/60 Hz
ticks, and a feature-gated developer MCP for stepping, input and screenshots.

## Local data (never committed)

- `media/`: disc images
- `bios/`: BIOS dumps
- `.psx/`: run outputs

## Tests

```bash
cargo test
cargo test -p psx-media -- --ignored   # needs media/ discs
```

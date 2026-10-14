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
| `psx textures <run_dir> [--page X,Y,DEPTH --clut X,Y]` | Decode VRAM texture pages through their CLUTs into PNGs: every page/CLUT pair a `--gpu-frame` capture used, full and cropped to the UVs sampled, or one page by hand |
| Frame → Bevy | `psx textures` also writes `textures/scene.psxscene`; the template's `PsxSceneRoot` (and `cargo run -- --scene ...`) spawns it as sprites and meshes |
| `psx gpu-replay <stream.bin> -o vram.bin` / `psx vram-diff a.bin b.bin` | Replay a GP0 stream into `psx-gpu`; diff two VRAM dumps per pixel (per-cell counts, channel deltas, diff PNG). `tools/probes/gpu/compare.sh` diffs `psx-gpu` against Redux on the GPU probe EXE |
| `psx assets scan <image.cue>[::/PATH] \| <file> [--json]` | Find TIM, TMD, VAB (VH+VB, or a `.VH` with its `.VB`) and VAG data in disc files, game archives and RAM dumps (`ram.bin`: relocated TMDs resolve at 0x80000000) by signature + full validation. A bare disc image scans every file; `::/DIR/` a directory. Hits inside an archive with a Croc-style `.DIR` index name their entry |
| `psx assets extract <source> -o DIR [--kind K] [--offset HEX] [--textures SRC]` | TIM → PNG per CLUT, TMD → glTF (texture pages decoded from the TIMs in the same file or `--textures` sources, through a virtual VRAM), VAB/VAG → WAV per waveform (+ programs/tones JSON); the original bytes are written next to each. See [research/assets.md](research/assets.md) |
| `psx memcard <image.mcd>` | List the saves on a memory card image: product code, blocks, title, icon frames |
| LibCrypt discs | Put the redump `.sbi`/`.lsd` beside the `.cue`; `psx disc` reports it and the native core's GetlocP honours it |
| `psx sector <image> <lba> [--count N]` | Sector form, XA subheader, owning file, hex preview |
| `psx addr 0x80010000` | Segment, memory region and folded offset |
| `psx run <disc\|exe> --bios B --frames N [...]` | Bounded run in an isolated PCSX-Redux process, or on the native core with `--backend native`. The native core gives exact observations, native save states, `--trace` for windowed instruction traces with coverage and call summaries, `--memcard1/2 PATH|new` for memory cards (written cards are saved to the run directory, never to your file), `--gpu-frame N` to decode one frame's GPU commands (primitives, texture pages, CLUTs, VRAM before/after/wireframe), and `--pad dualshock` for analog games (replay events take `"sticks": [lx, ly, rx, ry]`; Redux gets its analog pad). Captures RAM, VRAM, screen, registers and an optional state. `--watch ADDR[:W]` reports CPU/DMA writers with PC and value; `--trace-cd` and `--trace-bios` log CD and BIOS activity; `--replay`, `--from-state`/`--save-state`; `--fast` uses the dynarec (no observation) |
| `psx loads <run_dir> --disc <cue>` | Turns a `--trace-cd` run into disc file → RAM spans, verified by content |
| `psx symbols <exe\|disc::/PATH>` | Name PsyQ library functions by signature (needs a local `external/psx_psyq_signatures`); Ghidra and `.sym` output. Pass the `.symbols.json` to `run`/`loads --symbols` to name PCs, checked against RAM residency |
| `psx template create DIR [--name N]` | Native Bevy game starter (see below) |

Input images can be `.cue`, a raw `.bin` (single data track) or a cooked `.iso`.

Other crates:

- **`psx-gpu`:** a software PS1 GPU. It has VRAM, a streaming GP0/GP1 decoder and hardware pixel
  rules, and logs GP0 commands. It is meant for replaying and diffing captured
  draw streams and decoding VRAM, not for game code.
- **`psx-gte`:** a register-accurate GTE (COP2) with typed helpers and libgte-style math.
- **`psx-assets`:** TIM/TMD/VAB/VAG parsing, SPU-ADPCM (shared with the native core's SPU),
  the signature scanner and TMD → glTF. `tim.rs`/`tmd.rs` are copied verbatim into the template.

Backend findings are in [research/backend-comparison.md](research/backend-comparison.md); a worked
provenance example (Croc's boot chain) is in [research/load-provenance.md](research/load-provenance.md).
`psx run` needs `external/pcsx-redux` built at the pinned commit (or `PSX_REDUX_BIN`).

## Native game template

```bash
psx template create ../my-game   # standalone Bevy 0.20 project
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
```

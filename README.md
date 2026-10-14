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

## Local data (never committed)

- `media/`: disc images
- `bios/`: BIOS dumps
- `.psx/`: run outputs

## Tests

```bash
cargo test
cargo test -p psx-media -- --ignored   # needs media/ discs
```

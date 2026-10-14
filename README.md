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
| `psx sector <image> <lba> [--count N]` | Sector form, XA subheader, owning file, hex preview |
| `psx addr 0x80010000` | Segment, memory region and folded offset |
| `psx template create DIR [--name N]` | Native Bevy game starter (see below) |

Input images can be `.cue`, a raw `.bin` (single data track) or a cooked `.iso`.

Other crates:

- **`psx-gpu`:** a software PS1 GPU. It has VRAM, a streaming GP0/GP1 decoder and hardware pixel
  rules, and logs GP0 commands. It is meant for replaying and diffing captured
  draw streams and decoding VRAM, not for game code.
- **`psx-gte`:** a register-accurate GTE (COP2) with typed helpers and libgte-style math.

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

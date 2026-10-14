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

Input images can be `.cue`, a raw `.bin` (single data track) or a cooked `.iso`.

## Local data (never committed)

- `media/`: disc images
- `bios/`: BIOS dumps
- `.psx/`: run outputs

## Tests

```bash
cargo test
cargo test -p psx-media -- --ignored   # needs media/ discs
```

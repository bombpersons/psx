# Asset formats: TIM, TMD, VAB/VAG (2026-10-14)

`crates/psx-assets` parses and converts the PsyQ standard asset formats;
`psx assets scan|extract` finds them on discs, inside archives and in RAM
dumps; the template's `TimLoader`/`TmdLoader` load the extracted originals into
Bevy. Format layouts are documented at the top of each source file.

## What is implemented

| Format | Parsing | Conversion |
|---|---|---|
| TIM | 4/8/16/24-bit, with or without CLUT, any number of palettes (CLUT rows), VRAM rects bounds-checked, block lengths must match | PNG per palette. 0x0000 → alpha 0, everything else opaque (`Cutout`); `SemiStp` gives STP texels alpha 128. 5→8-bit by bit replication |
| TMD | Header, object table, vertices, normals, polygon packets for every combination of lit/unlit (LGT), flat/Gouraud (IIP), tri/quad, textured (TME), gradation (GRD); semi-transparency (ABE) and double-sided (FCE) flags; UVs, CBA and TSB. The packet layout is derived from the mode/flag bits and checked against `ilen`; lines, sprites and mismatching packets are counted as `skipped`. FIXP (relocated) models resolve against a RAM base | glTF 2.0 (embedded buffer, one mesh per object, one primitive per page/CLUT/blend, `KHR_materials_unlit`, nearest samplers), texture pages decoded from a virtual VRAM built from TIMs |
| VAB / VH+VB | Header, 128 program slots, 16 tone slots per program (centre note, key range, ADSR, waveform), waveform size table. The VB is found right after the VH, after a u32 size (Croc), after padding (Mega Man 8 `.PAC`), or in a `.VB` next to a `.VH` | WAV per waveform at 44.1 kHz (the SPU rate at the tone's centre note) + JSON of programs/tones |
| VAG | `VAGp` header (big-endian size and rate, name) | WAV at the header rate |
| SPU-ADPCM | `adpcm::decode_block` is the decoder `psx-core`'s SPU now calls (behaviour unchanged, its tests pass) | loop points from START/REPEAT/END flags |

Validation that keeps the scanner honest:

- TIM: flags must have no reserved bits; each block's length must equal 12 + 2·w·h and fit VRAM. CLUT-less 4/8-bit TIMs are accepted only above 64 halfwords.
- TMD: flags 0/1, 1–4096 objects, aligned in-range tables, every vertex/normal index in range, at least one polygon, skipped packets ≤ 4× polygons.
- VB pairing: every waveform must be whole 16-byte blocks with filter ≤ 4, shift ≤ 12, flags ≤ 7, and END only in its last two blocks (encoders end with an END block plus a silent `flags = 7` block), the last one included. Without this, a VH followed by unrelated ADPCM data (Crash's NSF pages, RE RAM) was paired with garbage.

## Real discs (`psx assets scan <disc>`, every file)

| Game | TIM | TMD | VAB (with VB found) | Notes |
|---|---|---|---|---|
| Resident Evil | 2524 | 908 | 409 (328) | EMD/IVM/DOR/RDT files embed TIM+TMD+VAB |
| PaRappa the Rapper | 9000 | 269 | 36 (35) | `COMPOnn.INT` archives |
| Tekken 3 | 8661 | 0 | 51 (1) | `TEKKEN3.BNS` archive; VBs elsewhere |
| Ape Escape | 3887 | 0 | 0 | `KKIIDDZZ.DAT`, `DEMO/*.ALL` |
| Vagrant Story | 824 | 0 | 0 | |
| Final Fantasy VII (disc 1) | 387 | 0 | 0 | |
| WipEout | 98 | 0 | 1 (0) | loose `.TIM` files |
| Castlevania SOTN | 1 | 0 | 73 (VH + separate `.VB`, paired at extract) | |
| Mega Man 8 | 2 | 0 | 70 (70) | VB after padding inside `.PAC` |
| Croc | 0 | 0 | 32 (32) | inside `CROCFILE.1`, named by `CROCFILE.DIR` entry; `VH, u32 size, VB` |
| Tomb Raider | 0 | 0 | 17 (17) | level `.PSX` files |
| Crash Bandicoot | 0 | 0 | 42 (0) | NSF pages hold VHs; waveforms live in other chunks |
| R-Type Delta / Oddworld / Pandemonium | 0 | 0 | 30 (30) / 15 (12) / 39 (0) | |
| Metal Gear Solid / Ridge Racer | 5 / 7 | 0 | 0 / 1 (0) | |
| Spyro, Gran Turismo | 0 | 0 | 0 | custom/compressed formats |

No disc here contains a standalone VAG (`VAGp`) file; VAG decoding is
covered by unit tests only. Croc has no plain TIMs or TMDs: its graphics are
in its own (likely compressed) formats. Formats that games compress or
replace are invisible to a signature scanner by design.

Checked by eye / ear (outputs under the main checkout's `.psx/assets/`):

- TIM: WipEout `LOAD1.TIM` (16-bit 320×240 loading screen), Ape Escape
  `DEMO/TITLE.ALL` (8/4-bit, logo and ring art), Resident Evil `EM1020.EMD`
  (8-bit 256×256 with two CLUTs: each palette colours one half of the sheet,
  matching the TMD's two TSB/CBA pairs).
- TMD rendered in the template (`--model` viewer, dev-MCP screenshots in
  `.psx/assets/screenshots/`): RE `ITEM_M2/I00V.IVM` (Beretta, lit Gouraud
  textured `0x34` packets), RE `ITEM_M1/DOOR00.DOR` (12-object door), PaRappa
  `S2/COMPO02.INT` @0x139638 (17-object car: Gouraud colour + textured
  parts, textures from the archive's 268 TIMs) and @0x145FA0 (road). With
  `--cull` the Beretta (flag 0, all single-sided) renders identically, which
  confirms the screen-clockwise front-face winding.
- glTF: the Beretta's `.gltf` loads through Bevy's glTF loader (1488
  vertices, position/normal/UV/colour) and renders the same as the TMD path.
- WAV: Croc `AMBIBIRD.BIN` (7 bird calls), SOTN `SD_ZKBO0.VH`+`.VB`, Mega
  Man 8 `PBGM00.PAC`, Tomb Raider `LEVEL1.PSX`, a VAB in RE RAM: no clipped
  samples; tonal waveforms have first-difference/RMS ratios of 0.04–0.5,
  percussion and noise 0.8–1.8 (white noise is 1.41). Not listened to.

RAM dumps: the BIOS shell's RAM (`.psx/runs/bios_f350/ram.bin`) holds 17
TIMs and a VAB; an RE run at frame 2400 (title, before gameplay) holds 3 TIMs
and a complete VAB (VB about 61 KiB after the VH). No dump contained a TMD, so the
FIXP path is unit-tested only. Several BIOS shell TIMs decode as two
half-height copies side by side (even rows left, odd right): the header says
248×130 and the decoder follows it; how the shell draws them was not checked.

## Open issues

- TMD objects are placed as stored; skeletons/animations (RE EMD, PaRappa)
  are game-specific and not applied.
- Texture pages come from TIM headers' VRAM positions. Games that upload
  TIMs elsewhere at run time need `--textures` from a VRAM-matching source
  (or `psx textures` on a run's `vram.bin`).
- Lines and sprites in TMDs are skipped, not converted.
- VAB waveforms are written at 44.1 kHz (their pitch at the centre note);
  per-tone pitch and ADSR are reported, not applied.
- `units_per_metre` defaults to 64 (as scene captures); RE models are in
  millimetres and look huge at that scale.

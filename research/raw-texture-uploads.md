# Raw VRAM uploads and King's Field RTIM (2026-10-15)

Implemented the texture half of the preprocessed-model/raw-upload task using
the external parser boundary. `psx-assets::upload::UploadDocument` validates
ordered rectangle/word uploads, source spans and byte lengths. `assets scan`
lists these separately from TIMs; extraction retains original bytes, parser
JSON, an upload manifest and raw little-endian word files. `--texture-parser`
adapts `--textures` inputs before the ordinary model page/CLUT decoding and glTF
export. No game-specific format detection or pixel-depth guessing was added to
the standard scanner.

## Shared behavior

An asset parser may emit version-1 `uploads` instead of one standard payload.
Each record names its input byte span, `[x,y,width_words,height]` in VRAM word
coordinates, and base64 little-endian u16 words. Rectangles must fit VRAM without
wrapping and data lengths must match exactly. The document is fully validated
before applying it. Pixel depth and palette interpretation come from the model's
texture references; raw rectangles alone do not identify either.

Texture sources load in command order, entries in archive index order, and
uploads in document order. Later writes overwrite earlier ones, including
palettes. TIM and raw-upload entries can be interleaved without changing that
order, even if an external archive's entry offsets are unsorted. Written-word
coverage is retained, so missing pixels/palettes are not reported as present.
MCP supports the same flags (`texture_parser`, `texture_parser_args`) and
supervised timeout/cancellation as the existing parser hooks.

See [the protocol](../docs/external-parsers.md#raw-vram-uploads) for the exact
document shape, filters, output fields and limits.

## Worked parser and evidence

[`tools/parsers/kings_field_rtim.py`](../tools/parsers/kings_field_rtim.py) is a
stdlib-only external example for the known PAL SCES-00510 layout. It accepts one
raw archive entry, or runs per entry through the built-in archive index. Each
upload is two identical LE u16 rectangles followed by width×height u16 words.
The existing study's `research/FORMAT_NOTES.md`, `tools/extract_world.py` and
saved GAME.EXE `0x800173EC` decompilation describe the loader. The example uses
the established duplicated-rectangle rules; no new decompilation was needed.

The loader stops on mismatched rectangle copies, zero dimensions, or matching
rectangles with x=-1. These are game-specific stream terminators, and the
sector tail can contain unrelated nonzero bytes. The example records the stop
reason, consumed length and trailing length on stderr (captured in provenance).
A valid-looking upload with truncated data or a rectangle outside VRAM is an
error. At least one valid upload is required. Standard TIM scanning never adopts
these signatureless termination rules.

Local checks found 75 RTIM entries: three empty and 72 parsed streams, containing
1,396 uploads. Seventy-one nonempty streams stop on the usual `FFFF` marker;
entry 18 stops on mismatched rectangle copies after two uploads. FDAT[47] supplies
500 common uploads. RTIM[0] supplies 66 opening uploads, beginning with palette
`[448,510,16,1]` and image words `[384,256,16,64]`.

## Runtime and image validation

Reconstructing common then opening uploads and comparing the final words in
all 66 opening rectangles to `research/reference/spawn/vram.bin` gives **86,032
word comparisons and zero differences**. This reproduces the existing study's
runtime check, including palette words and STP bits. Repeated coordinates, if
any, are compared in their final uploaded state.

The external RTMD model parser and the two upload sources produce a textured
glTF with 185 objects and 5,257 polygons. Its 20 page/CLUT keys yield 17 PNGs with
nonzero written coverage. All 17 PNGs match an independent Python reconstruction
and indexed-pixel/CLUT decoder, byte for byte (including transparent texels).

Three keys have no coverage in these selected banks:

| Page | Depth | CLUT |
|---|---:|---|
| [320,0] | 4 | [576,277] |
| [896,0] | 4 | [576,267] |
| [960,0] | 4 | [576,268] |

These remain reported as uncovered; selecting other banks or reconstructing
their runtime loading belongs to the game study. The full RTMD bank can contain
models not used by the opening area.

Of 945,664 covered texel lookups across the 17 pages, 12,977 resolve to a
different 16-bit colour in the later spawn capture. They are confined to two
page/CLUT pairs: `[320,256]` / `[384,498]` (11,136) and `[384,256]` / `[448,481]`
(1,841), both 4-bit. The remaining 15 pages' covered lookups agree. This does not
contradict the exact opening-upload comparison: a page also references common
texture words, and the live snapshot can differ from disc data outside those
opening rectangles. The origin of those differences has not been investigated;
this change exports the selected disc uploads, rather than substituting runtime
pixels or claiming every texture matches the later capture.

## Reproduction

Build the toolkit and model example, then extract the three selected banks:

```sh
cargo build --release -p psx -p psx-assets --bin psx --example kings_field_parser
target/release/psx assets extract "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/FDAT.T" --entry 47 -o .psx/raw-texture-uploads/raw
target/release/psx assets extract "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/RTIM.T" --entry 0 -o .psx/raw-texture-uploads/raw
target/release/psx assets extract "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/RTMD.T" --entry 0 -o .psx/raw-texture-uploads/raw
```

Use those exact raw entry files as texture/model inputs. For the local validation
they were also copied to `common.dat`, `opening.dat` and `model.dat` respectively:

```sh
target/release/psx assets scan .psx/raw-texture-uploads/opening.dat \
  --asset-parser ./tools/parsers/kings_field_rtim.py --json
target/release/psx assets extract .psx/raw-texture-uploads/opening.dat \
  --asset-parser ./tools/parsers/kings_field_rtim.py -o .psx/raw-texture-uploads/uploads
target/release/psx assets extract .psx/raw-texture-uploads/model.dat \
  --model-parser ./target/release/examples/kings_field_parser \
  --textures .psx/raw-texture-uploads/common.dat \
  --textures .psx/raw-texture-uploads/opening.dat \
  --texture-parser ./tools/parsers/kings_field_rtim.py \
  -o .psx/raw-texture-uploads/textured
```

Do not combine every RTIM bank as one texture source for a single level: banks
are alternatives and later writes may replace the chosen level's textures.
The local validation reports are under ignored `.psx/raw-texture-uploads/`:
`all-RTIM-scan.json`, `common-scan.json`, `opening-scan.json`,
`opening-extract.json`, `textured-model.json`, and `validation.json`. No retail
assets are added to the repository or to parser templates.

Authored tests cover rectangle/span/base64/version failures, overwrite order,
STP preservation, indexed lookup, coverage, mixed TIM/raw entry ordering,
parser truncation, raw extraction provenance and CLI/MCP parity. An authored
textured triangle proves that upload documents reach the existing PNG/glTF
path. `cargo test -p psx -p psx-assets --all-targets --offline` passes 77 tests;
the two environment-dependent tests remain ignored. Clippy and release builds
pass. The shared template format files are unchanged by this upload work.

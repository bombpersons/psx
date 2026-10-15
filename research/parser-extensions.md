# External parsers and starters (2026-10-15)

The extension boundary is implemented for archive indexes, model geometry,
asset wrappers (including unusual sound-bank packing) and STR frame wrappers.
The toolkit supplies source resolution, bounded entry views, protocol validation,
provenance and its existing PNG/glTF/WAV/MDEC exporters. Explicitly selected
executables supply game-specific parsing. No game registry or automatic code
execution is introduced. Existing small built-ins remain compatible; emulator
hardware behavior and game-derived hardware regression fixes stay in core.

`psx template parser DIR --kind archive|model|asset|video` writes `parser.py`,
an authored `sample.bin`, a README and the versioned protocol documentation.
The Python starters need only the standard library; executables in other
languages use the same stdin-bytes/stdout-JSON contracts. Model and archive
starters compose: the sample archive holds an empty entry followed by the model
sample. The asset sample wraps a real 2×2 TIM; the video sample wraps a real
16×16 BS v2 frame. These are invented teaching containers, with no retail data.
MCP exposes `template_parser_create`; all hooks are shared by CLI and MCP.

Contracts, flags, limits, context and examples are in
[external-parsers.md](../docs/external-parsers.md).

## Worked King's Field RTMD parser

The example lives in
[`kings_field_parser.rs`](../crates/psx-assets/examples/kings_field_parser.rs)
with its decoder in
[`kings_field/parser.rs`](../crates/psx-assets/examples/kings_field/parser.rs).
It is an external executable, not a built-in game format selection. Its known
profile accepts headers `{0, 0x10/0x12, object_count}`, sets standard TMD
`{0x41, 0, object_count}`, and divides actual vertex/normal references by eight
(SVECTOR byte offsets). UVs, colours and padding are unchanged. A public shared
`polygon_reference_range` helper exposes the existing packet layout math;
standard TMD parsing remains strict and validates the normalized result.

Unknown layouts, nonintegral references, malformed tables and out-of-range
indices are errors. Authored tests cover lit/unlit triangles, a quad, flat and
Gouraud normals, per-corner colours and deliberately nondivisible padding.
Other games are not claimed compatible merely because their data looks similar.

All nine banks in the local PAL SCES-00510 `/CD/COM/RTMD.T` pass through the
external parser and glTF exporter:

| Bank | Objects | Vertices | Polygons | Skipped |
|---|---:|---:|---:|---:|
| 0 | 185 | 4,761 | 5,257 | 0 |
| 1 | 120 | 2,728 | 2,709 | 0 |
| 2 | 159 | 3,890 | 3,921 | 0 |
| 3 | 190 | 4,871 | 5,008 | 0 |
| 4 | 114 | 2,606 | 2,709 | 0 |
| 5 | 81 | 1,806 | 1,809 | 0 |
| 6 | 104 | 2,217 | 2,154 | 0 |
| 7 | 38 | 748 | 893 | 0 |
| 8 | 28 | 528 | 413 | 0 |

Bank 0's original 217,088 bytes equal the existing spawn RAM capture at physical
`0x130A90` (pointer at `0x190240`). Normalized bytes equal the study's
`research/stage0-normalized.tmd` byte for byte, SHA-256
`9ff79a0fee8bd3102975ddf0704f5a27dfd07bbeaea5b6af1b2b58e17bbe2a15`.
The external JSON model export equals the standard normalized TMD export's
embedded geometry buffer, buffer views, accessors and meshes, ignoring mesh
names derived from the source filename. This checks positions, normals, UVs,
colours and triangle topology through both paths. RTIM texture uploads are not
decoded by this model example. The subsequent external RTIM example and shared
upload support supply texture sources; see [raw-texture-uploads.md](raw-texture-uploads.md)
for coverage and runtime comparison limits.

Reproduce from the toolkit root with the local study present:

```sh
cargo build --release -p psx -p psx-assets --bin psx --example kings_field_parser
target/release/psx assets scan \
  "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/RTMD.T" \
  --model-parser ./target/release/examples/kings_field_parser --json
target/release/psx assets extract \
  "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/RTMD.T" \
  --model-parser ./target/release/examples/kings_field_parser \
  -o .psx/parser-extensions/converted
```

Ignored local artifacts: `.psx/parser-extensions/RTMD-scan.json`,
`RTMD-extract.json`, `validation.json`, `stage0-normalized.tmd`, and the converted
glTFs. The game source, reference TMD and RAM remain in the sibling King's Field
study; no retail assets are included in the parser examples or starters.

## Verification

`cargo test -p psx -p psx-assets --all-targets --offline` passes: 73 tests, including
the example tests. Two existing environment-dependent Ghidra and game-state tests
remain ignored. Tests cover geometry/texture constraints, external index bounds and
overlap, malformed protocol output, literal executable arguments, timeout,
stdout/stderr limits and descendant cancellation. Generated samples pass their
contracts and refuse nonempty destinations.

Real CLI/MCP tests exercise generated archive → model → glTF, wrapper → TIM →
PNG and wrapped STR → BS → hardware MDEC → PNG; a wrapped authored VAB also
converts to a 28-sample WAV at 44.1 kHz. The authored movie's decoded
pixels are all neutral grey (128), and it is refused without the adapter.
MCP cancellation stops a selected parser and its ordinary descendants before
conversion writes outputs. The shared template TMD copy stays byte-identical.
Release builds and Clippy pass; template formatting and Clippy with `dev-mcp`
also passed after the shared TMD helper change.

The archive survey's initial reader estimates remain historical planning data.
For future niche formats, use an external parser first and promote shared code
only when actual reuse and validation justify maintaining a built-in decoder.

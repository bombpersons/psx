# External parsers (protocol 1)

Keep standard PS1 formats and reusable exporters in the toolkit. For a game-specific format, write a small executable implementing one of these contracts; no rebuild or plugin registration is needed. Parsers can be Python scripts, Rust binaries or any other executable. Existing built-in DOT-T/Croc indexes, VAB body handling and FF7 frame prefix handling remain available.

Generate a working Python starter, authored sample and a copy of this document:

```sh
psx template parser parsers/my-model --kind model
psx template parser parsers/my-archive --kind archive
psx template parser parsers/my-wrapper --kind asset
psx template parser parsers/my-frame --kind video
```

The destination must be new or empty. Python 3 is needed to run these starters. Other languages can use the same protocol. The sample formats (TRI1, ARC1, WRAP and VBS1) are teaching fixtures, not claims about real games. The archive sample contains the model sample and an empty entry, so the two starters can be chained.

```sh
psx assets scan parsers/my-archive/sample.bin \
  --archive-parser ./parsers/my-archive/parser.py \
  --model-parser ./parsers/my-model/parser.py --json
psx assets extract parsers/my-archive/sample.bin \
  --archive-parser ./parsers/my-archive/parser.py \
  --model-parser ./parsers/my-model/parser.py -o converted
psx assets extract packed-bank.bin --asset-parser ./unpack-bank -o sound
psx str disc.cue::/MOVIE.STR --frame-parser ./unwrap-frame -o movie
```

Pass arguments with repeated `--archive-parser-arg=VALUE`, `--model-parser-arg=VALUE`, `--asset-parser-arg=VALUE` or `--frame-parser-arg=VALUE`. The executable is launched directly; arguments are literal, with no shell expansion. Use `./parser.py` or an absolute path for an executable outside PATH. A parser is explicitly selected; there is no automatic execution based on file contents. It runs with the caller's permissions. Supervision is not a sandbox.

MCP offers `template_parser_create` with `dest` and `kind`. `assets_scan` / `assets_extract` accept `archive_parser`, `archive_parser_args`, `model_parser`, `model_parser_args`, `asset_parser`, `asset_parser_args`. `str` accepts `frame_parser`, `frame_parser_args`. Argument lists are arrays of strings. All accept `parser_timeout` (default 30 seconds per invocation, range 1–300).

## Transport and supervision

Read the complete binary input from stdin, then write exactly one UTF-8 JSON document to stdout and exit zero. Send diagnostics to stderr. Nonzero exit, invalid JSON, unknown fields, unsupported version, invalid ranges or invalid geometry fail the command with a diagnostic. Input is capped at 512 MiB, JSON stdout at 16 MiB and stderr at 64 KiB per invocation. Unix process groups are stopped on timeout, cancellation or completion, including ordinary descendants. A selected parser failure does not silently fall back to a guessed built-in format. MCP cancellation interrupts parser supervision.

Every invocation receives `PSX_PARSER_PROTOCOL=1`, `PSX_PARSER_KIND` (`archive`, `model`, `asset`, `video`) and `PSX_SOURCE_FILE` (host path or disc file label). Model/asset parsers also receive `PSX_ENTRY_INDEX` (empty for a whole file), `PSX_ENTRY_OFFSET` (decimal source byte offset). Video parsers receive `PSX_FRAME_INDEX` (1-based selected movie index), `PSX_STR_FRAME` (original frame number), `PSX_WIDTH`, `PSX_HEIGHT`. These values describe the source; stdout is still authoritative for the parsed data.

Results record executable, literal arguments, timeout, protocol version, input/output SHA-256 and stderr. Model/asset extraction retains original source bytes and parser JSON; wrapper adapters additionally write the normalized standard payload. Archive results retain the validated index and its parser provenance. Source offsets always refer to the original file, not transformed data. Video extraction writes normalized `.bs` frames and provenance in `frames.json`.

## Archive index

Invoked once per input file. Return byte ranges into the unchanged stdin bytes:

```json
{"version":1,"entries":[{"offset":24,"size":0,"name":"empty"},{"offset":24,"size":32,"name":"model"}]}
```

Entries have implicit zero-based indices in returned order. `name` is optional and is only metadata, never an output path. At most 65,536 entries are accepted; spans must be in bounds, nonempty spans cannot overlap, and empty entries are retained. Entries need not be sorted by offset. `--entry N` extracts the exact original span. `--archive-parser` replaces built-in archive detection and conflicts with `--archive`. Model/asset parsers can be applied to each nonempty indexed entry. Compression belongs in the per-entry asset/model parser; archive indexes do not invent transformed offsets or recursively dispatch programs.

## Model geometry

Invoked once per whole file, or per nonempty archive entry. Emit geometry, not TMD packets:

```json
{"version":1,"objects":[{"vertices":[[0,0,0],[100,0,0],[0,100,0]],"polygons":[{"vertices":[0,1,2],"colors":[[255,128,64]]}]}]}
```

Coordinates are signed i16 PS1 units (Y down, Z forward); the existing exporters apply their usual coordinate conversion. Each object contains `vertices`, optional `normals` (signed i16 XYZ), and `polygons`. Indices are unsigned u16 and local to that object. There must be 1–4096 objects and at least one polygon overall; an object has at most 65,536 vertices, normals and polygons. Faces are triangles or quads, using the toolkit's TMD corner order (quad split 0–1–2, 1–3–2).

A polygon contains `vertices`, optional `normals` (zero, one flat normal, or one per corner), optional `colors` (zero, one flat RGB or one per corner; unsigned bytes), optional `uvs` (one unsigned-byte UV pair per corner when textured), optional `texture`, and optional booleans `semi_transparent`, `double_sided`, `unmodulated` (all false by default). Untextured faces with no colors become white. Indices and array lengths are validated before export.

`texture` is `{"page":[X,Y],"depth":4,"clut":[CX,CY]}`: VRAM word coordinates, page X a multiple of 64 in 0–960, page Y 0 or 256, depth 4/8/16. Indexed textures require a CLUT (X a multiple of 16 in 0–1008, Y 0–511); 16-bit textures omit it. UVs and `unmodulated` require a texture. Use the normal `--textures` TIM/VRAM options for glTF material resolution. The protocol currently has no animations, skinning or arbitrary external texture paths.

The worked King's Field decoder is `crates/psx-assets/examples/kings_field_parser.rs`:

```sh
cargo build -p psx-assets --example kings_field_parser
psx assets extract 'game.cue::/CD/COM/RTMD.T' \
  --model-parser ./target/debug/examples/kings_field_parser -o geometry
```

It recognizes the known RTMD headers and normalizes SVECTOR byte references. Its optional standalone `--tmd-out PATH` writes normalized TMD bytes for comparison. This example does not decode RTIM textures.

## Asset wrapper and video frame

Both emit one standard payload as base64:

```json
{"version":1,"data_base64":"..."}
```

`--asset-parser` is invoked per whole file or nonempty archive entry. Return one ordinary TIM, TMD, VAB or VAG starting at byte zero; the existing parser/exporter handles PNG/glTF/WAV. Use this for prefixes, decompression or arranging a packed VH/VB body into a standard contiguous VAB. It conflicts with `--model-parser`. Raw `--entry` extraction conflicts with both transformations.

`--frame-parser` receives one reassembled STR frame, including any game-specific wrapper; return a BS v1/v2/v3 frame starting at byte zero. The standard Huffman and MDEC stages produce PNG/Y4M. Width and height still come from the STR sector header. The hook does not replace sector demux, XA decoding or hardware MDEC, so a different sector layout needs a separate preprocessing step. The generated video sample is a wrapped 16×16 neutral-grey BS frame, not a disc image.

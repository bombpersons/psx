# External parsers (protocol 1)

Keep standard PS1 formats and reusable exporters in the toolkit. For a game-specific format, write a small executable implementing one of these contracts; no rebuild or plugin registration is needed. Parsers can be Python scripts, Rust binaries or any other executable. Existing built-in DOT-T/Croc indexes, VAB body handling and FF7 frame prefix handling remain available.

`crates/psx/tests/parsers/` has one small working parser of each kind (Python 3, standard library only), which psx's tests run against authored samples; the sample formats (TRI1, ARC1, WRAP and VBS1) are test fixtures, not claims about real games.

```sh
psx assets scan archive.bin --archive-parser ./archive.py --model-parser ./model.py --json
psx assets extract archive.bin --archive-parser ./archive.py --model-parser ./model.py -o converted
psx assets extract packed-bank.bin --asset-parser ./unpack-bank -o sound
psx str disc.cue::/MOVIE.STR --frame-parser ./unwrap-frame -o movie
```

Pass arguments with repeated `--archive-parser-arg=VALUE`, `--model-parser-arg=VALUE`, `--asset-parser-arg=VALUE` or `--frame-parser-arg=VALUE`. The executable is launched directly; arguments are literal, with no shell expansion. Use `./parser.py` or an absolute path for an executable outside PATH. A parser is explicitly selected; there is no automatic execution based on file contents. It runs with the caller's permissions. Supervision is not a sandbox.

Over MCP, `assets_scan` / `assets_extract` accept `archive_parser`, `archive_parser_args`, `model_parser`, `model_parser_args`, `asset_parser`, `asset_parser_args`. Extraction also accepts `texture_parser`, `texture_parser_args` for its `textures` sources. `str` accepts `frame_parser`, `frame_parser_args`. Argument lists are arrays of strings. All accept `parser_timeout` (default 30 seconds per invocation, range 1–300).

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

`texture` is `{"page":[X,Y],"depth":4,"clut":[CX,CY]}`: VRAM word coordinates, page X a multiple of 64 in 0–960, page Y 0 or 256, depth 4/8/16. Indexed textures require a CLUT (X a multiple of 16 in 0–1008, Y 0–511); 16-bit textures omit it. UVs and `unmodulated` require a texture. Use `--textures` TIM sources, or `--textures` with `--texture-parser` for raw uploads, to supply glTF texture pages. The protocol currently has no animations, skinning or arbitrary external texture paths.

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

`--asset-parser` is invoked per whole file or nonempty archive entry. Return one ordinary TIM, TMD, VAB or VAG starting at byte zero; the existing parser/exporter handles PNG/glTF/WAV. Use this for prefixes, decompression or arranging a packed VH/VB body into a standard contiguous VAB. Alternatively return a raw upload document as described below. It conflicts with `--model-parser`. Raw `--entry` extraction conflicts with transformations.

`--frame-parser` receives one reassembled STR frame, including any game-specific wrapper; return a BS v1/v2/v3 frame starting at byte zero. The standard Huffman and MDEC stages produce PNG/Y4M. Width and height still come from the STR sector header. The hook does not replace sector demux, XA decoding or hardware MDEC, so a different sector layout needs a separate preprocessing step. The generated video sample is a wrapped 16×16 neutral-grey BS frame, not a disc image.

## Raw VRAM uploads

An asset parser can return this alternative document instead of `data_base64`:

```json
{"version":1,"uploads":[{"source_offset":0,"source_size":18,"rect":[640,256,1,1],"data_base64":"ERE="}]}
```

Each upload contains `rect` (`[x,y,width_words,height]`), exactly `width_words*height*2` bytes of row-major little-endian u16 data, and the original `source_offset` / `source_size` that produced it (relative to parser stdin). Source spans must be nonempty and in bounds; decoded size may differ for compressed input. There must be 1–65,536 uploads. Rectangles must fit the 1024×512-word VRAM without wrapping and have nonzero dimensions. Uploads apply in array order; later writes can overwrite earlier words. Source spans can overlap if a format reuses data.

No bit depth or palette role is inferred. Raw words may represent packed indexed pixels, CLUT colours, direct-colour pixels or other data. Their interpretation comes from the consuming model's page/CLUT references. Standard TIM detection remains unchanged.

`assets scan --asset-parser` lists these in `files[].upload_streams` and `upload_totals`, separately from standard hits/totals. `assets extract --asset-parser` returns one `kind: "vram_uploads"` record per input stream, retaining original bytes, parser JSON, an `.uploads.json` manifest and individual little-endian `.words.bin` files. Every upload has its original parent-file offset and rectangle; the enclosing record retains archive entry provenance. `--offset` selects the stream's start; standard `--kind` filters exclude raw streams.

To texture models directly, use `assets extract MODEL --model-parser EXEC --textures BANK --texture-parser EXEC`. The texture parser implements the asset contract and receives `PSX_PARSER_KIND=asset`, with the usual file/entry context. It can return a standard payload or an upload document. Pass arguments with repeated `--texture-parser-arg=VALUE`; `parser_timeout` and cancellation apply equally. `--texture-parser` requires `--textures` and applies to every texture source. Inputs load in command order, archive entries in index order, then the primary model source's own texture data. Texture sources do not inherit the primary source's model/archive parser selections. Extract the desired banks first when an archive contains alternative level or animation texture sets.

Model extraction records raw upload counts and `texture_sources` provenance. The worked `tools/parsers/kings_field_rtim.py` handles King's Field RTIM and common FDAT[47] streams; the repository's `research/raw-texture-uploads.md` records validation and commands.

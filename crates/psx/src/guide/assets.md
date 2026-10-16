Use ls with filter/offset/limit to orient yourself, then assets_scan on a narrow
source or filename filter. Scan totals cover the selected scope; page and coverage
describe returned rows and what was actually inspected. No standard TIM/TMD/VAB/VAG
hits does not prove that a game has no textures, models or audio.

assets_extract converts validated hits to PNG, glTF or WAV and retains source
bytes/provenance. Select kinds, an offset or an archive entry when appropriate.
Extraction returns paged results (limit/report_offset), or brief:true summary
counts. The complete content-bound manifest and next_call let artifact_read
continue without repeating conversion. Nested output lists are bounded too;
inspect the manifest with a JSON pointer for omitted rows. Narrow source/kinds
to avoid converting unnecessary assets; originals and provenance are retained.

Custom containers, compression and models need format evidence. Generate a
working scaffold with template_parser_create: archive parsers index unchanged
source spans; asset parsers unwrap/decompress standard payloads or raw VRAM
uploads; model parsers produce validated geometry. Parser flags have separate
roles; consult the generated protocol rather than copying internal Rust structs
into JSON. Inspect errors and stderr; do not silently fall back after a parser
failure. Retain original offsets, hashes and parser provenance.

run with gpu_frame captures an exact frame; gpu_next_draw searches completed
frames containing draws within its bounded window. Fills qualify, so a clear or
logo may be found before gameplay. Inspect gpu_frame.content, draw_commands,
textures and the image. gpu_draw_filter:primitives skips fill-only frames;
gpu_draw_filter:textured requires textured polygons/rectangles. Both filters
require gpu_next_draw and remain within its search/run budget.
textures decodes the captured frame's resident VRAM and creates a .psxscene for
reference. That capture is screen-space drawing evidence, not recovered world
geometry or a complete level. Raw streamed-file extraction is marked raw_2352;
do not interpret sector framing as ordinary file payload.

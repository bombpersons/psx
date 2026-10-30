Use ls with filter/offset/limit to orient yourself, then assets_scan on a narrow
source or filename filter. Scan totals cover the selected scope; page and coverage
describe returned rows and what was actually inspected. No standard TIM/TMD/VAB/VAG
hits does not prove that a game has no textures, models or audio.
Host directories are expanded recursively in sorted path order, without following
symlinks. Each file retains its provenance and sibling .DIR/.VB lookup. Use the
same source syntax for extraction and additional texture sources.

assets_extract converts validated hits to PNG, glTF or WAV and retains source
bytes/provenance. Select kinds, an offset or an archive entry when appropriate.
Extraction returns paged results (limit/report_offset), or brief:true summary
counts. The complete content-bound manifest and next_call let artifact_read
continue without repeating conversion. Nested output lists are bounded too;
inspect the manifest with a JSON pointer for omitted rows. Narrow source/kinds
to avoid converting unnecessary assets; originals and provenance are retained.
TMD details and summary.skipped_primitives report omitted geometry, with a warning
when conversion skips primitive types or packet layouts. Runtime FIXP size:0 means
the model uses relocated pointers, not that its geometry is empty. Gs-style
preprocessed primitive runs need an explicit model/asset parser; the standard
scanner does not infer their packet lengths or guarantee discovery.

Sounds: VAB/VAG extraction reports adpcm_sha256 per waveform. run with
trace_spu logs which voice keyed on which sample when (spu.jsonl), with the
same hash as sample_sha256, the key-on PC and the sample's main RAM source when
its upload was traced. Match the two to name the bank and waveform behind a
gameplay sound; banks often share waveforms, so one hash may match several.
Key-ons usually come from one sound-driver routine; trace the request (watch
the driver's queue) to find the gameplay code that asked for a sound.

Archives: validated DOT-T sector archives and Croc sibling .DIR indexes are
recognised automatically; archive:dot-t forces validation and archive:none
disables indexing. files[].archive lists every zero-based entry, including empty
ones; hits keep the parent-file offset plus entry index and relative offset.
assets_extract entry:N writes one complete raw entry without interpreting it
(one source file; not with offset, kinds or textures). ram_base overrides RAM
address mapping for dumps. TMD textures come from this file or extra textures
sources; texture_parser adapts those sources first.

Custom containers, compression and models need format evidence. An external
parser (docs/external-parsers.md) is any executable: archive parsers index unchanged
source spans; asset parsers unwrap/decompress standard payloads or raw VRAM
uploads; model parsers produce validated geometry. Parser flags have separate
roles; consult the protocol rather than copying internal Rust structs
into JSON. Inspect errors and stderr; do not silently fall back after a parser
failure. Retain original offsets, hashes and parser provenance.

run with gpu_frame captures an exact frame; gpu_next_draw searches completed
frames containing draws within its bounded window. Fills qualify, so a clear or
logo may be found before gameplay. Inspect gpu_frame.content, draw_commands,
textures and the image. gpu_draw_filter:primitives skips fill-only frames;
gpu_draw_filter:textured requires textured polygons/rectangles. Both filters
require gpu_next_draw and remain within its search/run budget.
search.effective_max_frames shows the window available within frames, and
search.truncated_by_run_budget identifies a shorter run. A frames:1 run can
inspect only one completed frame even when gpu_search_frames defaults to 60.
textures decodes the captured frame's resident VRAM and creates a .psxscene for
reference. That capture is screen-space drawing evidence, not recovered world
geometry or a complete level. Raw streamed-file extraction is marked raw_2352;
do not interpret sector framing as ordinary file payload.

Bytes at a known file offset: extract path offset:0x597aa length:0x40 reads only
the sectors that window covers (no need to extract a large volume file) and
returns the bytes inline as hex up to 0x100. start.image.offset is where the
first byte sits in the raw .bin: file offset O is user-data byte O%2048 of the
file's O/2048th sector, after a 16 (Mode 1) or 24 (Mode 2) byte header, so it is
not lba*2352+O%2048 without the header. CHD images report no image offset.

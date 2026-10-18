# Agent investigation workflow

The same workflow guidance is available inside the executable: call MCP
`guide({})` or CLI `psx guide` for an overview and topic navigation. Use
`guide({"topic":"input"})` / `psx guide --topic input` for a specific workflow;
other topics are setup, code, assets, bevy and troubleshooting. The guide reads
no project config and creates no files, so it remains available when setup is
broken. `context` performs actual installation/default discovery.

Start with `context` to resolve project defaults and capabilities. Then run from
a checkpoint, inspect bounded evidence through `artifact_read`, and follow writer
PCs into disassembly or Ghidra. CLI arguments below have equivalent MCP keys;
repeatable flags become arrays. Paths resolve against the configured project root.

## Isolate game studies

Calls without `project_root` use the server's default root: `--root`/`PSX_ROOT`
when the server was started with one, else the client's single MCP workspace root,
else the server's working directory. Registering the server without `--root` and
opening the agent in the study directory therefore needs no per-call root. Call
`context` to confirm the root in use. Two or more client roots are ambiguous and fall
back to the working directory. Start the server with `mcp --require-project-root`
when one client drives several studies: calls that omit `project_root` then fail
instead of reaching a default. A server-side "current project" is deliberately not
offered: subagents share one MCP connection, so selecting it would affect their
concurrent calls.

Every MCP tool accepts an optional `project_root`, an existing directory specified
absolutely or relative to the server's default root:

```json
{"project_root":"/absolute/studies/tomb-raider","image":"Tomb Raider.cue"}
```

Use that with `context`, then retain `project_root` on subsequent calls. A scoped
call runs in a separate process: relative paths, saved `.psx/project.json`
defaults, and default outputs belong to that project. Concurrent calls cannot
change one another's CWD or the server default. Returned next calls keep the
scope, including artifact reads; scoped responses identify `project_root`.
`context(save:true)` writes only that selected project's defaults. Absolute
input paths can still refer to shared media or tools. CLI equivalent:
`psx --root /absolute/studies/tomb-raider context --image 'Tomb Raider.cue'`.

IO errors expose the project root/path-resolution base; filesystem errors also
include an absolute `error.path` where available. A shared BIOS directory remains
shared deliberately; use absolute `PSX_BIOS_DIR` or
`PSX_GHIDRA_HOME` when a shared installation lives outside each study.
`PSX_SIGS_DIR` similarly shares a PsyQ signature checkout; explicit `symbols.sigs`
overrides it and `context.signatures` reports the default path and directory presence.
For BIOS files still held only in the toolkit checkout, import them once with
`bios_install(from:"/absolute/toolkit/bios/scph7502.bin")` or set an absolute
`PSX_BIOS_DIR` before starting the server. Discovery does not search sibling projects.

## Inline controller input

```json
{
  "from_state": ".psx/study/start/state.bin",
  "frames": 60,
  "input": [
    {"frame": 0, "buttons": ["CROSS"], "until": 5},
    {"frame": 10, "buttons": ["RIGHT"]},
    {"frame": 30, "buttons": []}
  ],
  "sample": [{"addr": "8015E8AC", "format": "fx16.16", "name": "y"}, "8015E89E:u8:state"],
  "save_state": true,
  "brief": true,
  "return_screen": true
}
```

CLI: `psx run --from-state STATE --frames 60 --input '[...]' --save-state --brief`.
`replay` / `--replay PATH` reads the same JSON array from a file; choose either
inline input or a replay file. Each `sample` entry is either `ADDR:FMT[:NAME]` text
(the CLI form) or a typed `{"addr", "format", "name"}` object; `format` defaults to
`u32`, and a watch-style width (`1`, `2`, `4`) reads unsigned.

Each event replaces the **complete pad state** at a run-relative frame boundary.
Without `until`, that state lasts until the next event. Empty buttons release all
buttons. `until` is an exclusive end: `[0,5)` presses for five frames and applies
neutral buttons/sticks at boundary 5. An explicit event at boundary 5 replaces that
neutral state. Releases at the final requested boundary are reflected in the final
native checkpoint, including a zero-frame run's boundary-0 input. Before the first
event, a resumed run retains its checkpoint's pad state.

Events must be strictly increasing, with no duplicate frames/buttons or overlapping
`until` intervals. Boundaries must lie within `0..=frames`, and `until > frame`.
Malformed input fails before an emulation output directory is created. Both
backends use this validator; replay files are capped at 1 MiB and 10,000 events.
Legacy files must also meet the ordering and frame-budget rules.

Button names are case insensitive: SELECT, L3, R3, START, UP, RIGHT, DOWN, LEFT,
L2, R2, L1, R1, TRIANGLE, CIRCLE, CROSS, SQUARE. Optional `sticks` is
`[left_x,left_y,right_x,right_y]`, each 0–255, centered at 128. Noncentral sticks
require the native backend and a DualShock pad; for checkpoints the pad type comes
from the state, so configure it at boot. Missing sticks center the axes.

The full report's `input.events` records the normalized schedule, including
generated releases. Brief output previews eight events and reports their page
metadata. Input, poke, GPU and trace-window frames count from this run's start;
log `frame` values are absolute, and `run_frame` values are relative.

Native reports also include `input.application`: scheduled, applied and pending
event counts. When a schedule was supplied, `input.jsonl` records every actual
application with its schedule index, requested relative frame, actual `run_frame`,
absolute `frame`, buttons and sticks. Final-boundary releases are included.
Timed-out runs retain pending counts instead of claiming the whole schedule ran.
This observes scheduler application, not whether the game polled each button.

## Choose useful GPU frames

An exact `gpu_frame` can successfully capture an empty frame. Inspect
`gpu_frame.content`: `empty`, `no_draws`, `fills_only`, `primitives`, or
`textured_primitives`, with counts for fills, primitives and textured primitives.
Capture completion and content classification are separate.

`gpu_next_draw` keeps its original default: any draw, including fills. Add
`gpu_draw_filter:"primitives"` to skip fill-only frames, or `"textured"` to require
a textured polygon/rectangle. CLI: `--gpu-next-draw --gpu-draw-filter textured`.
The filter requires next-draw mode and shares the existing bounded search window
and run budget. A textured draw could still be a BIOS screen or logo; it does not
establish gameplay readiness. Use snapshots/checkpoints and observed game events
to locate the state needed for an experiment.
`gpu_frame.search.effective_max_frames` reports the window available inside
`frames`, with `truncated_by_run_budget` when it is shorter than `max_frames`.
For example, `frames:1` permits one completed frame even when the search window is
60; increase the run length to pass fields with no drawing commands.

## Bound extraction results

Asset sources can be host files or directories as well as discs and ISO members.
Host directories expand recursively in sorted order, without following symlinks;
each file keeps its source path, archive provenance and sibling `.DIR`/`.VB` lookup.
Texture sources use the same rules.

`assets_extract` / `psx assets extract` writes a complete, content-bound
`extraction-<sha256>.json` manifest in `out`. Identical conversion results reuse
that manifest; different results have distinct manifests. Original asset bytes
and provenance remain intact.

Use `brief:true` / `--brief` for counts and a manifest read call. Otherwise,
`limit` (default 100, range 1..=1000) bounds returned records and nested output
rows; `report_offset` chooses the first record. The existing hex `offset` still
selects a source asset. Response paging never limits the actual conversions.
`assets_page`, `entries_page` and other list-page metadata describe truncation.

Follow `next_call` to read the full manifest through `artifact_read`, then use its
cursor to continue. Do not repeat extraction merely to get another response
page. Nested `outputs`, uploads and texture-page lists are complete in the
manifest; use JSON pointers to inspect a specific record or list. The summary's
output-file count excludes the manifest itself.
TMD conversions expose `details.polygons`, `details.skipped_primitives` and a warning
if packets were omitted. The summary counts warnings and skipped primitives even
in brief mode. Preprocessed primitive-run representations require explicit parser
support and may be missed by standard scanning; see `guide`'s assets topic.
Manifest output records include file sizes and SHA-256s, including model texture
pages and raw upload words. Reusing an output directory can replace conversions;
the retained manifests let a consumer detect when a file no longer matches an
earlier extraction's bytes.

## Read artifacts directly

`artifact_read` / `psx artifact-read PATH` accepts a file or directory. Run results
include `artifacts` descriptors with paths, sizes, formats and concrete read calls.
A directory read inventories its immediate children; follow a returned directory
path to inspect nested screens. No files are written by the reader.

| Format | Result and navigation |
|---|---|
| Directory | `items` of descriptors; `offset`/`limit` and `next_cursor` |
| JSON | `pointer` selects an RFC 6901 value; arrays page as `items`, other values return `value` |
| JSONL | `items` of parsed objects; stream with `next_cursor` |
| CSV | Header-keyed string records, including quoted commas, escaped quotes and quoted newlines |
| Text | UTF-8 lines; optional case-sensitive `contains` filter |
| Hex | Space-separated bytes; `offset`/`limit` are byte counts |
| PNG | Dimensions and image descriptor; MCP also attaches the PNG automatically |

`format` overrides filename detection. Use hex for binary data and text for a large
JSON document that cannot be loaded within the JSON budget. CSV values remain
strings; frame/PC filters compare their numeric values.

```json
{
  "path": ".psx/runs/RUN/samples.csv",
  "frame_field": "run_frame",
  "frame_from": 2,
  "frame_to": 20,
  "columns": ["run_frame", "y"],
  "limit": 5
}
```

JSONL and CSV support inclusive `frame_from`/`frame_to`, `frame_field` (`frame`
or `run_frame`), hex `pc` (matching `pc`/`pc_hex`), `contains`, and field projection
with `columns`. Filters run before projection. CLI uses repeated `--column NAME`.
To read a report section: `psx artifact-read REPORT --pointer /input/events --limit 10`.

Continue by copying `page.next_cursor` into the next request's `cursor`, retaining
the same path, format, pointer and filters, and omitting `offset`. `limit` may
change. Cursors bind canonical path, size/modification metadata and query; changed
sources or filters return `stale_cursor`. This detects normal file changes; it is
not a claim that every stream page hashes the entire source. JSON and PNG reads
also expose full-file SHA-256 after reading their complete bounded input.

Reader budgets: 1–1,000 returned rows/bytes; 4 MiB of streamed scan data per call
(plus at most one 64 KiB record); 64 KiB per stream record, including CSV headers;
16 MiB per JSON file; 4 MiB per PNG; 10,000 directory children. Selection has a
96 KiB content budget and the final formatted response is capped at 192 KiB.
Narrow pointers/columns or use smaller pages when a value exceeds a response cap.
The reader returns explicit limits instead of recursively spilling its own result.

## Pagination and coverage

Query pages expose `total`, `offset`, `returned`, `limit`, `truncated`, `complete`
and `next_offset`. Complete means the returned page contains the whole result
from offset zero. `coverage` separately describes what was inspected. A final
page after a nonzero offset can finish navigation without itself being complete.

For streaming artifacts, `total` is null until a call inspects the whole query
from its beginning. A page can return no matching rows and still have a cursor:
the scan byte budget was reached before the source ended. `coverage` records byte
ranges and records inspected; continue until `truncated` is false. `offset` and
`next_offset` count source records, before filtering (CSV excludes the header).
Use the cursor to avoid rescanning large logs.

- `ls`: case-insensitive path `filter`, `offset`, `limit`; default 100 entries.
- `assets_scan`: filename `filter` applies before reading files/running parsers.
  `offset`/`limit` page files with hits/uploads/archive indexes; `item_offset` and
  the same `limit` page each file's hits, upload streams and archive entries.
  Use a narrow filename filter for nested continuation. Totals describe the
  selected scan scope, independent of the page. Coverage lists selected/inspected
  files and paginated skipped-file reasons; failed reads make coverage incomplete.
  Default text is a summary with up to three hit previews per file; `json:true`
  returns structured pages. Custom assets may remain unrecognized even when the
  supported signatures were scanned completely.
- `xrefs`, `ram_diff`, `ram_search`, `loads`: `offset`/`limit` and shared page
  metadata. Static xrefs do not claim complete indirect-reference coverage. RAM
  search saves all matches when requested, regardless of the displayed page.
  Load spans still cover the full result; only individual transfer runs are paged.
- Ghidra function/reference/caller/callee pages use the same metadata; index
  truncation and unresolved indirect references remain separate limitations.
- `disasm(text:true)` and `xrefs(text:true)` return text and summary metadata
  without duplicate JSON instruction/reference lists. JSON mode retains details.

Native write watches distinguish disabled, enabled with zero events, and capped
recording. `writes.jsonl` is produced only when a write watch was requested.
`watch.recorded` counts stored raw events, `emitted` counts events after
`watch_changes_only`, and `complete`/`truncated` describe the recording cap.
Read watches report the same cap distinction plus a bounded reader-PC summary.
`disc.media_files` lists the image files backing the disc (the `.bin`, `.chd`);
`disc.filesystem` counts the ISO9660 files and directories on it.

Large ordinary tool results can still exceed MCP's 256 KiB inline limit. Their
spill wrapper includes an artifact descriptor and an `artifact_read` next call;
select an array/section with `pointer` instead of requesting the entire object.

## Ghidra concurrency

Requests for the same analysis wait for its psx owner, including concurrent cold
imports and cross-process CLI calls. Queries, annotations and symbol export hold
the same exclusive lock. Different analyses have independent locks. Waiting
counts against each operation's `timeout` and supports cancellation; export now
accepts `timeout` too (default 300 seconds). MCP's execution-slot queue is bounded
by the call cap and supports cancellation.

Foreign ownership markers and Ghidra GUI locks remain explicit `analysis_busy`
errors. psx never removes another owner's lock. An interrupted psx marker can
cause a queue timeout and requires inspection; waiting does not establish that
the original owner has exited. Cached project fingerprints are checked after
acquiring ownership, preserving detection of external edits and staged revisions.

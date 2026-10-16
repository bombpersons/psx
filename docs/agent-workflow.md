# Agent investigation workflow

Start with `context` to resolve project defaults and capabilities. Then run from
a checkpoint, inspect bounded evidence through `artifact_read`, and follow writer
PCs into disassembly or Ghidra. CLI arguments below have equivalent MCP keys;
repeatable flags become arrays. Paths resolve against the configured project root.

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
  "sample": ["8015E8AC:fx16.16:y"],
  "save_state": true,
  "brief": true,
  "return_screen": true
}
```

CLI: `psx run --from-state STATE --frames 60 --input '[...]' --save-state --brief`.
`replay` / `--replay PATH` reads the same JSON array from a file; choose either
inline input or a replay file.

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
`disc.media_files` names backing images and `disc.filesystem` names filesystem
statistics; the previous `files` and `entries` aliases remain available.

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

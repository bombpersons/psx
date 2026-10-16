# Agent usability: investigation workflow

2026-10-16. Implements the four follow-ups to
[first-use setup](agent-usability-first-use.md): artifact inspection, inline input,
query pagination/coverage, and queued Ghidra requests. Contracts and practical
examples live in [docs/agent-workflow.md](../docs/agent-workflow.md).

## Implementation

- `artifact_read` / `artifact-read`: bounded directory inventory, JSON pointers
  and array pages, streamed JSONL/CSV/text, hex bytes and automatic PNG delivery
  over MCP. Frame/PC/substr filters and column projection avoid custom scripts.
  Opaque cursors bind source metadata/query and preserve stream positions. Partial
  scans expose unknown totals, byte ranges and continuation, including empty
  filtered pages. Oversized records/values fail explicitly; reader results never
  recursively spill. Ordinary spill wrappers now include a read descriptor/call.
- `run.input`: typed button-state arrays, optional exclusive `until` releases,
  ordering/budget/overlap checks, shared validation with replay files and Redux.
  Final-boundary releases are applied before saving native checkpoints. Reports
  include the normalized schedule and actual final native pad state; brief mode
  previews eight events. Noncentral sticks require a restored/configured DualShock.
- Shared page metadata in filesystem listings, asset scans, xrefs, RAM diff/search,
  load transfer runs and Ghidra indexes. Asset file filters run before parser
  execution. Nested hit/upload/archive pages retain full scan totals; skipped
  reads make coverage explicitly incomplete. Standard signatures do not establish
  absence of custom assets. Compact text omits duplicated detail arrays.
- Native watch results distinguish disabled, empty and capped observation, with
  optional artifact paths. The new cap regression exposed that `max_events` had
  not been applied to native write watches (the observer used its diagnostic
  10,000-hit default); investigation runs now honor the requested cap, including
  zero. Recording and post-filter emitted counts are separate.
- Same-analysis Ghidra calls wait on atomic psx ownership markers, including cold
  imports, cached queries, annotations and export. Waiting supports timeout and
  cancellation. Foreign/GUI locks and stale-project checks remain enforced.
  An interrupted marker is preserved and can time out; no guessed lock removal.
  MCP execution-slot waits also count toward the hard call cap.

## Validation

Workspace tests, strict Clippy, both opt-in Ghidra tests and the real-game native
observation test pass: **221** default workspace tests, **2** Ghidra integration
tests and **1** recorded jump/poke regression. The suite covers:

- CLI/MCP artifact equality, JSON pointer/cursor continuation, stale source/query
  detection, escaped/multiline CSV, numeric frame/PC filters and column projection.
- Multi-page filtered logs without duplicated/missing rows, a scan byte-budget
  miss followed by successful continuation, initial offsets beyond the scan
  budget retained across cursors, record limits, directory descriptors,
  raw-byte offsets, and PNG content matching the file bytes.
- A spilled 4,096-instruction result navigated through `artifact_read` directly.
- Inline/replay/CLI checkpoint byte equality; a saved intermediate held state and
  final exclusive release; malformed schedules rejected before output creation;
  disabled-watch artifact absence and a capped, changes-only write recording.
- Authored ISO listings and nested asset pages, full counts despite limited
  results, filters preceding parser launch, visible read failures, RAM query
  continuation and compact disassembly/xref output.
- Same-analysis wait/release, independent analysis locks, deadline/cancellation
  preserving ownership and foreign-lock rejection. Two real simultaneous cold
  MCP decompilation requests share a single Ghidra analysis and both succeed.
- Existing G2 annotation/export/same-address overlay checks and recorded Mega Man
  8 jump/poke observations continue to pass.

Local game checks:

- Mega Man 8 checkpoint frame 3901 resumed for six frames with inline
  `CROSS` at 0 and release at 1. It reaches 3907, writes samples/state/screenshot,
  and the artifact reader returns filtered `frame,y` records: 3902→441.0,
  3903→435.46484375, 3904→435.71484375, with a continuation cursor.
- Tomb Raider `/PSXDATA/LEVEL2.PSX` still yields the independently validated VAB,
  with complete standard scan coverage for that file. No VAB detection changes.
- Tomb Raider `ls --filter PSX --limit 3` returns three of 23 matching entries,
  while coverage records all 50 filesystem entries.
- Its `0x80011E90` function still emits the discovered 252-instruction extent;
  text output no longer also returns the structured instruction list over MCP.

Release verification: build `src-fdc414f687ad7845-release` succeeds. A fresh MCP
process advertises 37 tools with matching CLI/server/context identity. It resumes
the Mega Man checkpoint with inline input, verifies the released final pad, pages
sample records with a cursor, attaches the final PNG both from `run` and
`artifact_read`, and returns the paginated Tomb Raider listing. The PNG bytes
match the artifact on disk.

Game bytes and run evidence stay outside versioned source. Existing MCP processes
must restart to obtain the new 37-tool catalog and source build identity. Template
input rebinding/controls UI is the next toolkit queue item; backend pacing and
display fidelity remain a separate correctness investigation.

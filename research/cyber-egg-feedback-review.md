# Cyber Egg usability feedback, 2026-10-16

Reviewed the local sibling study's
`../external_agent_tests/cyber_egg/research/PSX_TOOL_FEEDBACK.md`, which reports
build `src-2428047e51663f34-release` / 0.1.0. Changes here concern the shared
toolkit and starter. The Cyber Egg game and its custom PAC/model decoders were
not modified or rerun.

## Fixes and guidance

**Host directory sources.** The report correctly identified that `scan_inputs`
read host paths as files. Both asset commands now expand directories recursively,
sort regular-file paths and process each through the existing host-file decoder.
Directory traversal does not follow symlinks. File names and archive entry offsets
remain in provenance; each file resolves its own `.DIR` and `.VB` siblings.
Filters act before decoding and scan coverage counts discovered, selected and
inspected files separately. Empty directories are valid and yield zero assets.
Directory contents are treated as asset files, rather than recursively opening
any contained BINs as discs. Explicit single-file/disc syntax keeps its behavior.
Additional texture sources share this resolver.

**Removing the starter demo.** The claimed independence was wrong:
`tests/player.rs` and `tests/dev_mcp.rs::headless_render_edit_step_and_query`
depend on demo rules. README and AGENTS now explicitly require replacing/removing
those tests and adapting/removing `Client::state` alongside `demo/`. The GPU/MCP
test assumes its named player, initial transform, movement constants and scene.
The instructions explain that ignored tests still compile and prescribe
`cargo test --features dev-mcp --all-targets --no-run` after migration. This is
an explicit migration requirement; the smoke test still exercises the shipped
demo and was not made independent of it.

**Incomplete TMD conversions.** Standard parsing remains strict. Conversions
now expose `details.polygons` and `details.skipped_primitives`; omitted packets
produce a warning with external-parser guidance. `summary.warnings` and
`summary.skipped_primitives` remain visible in brief extraction responses and in
the complete manifest. Skipped packets can be unsupported primitive types as well
as polygon layouts. They do not by themselves identify a preprocessed format.
The assets guide explains that runtime FIXP `size:0` describes an unknown
contiguous source extent, not empty geometry. Gs-style primitive-run detection
and normalization remain unsupported automatically; the reported 71-object robot
result is study evidence, not a new validation claim for the standard parser.

**Shared signatures and BIOS.** Explicit `symbols.sigs` / `--sigs` wins, then
`PSX_SIGS_DIR`, then the project-local `external/psx_psyq_signatures` default.
Set an absolute environment path when starting the server to share a checkout
across scoped projects. `context.signatures` reports that default path, selection
and directory presence without loading or validating the index. Lookup failures
also explain the shared setting. BIOS already has a shared store; setup guidance
now explains importing a checkout-local dump, using absolute `PSX_BIOS_DIR`, or
saving an explicit project BIOS. Discovery still does not search sibling projects
or download firmware automatically.

**GPU search budgets.** Capture metadata now includes
`search.run_budget_frames`, `search.effective_max_frames` and
`search.truncated_by_run_budget`. The effective maximum is
`min(max_frames, max(0, frames - from_run_frame))`; timeout can reduce actual
completed frames further. This is present in brief/full reports and captured
frame summaries. The run budget and frame-selection behavior are unchanged.

**Build guidance.** The starter documents that switching `dev-mcp` can rebuild
shared dependencies through Cargo feature unification. Keep one feature set
through the agent iteration cycle and prewarm default/release builds separately
when needed. No dependency-feature changes or new performance measurements were
made; the reported 9m 34s remains the study's observation.

## Remaining interface work

- A thread/session workspace handle needs explicit ownership, lifetime and
  concurrent-client semantics. Per-call roots remain isolated and continuation
  calls already retain them; saved project context does not change the server root.
- Shorter MCP descriptions and shared guide/schema references would reduce
  repeated context cost. The current tool descriptions and run schemas are unchanged.
- Typed sample descriptors remain a useful extension. Until then samples require
  `ADDR:i32`, `ADDR:u32`, `ADDR:fx16.16`, etc.; `ADDR:4` is a watch width.
- A separate run summary mode could omit most artifact descriptors while keeping
  report/run paths and an image. Existing `brief` output remains compatible.
- Feature alignment should follow measurements of both template build modes;
  documentation alone does not remove duplicate dependency compilation.

The positive findings support retaining explicit roots, checkpoint identity,
bounded capture coverage and clean game rules/presentation separation. None of
this feedback establishes an emulation blocker or calls for loosening standard
asset validation.

## Validation

- `cargo test --workspace` passes (optional local-media/Ghidra checks remain
  ignored by default).
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- Authored CLI/MCP regression fixtures cover nested directory scan/extract parity,
  sorted results, name filters, empty directories, a symlink loop, Croc `.DIR`
  provenance, split `.VH`/`.VB` waveform extraction and original asset bytes.
- An authored TMD containing a triangle and unsupported line verifies parsed/
  skipped counts and the warning in full and brief extraction responses.
- A separate-process signature test verifies the shared environment default,
  context discovery and explicit `--sigs` precedence without mutating the test
  process environment.
- GPU unit checks cover full, shortened and zero-length effective windows. The
  native CLI/MCP integration check verifies metadata parity and a one-frame miss
  with the default 60-frame search window.
- Template generation tests continue to embed the updated README and AGENTS.
  Template runtime code was unchanged; no new offscreen rendering claim is made.

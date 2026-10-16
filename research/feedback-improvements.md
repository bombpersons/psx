# Feedback improvements (2026-10-16)

Follow-up to the [Tomb Raider/WipEout review](tomb-raider-wipeout-feedback-review.md).
Scope is the toolkit interface and evidence loop. Skill/plugin packaging remains
deferred; backend CD/FMV pacing and optional display normalization are separate
correctness work.

## Changes

- The `replay` property now explicitly means a JSON **file path**, with frames
  relative to the current run, including checkpoint continuations. Inline states
  belong in `input`.
- `context` carries a known PAL/NTSC display region into its suggested
  `template_create` call. It does not infer a game's logic cadence.
- Every MCP tool accepts `project_root` for one isolated call. Separate worker
  processes use the existing typed command implementations with a selected CWD;
  the long-running server never changes its CWD. Scope is retained in returned
  next calls and artifact-read calls. Saved defaults, relative paths, screenshots
  and default outputs stay in the selected study. Omitting the argument still
  uses the server's original root. No new global mutable project selection exists.
- IO errors identify the path-resolution base and, for media filesystem errors,
  an absolute failing path. BIOS inventory/context explain full-file versus
  native-normalized hash domains. Symbol results explain signature-set scoring,
  skipped ambiguous matches and why library coverage is not whole-game coverage.
- Asset extraction supports `brief`, `report_offset` and `limit`. The complete
  result is written to a content-bound manifest before response lists are paged.
  Summary counts cover all selected conversions, independent of response paging.
  Nested output lists are bounded too. Follow `next_call` through `artifact_read`
  instead of repeating extraction to navigate results. Every output has size/hash
  evidence, including model texture pages and raw upload words. Changed output
  bytes yield a different manifest even when paths/geometry metadata match.
- Native `input.application` reports observed applied/pending counts. Actual
  applications, including final releases, are logged to `input.jsonl` with
  schedule index, requested relative frame, actual relative/absolute frame,
  buttons and sticks. A timeout does not claim to apply pending events. Redux
  explicitly marks application observation unavailable. Scheduler application
  does not prove the game polled the button.
- GPU capture results classify empty/non-drawing, fill-only, primitive and
  textured-primitive content. Next-draw's default still accepts fills; optional
  `gpu_draw_filter:primitives|textured` skips unsuitable candidates within the
  same bounded run/search window. Neither capture completion nor a textured draw
  claims gameplay readiness.

Usage and contracts are in [agent workflow](../docs/agent-workflow.md) and the
embedded `guide`. This change does not add per-game format guesses or modify VAB
validation, raw raster sizes, emulation timing, or checkpoint format.

## Verification

Commands at the repository root:

```sh
cargo test --workspace
cargo test -p psx
cargo clippy -p psx --all-targets -- -D warnings
cargo build -p psx
cargo build --release -p psx
git diff --check
```

Authored CLI/MCP tests cover:

- Concurrent project saves to two directories, unchanged server defaults,
  relative-file reads, automatic next-call scoping, scoped screenshots and
  absolute IO-error paths, plus invalid root selectors and text-mode tools.
- Cancellation of parser and Ghidra descendants through both the original
  server execution and the project-worker path (Linux process tests).
- Extraction summaries and truncated asset/output lists backed by complete
  manifests; manifest reuse for identical bytes, changed identity for modified
  pixels, preserved previous manifest bytes and pre-write limit validation.
- Input schedule/file equivalence, actual log frames after a checkpoint at
  absolute frame 37, intermediate held state, final-boundary release and pending
  input after a timeout.
- PAL-aware template suggestions and GPU searches that skip fills and optionally
  untextured draws, while exact empty/partial capture remains available.

The workspace suite passed. After the final output-hash/log-write changes, the
psx suite and strict Clippy passed again. One verification attempt encountered
`Text file busy` launching a freshly generated parser; the full psx rerun passed.
Opt-in installed-Ghidra and media-dependent tests were not run in this slice;
the cancellation tests use authored worker fixtures.

## Local game checks

Tomb Raider: resumed the agent's `/tmp/opencode/menu1/state.bin` at absolute frame
12400 for 200 frames with `scph7502.bin`. CROSS during `[0,30)` records applications
at absolute frames **12400** and **12430**, with zero pending events. Final RAM
SHA-256 starts `5f1c7c8e35e03e33`; explicit neutral input produces
`eb10378e76b06541`, reproducing the earlier review's distinct outcomes. Reports
and input logs are in `/tmp/psx-feedback-fixed-{replay,neutral}/`.

WipEout: filtered the actual disc for `LOAD1.TIM`, then extracted
`/WIPEOUT/TEXTURES/LOAD1.TIM` with `--brief`. The response contains one TIM,
two output files, zero warnings, no inline asset records, and a manifest/read
call. Outputs are in `/tmp/psx-feedback-fixed-wipeout/`. No game media or BIOS
files were added to the repository.

## Remaining work

Native/Redux progress parity needs event-aligned CD/FMV investigation; it is not
fixed by these diagnostics. Optional normalized comparison screenshots need
mode-specific validation while preserving raw capture evidence. Optional flatter
asset export naming/retention controls and a portable skill/plugin bundle remain
future work. Project subprocess cancellation was tested on Linux; this does not
claim Windows descendant-process parity.

# Agent usability: first use and trustworthy evidence

2026-10-16. First implementation slice following the
[Tomb Raider/interface review](agent-usability-review.md). G2 predates this slice
and remains intact. The external agent's original notes predated G2.

## Changes

- CLI `context`, `bios list`, `bios install`; MCP `context`, `bios_list`,
  `bios_install`. Context reports build identity, root/output paths, candidates,
  selected BIOS and selection reason, capabilities, setup gaps and next calls.
- Explicit local import or regional Internet Archive download, pinned SHA-256 and
  size checks, HTTPS-only transport, cancellation, atomic publication and
  preservation of existing installations. Shared content-addressed storage;
  normal runs never download. See [setup](../docs/bios-setup.md).
- Optional run BIOS, native default, saved project media/BIOS/backend preferences.
  Native checkpoint headers select the exact BIOS and native backend; a report
  supplies paths only when bound to the state's hash. Core media/BIOS identity
  checks still apply. Authored short ROMs are matched by the padded native hash.
- Source build IDs in CLI version, MCP initialization, context and run reports.
  MCP schemas reject unknown top-level properties and advertise backend choices.
- `brief:true, return_screen:true` now attaches the actual PNG, as full results do.
- Function disassembly emits the discovered extent by default, separately bounds
  search with `function_budget`, and reports boundary evidence, stop reason,
  boundary truncation, listing truncation and continuation address. An explicit
  `count` caps the listing without redefining the discovered function. Bounds
  remain heuristic; Ghidra supplies analyzed function bodies.
- CD recent-command history records issue-time frame/cycle and final repeat
  frame/cycle. The native diagnostic exposes structured `command_history` while
  preserving its text array. Final GPU mode, dimensions and interlace are explicit.
  Recent diagnostic history remains excluded from native state serialization;
  state format stays 8.

## Validation

- Workspace suite: 217 tests pass; the optional hardware/ROM/Ghidra tests remain
  opt-in. CLI/MCP tests exercise missing setup, local import, repeated import,
  saved defaults, checkpoint selection despite changed defaults, explicit BIOS
  mismatch, invalid report binding, regional ambiguity/duplicate images,
  compact/full screenshots, bounded disassembly and malformed download rejection.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- Both opt-in Ghidra stdio tests also pass: executable analysis/cache parity and
  runtime annotation/export/overlay residency. The release build succeeds; a
  fresh release MCP process advertises 36 tools, reports its matching release
  build identity, resolves saved context and attaches the compact run screenshot.
- Actual explicit PAL download to `/tmp/psx-usability-installed-bios` succeeds;
  524288 bytes, SHA-256
  `1faaa18fa820a0225e488d9f086296b8e6c46df739666093987ff7d8fd352c09`.
  A fresh project using only that shared store resolves Tomb Raider's PAL BIOS,
  saves context and runs without BIOS/backend arguments.
- Existing Mega Man 8 state at absolute frame 3901 resumes to 3902 without BIOS,
  media or backend arguments, selecting its original BIOS through `state_sha256`.
- Tomb Raider `/SLES_004.86` function `0x80011E90` yields 252 instructions through
  `0x80012280` exclusive, `start_evidence: jal_target`, `stop_reason: return`,
  both truncation flags false. The old 32-instruction default no longer cuts it off.
- Tomb Raider native boot reaches 900 frames. Its last recent CD command is
  recorded at frame 841; all recorded first/last frames are within the run.
  Final video is PAL, 640×478 interlaced; no resolution normalization was added.
- A core unit test switches PAL/NTSC modes between repeated CD commands and checks
  that their original frame/cycle stamps survive aggregation.

## Remaining scope

Bounded artifact readers, inline controller events, wider pagination and coverage
contracts, and queued same-analysis Ghidra requests were the next slice; they are
now implemented in [agent-usability-workflow.md](agent-usability-workflow.md).
Existing low-level tools and file-based replay contracts continue to work.
MCP clients must restart an existing server process to discover the new tools and
build identity; replacing the binary does not update an already running process.

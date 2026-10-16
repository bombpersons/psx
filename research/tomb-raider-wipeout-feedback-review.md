# Tomb Raider and WipEout feedback review (2026-10-16)

Reviewed the new Tomb Raider `psx_notes.md` addenda/retests and WipEout's
`PSX_FEEDBACK.md` / `PSX_TOOL_FEEDBACK.md` in `../external_agent_tests/` against
commit `46a9d43`. This is a triage and reproduction pass, not an implementation
of every suggestion. The earlier [interface review](agent-usability-review.md)
and [workflow validation](agent-usability-workflow.md) remain relevant.

The feedback identifies real integration friction. Several reported failures
predate the current interface, and several technical explanations remain
unsupported. Record the connected server's build ID before another retest;
matching package versions alone do not identify the same implementation.

The subsequent [feedback improvements](feedback-improvements.md) implement the
interface, isolation, extraction-response and run-diagnostic changes recommended
below. The findings in this review describe the pre-change interface.

## Current findings

| Finding | Assessment on current source/build | Recommended response |
|---|---|---|
| TR B13: restored states discard replay input | **Not reproduced** using the agent's own frame-12400 checkpoint; replay and inline input agree and change RAM relative to neutral input | Preserve this real-game reproduction; add explicit checkpoint-input regression coverage and applied-event diagnostics before diagnosing another failure |
| TR B14: `replay` claims to accept inline JSON | **Confirmed field-description defect.** The `PathBuf` field says “JSON array”; the enclosing tool description correctly distinguishes `input` from `replay` | Describe `replay` as a file path directly in the property schema; keep inline input in the existing typed `input` field |
| TR B15: PAL context suggests an NTSC-default template | **Confirmed.** `disc_region_hint: PAL` accompanies `template_create({dest:"game"})`, whose region defaults to NTSC | Carry the known display region in the suggested call; do not guess a game's logic fields-per-tick from region |
| Relative paths and shared saved defaults across sibling tasks | **Real workflow gap.** Paths and `.psx/project.json` belong to the server root, not each client shell's CWD | Make the root explicit in discovery/errors and provide deliberate project isolation; existing CLI `--root`/`PSX_ROOT` helps, but does not give a shared MCP server per-call project contexts |
| Asset-extraction response volume and output-tree growth | **Still open.** Scan pagination does not bound extraction's per-asset results or reduce generated files | Add compact/paged extraction manifests, followed by optional naming/export controls; retain provenance by default |
| Empty or clear-only GPU captures | **Semantics/discoverability gap.** Exact-frame capture records empty frames intentionally; next-draw already skips empty frames but accepts fills, including clears | Expose content classification separately from capture success, and optionally require primitives/textured draws; do not imply that drawing establishes gameplay readiness |
| Input runs lack evidence that events reached the game | **Partially addressed.** Reports now include the normalized schedule and final native pad state; these do not prove the game polled each event | Add actual applied-event counts/boundaries and, where useful, bounded controller-poll evidence |
| TR B4: native/Redux progress divergence | **Serious reported correctness issue, not rerun in this pass** | Investigate CD/FMV pacing separately, using event-aligned comparisons; retain exact BIOS/media/backend identity |
| Capture dimensions and comparison usability | **Still needs mode-specific validation and optional normalization**; the notes' proposed universal 320×240 correction is not established | Preserve raw screenshots and add explicit display/interlace/aspect metadata plus optional comparison images |

Source touchpoints: [context](../crates/psx/src/context.rs),
[argument schemas](../crates/psx/src/commands.rs),
[input application](../crates/psx/src/run_native.rs),
[replay contract](../crates/psx/src/replay.rs),
[asset commands](../crates/psx/src/assets_cmd.rs), and
[GPU selection](../crates/psx/src/run_native/gpu_capture.rs).

## Checkpoint-input reproduction

The saved reports behind TR's claim identify build
`src-1d7e7d8c1314d156-release`. This review built the current CLI:
`src-06b0101d791eefe2-debug`.

Used `/tmp/opencode/menu1/state.bin` (frame 12400), the same Tomb Raider disc,
and `bios/scph7502.bin`. Ran 200 frames with explicit media/BIOS to avoid shared
project defaults affecting the comparison. All runs reached absolute frame 12600.

| Input from the checkpoint | Final RAM SHA-256 prefix | Final checkpoint SHA-256 prefix |
|---|---|---|
| Explicit neutral input at relative frame 0 | `eb10378e76b06541` | `696fc95966eaeb97` |
| CROSS held throughout | `eee97cb741e67405` | `c05adccf858efa38` |
| CROSS during `[0,30)`, replay file | `5f1c7c8e35e03e33` | `17a5623ee67f6c78` |
| CROSS during `[0,30)`, inline input | `5f1c7c8e35e03e33` | `17a5623ee67f6c78` |

Full reports and checkpoints are under `/tmp/psx-feedback-review-{neutral,cross,replay,inline}/`.
The latter two reports contain the frame-0 press and frame-30 release. The held
run reports final `pad.buttons:["CROSS"]`; the released runs report no held buttons.
This demonstrates effective input after restore in this reproduction. It does
not identify the cause of the older failure or prove that menu navigation and
controller behavior are correct in every game state.

## Corrections to preserve

**VAB claim (TR B9).** Re-extracted `/PSXDATA/LEVEL2.PSX` and independently
read its VAB header and waveform-size table. The 48,160-byte header declares a
429,760-byte total; the 381,600-byte body prefix agrees with the sum of the 89
waveforms. All **89/89** waveforms pass block-header/end-flag checks across
**23,850 blocks**: 23,672 flags=0, 89 flags=1 and 89 flags=7. The body occupies
`0xBC28..0x68EC8`, leaving 1,105,966 bytes afterward. This repeats the earlier
positive evidence for audio. A nibble histogram and labeling that chunk “level”
do not establish that extraction synthesized audio from geometry. Loader/runtime
evidence could settle final attribution; do not weaken detection on this report.

**BIOS hashes (WipEout).** Verified the 1,048,576-byte `ps-22j(v).bin` directly:
the full file hashes to `6ac8348c…`; its first 524,288 bytes hash to `1d51fe93…`.
Those match `sha256` and `native_sha256` respectively. The latter describes the
native core's ROM image, not the entire file. Clarify the two domains and the
normalization rather than making them artificially equal.

**`context.setup.saved` duplication (WipEout).** Not reproduced: the current
ready response has `saved:false`, `setup:null` and one `bios_candidates` list.
Current source does not create a `setup.saved` candidate list. Obtain the raw
response and build ID if it recurs.

**Symbols.** Low PsyQ signature coverage does not establish that a game's code
is non-PsyQ or that matching failed. Ambiguous signatures are skipped, not chosen;
the source and test explicitly enforce that. Explain coverage scope and how the
SDK score is selected in the result. Byte-different glTF files likewise do not
establish geometry loss; compare normalized semantics before declaring a defect.

## Feedback already addressed

The current interface has optional BIOS discovery, actual CD event frame/cycle
timestamps, explicit captured-mode metadata, disassembly search/listing limits,
inline typed input with release boundaries, `ls`/scan pagination and scan totals,
requested-only write-watch files, backing-media/filesystem naming, and queued
same-analysis Ghidra calls. Existing compatibility aliases can still cause
confusion. WipEout's aggregate `assets_scan.totals` request is already supported,
although bpp-specific summaries would be additional functionality.

See [agent workflow](../docs/agent-workflow.md) for the shipped contracts. A
workflow instruction pack would help discover them, but should accompany clear
schemas/results rather than encode workarounds for incorrect descriptions.

## Suggested order

1. Fix the small schema/next-call defects: replay file wording and PAL template
   propagation. Retain the checkpoint reproduction as a regression fixture recipe.
2. Establish isolated project contexts and explicit path/error resolution for
   concurrent external game studies.
3. Bound extraction responses and add applied-input/capture-content diagnostics.
4. Investigate backend progress parity and screenshot normalization as a separate
   evidence-driven correctness task. Avoid adopting the VAB and raster diagnoses
   without their missing validation.

No toolkit behavior or external test-project files were changed in this review.

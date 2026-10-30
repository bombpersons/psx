# Bounded next-draw GPU capture (2026-10-15)

Implemented on `feat/gpu-next-draw`, based on `feat/headless-template`.

The native `run` command and MCP tool accept `gpu_next_draw` / `--gpu-next-draw`.
`gpu_frame` is the inclusive search start (default 0), and `gpu_search_frames`
limits candidates (default 60, allowed 1..=600). The run's existing `frames`
and wall-clock timeout remain hard bounds. Selection freezes the capture and
lets emulation finish normally. Exact `gpu_frame` capture remains available.

Draw work means completed GP0 polygons, lines, rectangles/sprites or fills
(02h, 20h–7Fh). Environment commands, NOPs and transfers alone do not qualify.
This detects submitted drawing, including draws that leave pixels unchanged.
Only completed frames qualify in next-draw mode. Legacy exact mode can return
a partial capture on timeout, explicitly marked `complete: false`.

The full report, brief report and `gpu_frame.json` summary identify selected
`run_frame` and `absolute_frame`. Legacy GPU `frame` remains relative for
existing consumers. Search metadata records the start, maximum candidates and
completed frames inspected. A miss reports `status: "not_found"` with reason
`search_limit`, `run_end` or `timeout`; the overall run status still describes
whether emulation completed. No capture artifacts are added to that run's file
manifest on a miss.

The capture freezes start/end VRAM, drawing area, display state and GTE
projection parameters. GTE vertex matching retains only the selected and
immediately preceding frames' projections, supporting games that construct
GPU packets one frame ahead without accumulating every searched frame.
The final run boundary is processed so its last completed frame is eligible.

## Validation

- `cargo test --offline -p psx`: 33 passed, 2 environment-dependent tests ignored.
- `cargo clippy --offline -p psx --all-targets -- -D warnings`: clean.
- `cargo build --offline --release -p psx`: passed; release binary rebuilt.
- Six capture tests cover state/upload/copy-only frames, command-based detection
  when VRAM is unchanged, inclusive/exclusive bounds, final-boundary selection,
  timeout/run-end misses, exact empty/partial captures, frozen state and bounded
  GTE projection history.
- An authored MIPS BIOS integration test runs through CLI and real MCP stdio,
  including per-instruction and batched execution. From absolute frame 37 it
  draws red in run frame 2 and green in frame 3. Both interfaces select frame 2
  (absolute 39), return identical capture artifacts, retain the red capture
  while final VRAM is green, and preserve matching full/brief metadata.
  The test also checks exhausted searches and selection of the final frame.
  CLI/MCP parity cases cover invalid bounds, orphaned search limits, overflow
  and unsupported Redux use. No original game bytes are needed.
- Local King's Field PAL smoke test from the existing spawn state at absolute
  8320: run 116 frames, search from 110 with a six-frame limit. Exact frame 110
  has zero commands. Next-draw selects run frame 111 / absolute 8431 after two
  inspected frames: 1,166 GP0 commands, including 1,156 draw commands.
- The selected King's Field capture matches exact frame 111 byte for byte for
  decoded commands, before/after/screen/wire PNGs and captured VRAM. Summary
  differences are limited to selection mode/search metadata. Final RAM/VRAM
  also match, confirming selection does not change the emulation result.
  Captured VRAM SHA-256:
  `2f5e150f2e2818c293140136f06fd7ad43d0b6943f038bc347203c291d00c259`.
  The frame logs 744 GTE projections and lifts 304 polygons to view space.
  `textures` consumes the capture and writes a 1,156-element `.psxscene`;
  the captured 320×240 drawing area was visually inspected.

Local smoke artifacts are ignored under `.psx/gpu-next-draw-check/`.
Usage and MCP arguments are documented in `psx guide run` and `psx run --help`.

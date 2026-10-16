# Project cleanup review, 2026-10-16

Quick source and verification review after Ghidra G2. Scope: CLI/MCP boundaries,
source identity, address validation, worker lifecycle, current documentation and
workspace tests. This is not an emulator accuracy audit or a template UI review.

## Cleanups completed

- Moved symbol loading and residency checks from `main.rs` into
  [`symbols_io.rs`](../crates/psx/src/symbols_io.rs), shared by runtime and static
  tools. Disassembly/xrefs previously read legacy verified names directly from
  the table without checking the selected code image. They now check code bytes;
  legacy reports with a supplied source hash also reject changed source files.
- Kept the new format 2 document and full-body checks in
  [`bound_symbols.rs`](../crates/psx/src/bound_symbols.rs). Indexed body intervals
  avoid scanning every function on each live lookup. Actual body gaps receive no
  name. Existing plain symbol aliases remain usable and explicitly unverified.
- Fixed a reproduced RAM address-validation bug. `ram view --addr C0010000`
  previously read `80010000`: a mask silently folded an unmapped KSEG2 address
  into RAM. [`ram::locate`](../crates/psx/src/ram.rs) now delegates to the shared
  memory map, also rejecting inaccessible KSEG1 scratchpad, BIOS and MMIO.
  Checked range endpoints preserve the exclusive end of main RAM. Regression
  checks include valid mirrors, invalid segments, boundaries and width overflow.
- Updated the current queue and handoff to match shipped work. Ghidra docs now
  cover runtime inputs, patch revisions, symbol export and residency limitations.

## Recommended follow-ups

1. **Finish consolidating address handling.** `run::fold`, watch-range parsing
   and `code::offset` still contain manual masks. Route validation through
   `psx_media::memmap::resolve`, while retaining observed virtual addresses and
   distinct MMIO/scratchpad semantics. Add focused CLI/MCP checks for bad
   segments and ranges crossing mirrors. The G2 paths already use the shared
   resolver; the RAM inspection bug above is fixed.
2. **Split command and Ghidra modules by responsibility.** `commands.rs` and
   `ghidra.rs` each exceed 1,200 lines. Keep shared argument types, move snapshot
   mapping/identity and annotation/export logic into focused modules, and retain
   `Op` as the CLI/MCP boundary. This is a maintainability improvement, not a
   reason to rewrite working interfaces.
3. **Share worker termination and atomic-artifact primitives.** Ghidra and
   external parsers independently supervise Unix process groups, deadlines and
   cancellation. Extract those narrow mechanics while preserving their distinct
   file/log versus stdin/stdout contracts. Keep the existing descendant-kill,
   interruption and staged-publication tests as the acceptance gates.
4. **Add explicit analysis storage maintenance.** Preserving prior annotation
   projects and failed attempts is deliberate, but long studies will accumulate
   copies. A future list/size/prune command should show references from manifests,
   annotation histories and symbol exports, offer a dry run, and preserve all
   referenced input/project artifacts. No cache or project was deleted here.

The workspace suite and Clippy with warnings denied pass; both installed-Ghidra
integration tests also pass. The next agreed feature remains template input
rebinding and its reusable controls UI. These cleanup follow-ups can be separate,
small changes, starting with the remaining address helpers.

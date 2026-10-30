# Ghidra runtime analysis and verified names

Validated 2026-10-16 with Ghidra 12.1.4 / Java 21, Linux. G2 is implemented.
The command reference (`psx guide code`)
describes the CLI/MCP workflow and format 2 symbol contract.

## What passed

Normal workspace tests cover snapshot bounds/entries/GP, report-to-RAM hash
binding, aliases, body gaps, changed bytes outside the immediate PC window,
input/body hash corruption, annotation conflicts/swaps, and worker lifecycle.
A real supervised timeout during annotation verifies that the original project,
manifest, revision and query index remain usable. No normal test needs Ghidra or
game media.

Two opt-in integration tests run against the installed Ghidra:

```sh
cargo test -p psx --test mcp_stdio ghidra_ -- --ignored --nocapture
```

The EXE test preserves the G1 one-call workflow, GP-relative data, BSS, COP2
warnings, callers, cached CLI/MCP parity, project locks and safe reanalysis.
The runtime test authors its RAM and native state without game bytes and checks:

- One MCP call imports full RAM, follows explicit entries inside executable
  candidate ranges, and returns an interior function's C. Query aliases retain
  the requested PC and return the canonical RAM address.
- Changing the input filename reuses the analysis. Changing an instruction at
  the same address creates a different analysis and Ghidra program.
- Rename/comment patches displace a generated name deterministically, reject a
  user-name collision, retain earlier projects and invalidate cached pseudocode.
- CLI and MCP export exactly the same revision-specific symbol result.
- Native trace call labels and watched writes include the exported name while
  all function body bytes match. Disassembly and static references agree.
- Replacing the code suppresses the old name in traces, watches, disassembly
  and references. Snapshot A's stored annotations remain usable afterward.

The negative test changes a helper's immediate from 3 to 4 while retaining its
address. Separate unit tests change a disjoint body range far from the query PC:
a short matching instruction window cannot accidentally validate the name.

## Local game checks

Existing Croc and Mega Man 8 EXE queries still succeed with the updated scripts:

| Study | Writer PC | Enclosing function | Functions indexed |
|---|---|---|---|
| Croc PAL | `800286B0` | `80028688..800286F4` | 915 |
| Mega Man 8 PAL | `80106E40` | `80106E2C..80106E4C` | 1611 |

Ranges in this note are end-exclusive. EXE identities remain those recorded in
[the original integration validation](ghidra.md).

For the runtime gate, capture one frame from the existing Mega Man study state:

```sh
psx run 'media/Mega Man 8.cue' --bios bios/scph7502.bin --backend native \
  --from-state .psx/mm8-study/s3900/state.bin --frames 1 --save-state \
  --out .psx/ghidra-g2-validation/mm8-start --brief
psx ghidra decompile --ram .psx/ghidra-g2-validation/mm8-start \
  --range 80106E2C-80106E4C --entry 80106E2C --addr 80106E40
```

The captured frame is absolute vblank 3901. Its RAM SHA-256 is
`730abe346131edf4783ae34aa17cdafe77b855b8232989602b53387ae8238dae`.
The associated report's RAM hash matches, so the analysis retains its frame,
registers, native backend, BIOS and disc identity. Directories without a matching
report remain usable without claiming that metadata.

Apply this patch and export the returned analysis handle:

```json
{"functions":[{"addr":"80106E2C","name":"mm8_snap_to_floor","comment":"Floor collision writer at 0x80106E40 stores the integer Y halfword, retaining the subpixel halfword. See research/case-study-megaman8.md."}]}
```

```sh
psx ghidra annotate ANALYSIS --file PATCH.json
psx ghidra export-symbols ANALYSIS
psx run 'media/Mega Man 8.cue' --bios bios/scph7502.bin --backend native \
  --from-state .psx/ghidra-g2-validation/mm8-start/state.bin --frames 4 \
  --trace --trace-range mm8_snap_to_floor --watch 8015E8AE:2 \
  --symbols EXPORTED.symbols.json --max-events 200 \
  --out .psx/ghidra-g2-validation/mm8-named --brief
```

The 32-byte function body hashes to
`b0d2edd74964c17426a964509774e256d1e2beba0d628883459e8cf4a0f8e101`.
All four calls from `80106CEC` in frames 3901–3904 carry `mm8_snap_to_floor`
in `trace.txt`. All four halfword writes at `80106E40` carry
`mm8_snap_to_floor+0x14` and `check: "body_match"` in `writes.jsonl` and the report.
The integer Y remains 441, consistent with the existing movement study.

Game bytes, copied RAM, Ghidra databases and run outputs remain ignored and local.
Keep both `.psx/ghidra/` and `ghidra-projects/` when retaining a project. Symbol
exports also depend on their copied input, even after newer annotations are made.

## Limits

Only canonical KSEG0 RAM is mapped in Ghidra. User query aliases fold correctly;
static references through other mirrors may remain unresolved. Scratchpad, BIOS
and MMIO are never folded into RAM. Full overlay lifetimes are not reconstructed.

A final snapshot cannot establish which overlay occupied an address earlier.
Live native trace/watch/read naming checks the observed RAM; post-run summaries,
Redux results and load-provenance events expose only final-RAM candidates for
format 2 names. Code identical across two overlays cannot be distinguished by
bytes alone. Function body validation covers code, not the values of globals.

Generic MIPS load-delay/COP2 semantics still need instruction/runtime checks.
Typed annotations and importing GUI changes belong to G3. Annotation stages
preserve previous databases and failed attempts; automatic garbage collection is
not implemented. Per-project locking, worker process-group cancellation and
revision-specific indexes/caches remain in effect.

Concurrent JVMs in separate PID namespaces exposed a Ghidra launcher issue:
shared `/tmp/hsperfdata_*` warnings could corrupt LaunchSupport's Java-home
stdout. Workers now append `-XX:-UsePerfData` to their local Java options while
preserving caller options. Both real Ghidra integration tests pass concurrently.

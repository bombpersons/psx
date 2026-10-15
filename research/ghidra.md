# Ghidra EXE integration (2026-10-15)

The first integration supplies an EXE and writer PC to one `ghidra_decompile`
MCP call. psx extracts disc files, discovers Ghidra, creates a project, imports
the payload at its declared RAM address, seeds entry/GP/BSS and optional symbols,
analyses, decompiles the enclosing function and returns pseudocode/callers inline.
The CLI shares that implementation. See the
[command reference](../README_FOR_AGENTS.md#ghidra-one-call-from-a-writer-pc-to-pseudocode).

Validated environment: Linux, Ghidra 12.1.4, OpenJDK 21.0.12.1. The worker uses
`MIPS:LE:32:default` with the default compiler specification and shipped Java
scripts, with no Ghidra extension or separate server. The scripts are embedded
in the psx binary, so a deployed executable does not need the source checkout.

## Reproducible checks without game media

```sh
cargo test --offline --workspace
cargo clippy --offline -p psx --all-targets -- -D warnings
cargo test --offline -p psx --test mcp_stdio ghidra_single_call_and_cached_cli_mcp_parity -- --ignored --nocapture
```

The opt-in test creates an authored PS-X EXE with direct calls and delay slots,
a GP-relative global, BSS and a COP2 instruction. It starts with an empty cache
and makes a real stdio MCP decompile call for an interior instruction address.
The helper decompiles to a global plus 3 plus its argument; its caller is recovered
at the authored JAL site. Further checks cover:

- identical CLI/MCP output and no rewrite of the cached decompilation;
- listing functions and querying references to the GP-relative global;
- processing the saved project in a second JVM and resolving a KSEG1 alias;
- partial results retaining an analysis handle when the address is data;
- importing verified function/local labels from the existing symbol report format;
- exclusive project locks, detection of external edits and safe reanalysis that
  preserves the edited database.

Normal tests require neither Ghidra nor game images. They verify input rejection,
address folding, pagination and project lifecycle. Worker timeout/cancellation
tests start a real shell with a child process and check both stop. A separate
stdio MCP cancellation test uses a fake Ghidra launcher and verifies that no
completed analysis manifest is published for interrupted work.

## Local game smoke checks

Commands use local discs only; no original bytes or databases are committed:

```sh
psx ghidra decompile --source 'media/Croc - Legend of the Gobbos.cue::/CROC.EXE' --addr 800286B0
psx ghidra decompile --source 'media/Mega Man 8.cue::/SLES_010.64' --addr 80106E40
```

| Input | EXE SHA-256 | Queried writer | Enclosing function | Functions found |
|---|---|---|---|---|
| Croc PAL CROC.EXE | `47328073274136ab63e9cf5489594384b63b70c277edfeb01722452df68921fa` | `800286B0` | `80028688` | 915 |
| Mega Man 8 PAL SLES_010.64 | `6d573d6c39efb0cecae7e196503ca34830676055e8a859be96d3b98f7fd1161a` | `80106E40` | `80106E2C` | 1611 |

Both cold calls completed with inline pseudocode. Mega Man's function had two
resolved callers; Croc's had none. Absence of a static call reference is not
evidence that a function is unused (dispatchers can call indirectly).

The final release binary exposed all 30 tools through real stdio MCP. Its cold
decompile calls took 22.40 s (Croc) and 21.49 s (Mega Man 8) on this host; cached
CLI calls returned identical JSON in 0.057 s and 0.066 s. These are local smoke
measurements, not general performance guarantees. The workspace suite passed
177 tests (four opt-in tests ignored); the real Ghidra integration test also
passed separately. Clippy passed with warnings denied.

## Limits and next work

The first release accepts PS-X EXEs. Runtime snapshots, overlay-aware symbol
round trips, annotation/type editing and import of GUI annotations remain in
the [integration plan](../docs/ghidra-integration.md).

Ordinary branch-delay and GP-relative analysis works on the authored probe.
This is not a validation of exact PS1 load-delay or GTE semantics in Ghidra's
generic MIPS language. Results always flag that limitation, and decompiler
diagnostics remain visible. Wider indirect-call/jump-table and processor
characterisation should precede any claim of comprehensive PS1 analysis.

Ghidra 12 rejects hidden directory components in project locations. Databases
therefore live in ignored `ghidra-projects/`, while immutable inputs, manifests,
indexes, cached query JSON and worker logs live under `.psx/ghidra/`. Keep both
directories when retaining an analysis. Project/input/script/build identities
control reuse; changed GUI databases produce `stale_analysis`, not cached answers.

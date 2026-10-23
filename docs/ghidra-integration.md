# Ghidra integration plan

Toolkit plan, 2026-10-15. The first EXE integration (G0/G1) is implemented on
`feat/ghidra-integration`. G2 is implemented (2026-10-16); G3 remains planned.
See [runtime validation](../research/ghidra-runtime.md) for the snapshot, annotation
and byte-bound symbol roundtrip gates. Current commands and limitations
are documented in [README_FOR_AGENTS.md](../README_FOR_AGENTS.md#ghidra-one-call-from-a-writer-pc-to-pseudocode).

## Outcome

An agent should be able to follow a watched write to its enclosing function,
retrieve pseudocode and callers, give the function a useful name, and see that
name in subsequent psx traces. The same analysis should open in Ghidra's GUI for
human inspection. Both CLI and the existing psx MCP server expose this workflow.

The normal investigation requires one tool call: supply an EXE source and an
address, receive the enclosing function's pseudocode and callers. psx performs
installation discovery, source extraction, project creation/reuse, import,
analysis and decompilation internally. The agent does not launch Ghidra, write
scripts, configure a loader or call `analyze` first. A compatible Ghidra/Java
installation is a one-time environment prerequisite; it already exists locally.

Make this the next substantial toolkit milestone. Use the game studies as
validation inputs; this work does not implement any game mechanics.

## Starting point

- `psx symbols` already exports PsyQ matches to `.symbols.json`, `.sym` and
  Ghidra's text symbol-import format. See [research/symbols.md](../research/symbols.md).
- `psx disasm` and `psx xrefs` already read EXEs or runtime RAM, accept symbols,
  resolve addresses and estimate function bounds. Keep these available without Ghidra.
- Runtime naming checks executable bytes against RAM. Plain `.sym` files are
  explicitly unverified; the current verified-symbol loader assumes PS-X EXE input.
- The King's Field study needed a custom Java script to seed functions and export
  pseudocode. It demonstrates demand, but hard-codes that game's address ranges.
- Ghidra 12.1.4 is installed locally. Initially validate that version; record the
  actual version in every analysis rather than promise compatibility with untested releases.

## Approach

Use a user-installed Ghidra as an out-of-process headless worker, controlled by
Rust and a small set of shipped Java scripts. Ghidra's documented headless runner
supports import, processing existing programs, pre/post scripts and analysis
timeouts. Its decompiler API provides per-function timeouts and cancellation.
See the [headless reference](https://github.com/NationalSecurityAgency/ghidra/blob/master/Ghidra/RuntimeScripts/support/analyzeHeadlessREADME.md)
and [DecompInterface API](https://ghidra.re/ghidra_docs/api/ghidra/app/decompiler/DecompInterface.html).

Use durable local projects and exported query indexes. Cached function listings
and references should be readable without starting a JVM. Decompile selected
functions in one headless invocation; cache results against the analysis revision.
Measure startup cost before considering a persistent worker. A separate Ghidra
MCP server or third-party bridge is not required for this first implementation.

Proposed implementation homes:

- `crates/psx/src/ghidra.rs`: configuration, manifests, supervision and queries.
- `tools/ghidra/*.java`: import setup, analysis export, decompilation and annotations.
- `main.rs`, `mcp.rs`: CLI and MCP registration of the argument types in `ghidra.rs`.
- `crates/psx-symbols`: source-bound symbols and function body ranges.
- `.psx/ghidra/`: ignored input copies, manifests, logs and query artifacts.
- `ghidra-projects/`: ignored Ghidra databases. Ghidra 12 rejects hidden directory
  components in project locations, so databases cannot live inside `.psx/`.
  An annotation removes the project copy it supersedes (unless it is open in the
  GUI or was edited outside psx) and a failed import removes its own; a
  timed-out import keeps its partial project, and `reanalyze` keeps the old one.

## Proposed interface

G0–G2 operations below are implemented. Type editing and GUI annotation refresh
remain proposed in G3. All operations retain the existing CLI/MCP JSON contract.

| CLI | MCP | Purpose |
|---|---|---|
| `psx ghidra doctor` | `ghidra_doctor` | Find Ghidra/Java and report supported configuration |
| `psx ghidra analyze --source SOURCE` | `ghidra_analyze` | Import an EXE and create or reuse its analysis |
| `psx ghidra analyze --ram RUN --range LO-HI --entry ADDR` | same | Analyse a selected runtime code region, with RAM data context |
| `psx ghidra functions --source SOURCE` | `ghidra_functions` | Ensure analysis, then return a filtered, paginated function index |
| `psx ghidra decompile --source SOURCE --addr ADDR` | `ghidra_decompile` | Ensure analysis, then return the enclosing function, pseudocode, callers, body ranges and diagnostics |
| `psx ghidra xrefs --source SOURCE --addr ADDR` | `ghidra_xrefs` | Ensure analysis, then return analysis-derived references, callers and callees |
| `psx ghidra annotate ANALYSIS --file PATCH.json` | `ghidra_annotate` | Apply explicit names/comments and later types |
| `psx ghidra export-symbols ANALYSIS` | `ghidra_export_symbols` | Feed source-bound function names back to psx |

`SOURCE` uses existing EXE-file and `disc.cue::/PATH` syntax. Query tools accept
exactly one input selector: `source`, `analysis`, or `ram`. `analysis`
is an explicit manifest path returned by an earlier call; `ram` can be a run
directory or 2 MiB dump, with required explicit ranges/entry seeds and optional GP. `analyze`
is an optional prewarm/advanced operation, never a prerequisite tool call.
Addresses accept existing hex conventions. Add filters, limits and batch address
selection so an agent does not need to export every function's C before asking
one question.

Example proposed MCP arguments to `ghidra_decompile`:

```json
{"source":"media/Croc - Legend of the Gobbos.cue::/CROC.EXE","addr":"0x800286B0"}
```

Return bounded pseudocode and callers directly in the result, alongside artifact
paths and an analysis handle for follow-up questions. Return explicit timeout or
partial diagnostics within the same call; do not require polling in the normal
workflow. The first call may be slower because it performs analysis. Later calls
reuse the project and caches automatically.

## Milestones and completion gates

### G0: Confirm the PS1 import and decompiler settings

Build a small probe using the existing local installation and authored MIPS
fixtures. Start with `MIPS:LE:32:default` and its default compiler specification,
which exist in the installed language definitions; validate the calling convention.
Confirm entry setup, `gp` context, delay slots, indirect calls and COP2/GTE handling.
Record unsupported semantics and decompiler warnings explicitly. Generic MIPS
support alone is not evidence of correct PS1 instruction semantics.

Choose `--ghidra-home`, then `PSX_GHIDRA_HOME`, then a documented executable
lookup for discovery. Do not bake in the local installation path. The doctor
reports Java/version problems before creating a project.

**Gate:** a minimal EXE imports at its declared address, its entry and a known
callee decompile, and diagnostics identify any unsupported PS1 instructions.
Resolve blockers here before committing to a custom loader or processor extension.

### G1: Useful EXE analysis through CLI and MCP

- Extract the EXE payload using `psx-media`; exclude its 0x800-byte file header.
  Validate load bounds, truncation and entry location before starting Ghidra.
- Map the declared payload, record entry/GP/SP and BSS metadata, and create
  uninitialised context memory where appropriate. Do not mistake the EXE's
  loadable payload for a precise code-only section.
- Use Java setup scripts around Ghidra's binary import initially. Seed the entry
  and validated symbol functions; let analysis follow reachable code. Treat
  additional JAL/prologue/pointer-table discoveries as candidates with their
  evidence recorded. Do not turn every data word resembling JAL into a function.
- Import supplied PsyQ/user symbols with a visible conflict report. Signature
  data stays optional and local, as today.
- Persist the project; export function metadata, references and selected C files.
  Report unknown function boundaries and individual decompile failures.
- Expose doctor, analyze, functions, decompile and xrefs through CLI and MCP.
  Bound analysis time, per-function time, process runtime, CPU use and output size.

**Gate:** from a known writer PC in a local study EXE and an empty analysis cache,
one decompile tool call yields the enclosing function and its callers without
setup calls or custom scripts. CLI and MCP agree. Repeating the same request reuses the analysis/cache.
The saved project opens in the GUI. Ghidra remains optional for other psx commands.

### G2: Runtime code, overlays and names back into traces

**Completed 2026-10-16.** CLI/MCP snapshot queries, staged rename/comment patches,
revision-specific indexes/caches, format 2 exports, and live body-verified native
trace/watch naming pass the authored and real-game gates. Post-run observations
expose final-RAM candidates without asserting earlier residency. See the
[command reference](../README_FOR_AGENTS.md#ghidra-one-call-from-a-writer-pc-to-pseudocode)
and [validation](../research/ghidra-runtime.md). The scope below is retained as
the acceptance criteria.

- Accept a run directory or RAM image, with explicit candidate code ranges and
  entry seeds; retain the remainder of RAM as data context. Bare RAM also accepts
  explicit GP and entry options. Associate reports/registers only when available.
- Identify each snapshot from actual bytes and mapping, not its filename. Keep
  distinct snapshots as distinct programs even when code occupies the same address.
  Full overlay lifetime reconstruction is deferred.
- Attach frame/backend/media identity and load-provenance links when available.
  If an EXE is supplied as a naming aid, verify the relevant bytes first; retain
  runtime bytes when they differ. A final RAM image cannot prove earlier residency.
- Preserve the observed virtual PC and its folded RAM identity. Define and test
  alias resolution; start with a canonical KSEG0 RAM mapping and return explicit
  limitations for references that need additional mirrored mappings. Never fold
  scratchpad, BIOS or MMIO into main RAM.
- Extend the versioned symbol format to bind names to copied analysis input,
  byte ranges/hashes and function body ranges. Existing EXE reports and `.sym`
  files continue to work. Bound name lookup by actual bodies where available.
- Add rename/comment patches, deterministic collision handling and symbol export.
  User names take precedence over generated names; conflicts are reported.
  Exported verified names work with `run`, `loads`, `disasm` and `xrefs`.

**Gate:** two snapshots with different code at the same address remain separate;
names from one cannot silently label the other. Rename a studied writer, export
symbols, and observe the name in a subsequent residency-checked native trace.

### G3: Iterative analysis and runtime evidence

- Extend annotation patches to function signatures, globals and struct definitions;
  validate layouts, preview conflicts and apply transactions. Keep a portable
  annotation history alongside the Ghidra database.
- Import observed call edges and read/write sites with trace/frame provenance.
  Distinguish observed edges from static references, and use register values as
  scoped observations rather than universally true constants.
- Show function-level coverage and links back to trace/watch artifacts. Preserve
  unresolved indirect calls instead of presenting a complete call graph.
- Re-export indexes and invalidate dependent decompilations after annotations.
  Index data references separately from calls and avoid unbounded graph responses.

**Gate:** an agent can identify a RAM reader, inspect its callers, name the relevant
struct/fields, re-decompile with those types and retain the work across sessions.

## Identity, lifecycle and result contract

The versioned manifest records source hashes, immutable input copies, memory map,
entry/GP settings, Ghidra version, processor/compiler IDs, analysis options,
script hashes, annotation revision and all output paths. These determine reuse;
changing relevant inputs creates a new analysis or invalidates affected caches.
Record whether analysis completed or timed out. Do not cache partial work as complete.

Function results include entry, body ranges, naming origin, signature, pseudocode
artifact path, callers/callees and diagnostics. Distinguish missing functions,
unsupported instructions, analysis timeout and decompile failure. Pseudocode is
an analysis result to check against instructions and runtime evidence.

Run scripts via argument arrays and manifest paths, never shell interpolation.
Write Ghidra/decompiler logs to files and stderr; psx stdout remains JSON/MCP.
Use the existing large-result spill convention and return bounded summaries.
Integrate expensive operations into MCP job limits. Kill the whole worker process
group on timeout/cancellation, including decompiler children, and mark interrupted
projects for recovery. Stage new analyses before publishing a completed manifest.

Serialize access to each Ghidra project, including read-only headless queries;
Ghidra documents that headless use may fail when the GUI has the project open.
Return an actionable busy error; never remove someone else's lock or overwrite a
GUI-edited project. An explicit refresh imports GUI changes into exported indexes
and invalidates caches; GUI edits must not leave apparently current cached answers.

Current G1 behaviour: external edits produce `stale_analysis`. `--reanalyze` with
`--source` creates a new project without deleting the old one; it does not import
GUI annotations. An annotation-preserving refresh belongs to subsequent work.

## Validation

Normal tests use authored binaries and mocked worker results, requiring no game
media or Ghidra. Opt-in integration tests require the documented Ghidra version.

- EXE header/payload/BSS mapping, invalid ranges and paths containing spaces.
- Branch delay slots, GP-relative globals, jump tables/indirect calls, data that
  resembles instructions, and COP2 diagnostics. Compare structural facts and
  known constants rather than exact pseudocode formatting.
- Same-address code replacement, alias queries, symbol conflicts and annotation
  revision invalidation. Verify byte-bound names against changed runtime code.
- Real CLI/MCP parity and useful missing-installation errors.
- Partial analysis, function timeout, cancellation, child-process cleanup,
  project locks and interrupted-project recovery.
- Local smoke checks on two existing game studies: EXE entry/function queries,
  one runtime snapshot, and a renamed writer appearing in a native trace.
  Retain hashes and commands in the repo; keep game bytes/projects local.

## Scope and order

Implement G0 and G1 first as one reviewable initial integration. Follow with G2
before expanding the annotation/type surface in G3. No time estimate until G0
confirms processor semantics and worker startup costs.

Defer a live GUI bridge, debugger sync, whole-game overlay timelines, a custom
SLEIGH processor, BSim/FID integration and automatic source reconstruction.
The first useful deliverable is a single psx MCP decompile call that automatically
imports/analyzes an EXE and returns the requested function. Headless Bevy rendering
and game-specific asset decoders remain separate
toolkit workstreams.

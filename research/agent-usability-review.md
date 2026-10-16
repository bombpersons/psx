# psx agent usability review

2026-10-16. Read the Tomb Raider agent's pre-G2 notes and parser scripts, reviewed
the current working tree, and exercised the debug CLI and real stdio MCP server.
This records the interface before the usability changes. The first implementation
slice is documented in [agent-usability-first-use.md](agent-usability-first-use.md).
G2 remains in the working tree.

## Assessment

The useful primitives are present. The interface still expects agents to know the
project layout, BIOS choice, backend capabilities, frame conventions, file formats
and which tool to call next. Long descriptions explain those conventions, but the
schemas, defaults and results often cannot enforce or communicate them precisely.

The best next milestone is a coherent first-use and investigation workflow:
resolve context, run and checkpoint, supply input, inspect bounded evidence, follow
an observation to code, and export an asset with explicit provenance and coverage.
Keep the underlying expert operations available.

The Tomb Raider notes are strong evidence of friction. Several technical diagnoses
need correction before becoming implementation requirements. In particular, the
claimed VAB false positive is contradicted by the current byte-level checks below.

## Current interface checks

- The current debug MCP server publishes **33 tools**, with approximately **59 KB**
  of serialized tool definitions. `run` exposes **35 properties** and a description
  of **3,669 characters**. It requires `bios` while defaulting to `backend: redux`.
  The server's suggested investigation workflow recommends the native backend.
- First call `run({media: "media/Tomb Raider.cue"})` returns
  `invalid_arguments: missing field bios`. There is no general context/installation
  discovery tool or BIOS enumeration tool. Ghidra has its own doctor.
- The server returns structured MCP errors correctly: `structuredContent.error`
  contains `code` and `message`, alongside a compatible text content block and
  `isError: true`. JSON-in-an-error-string is not established as a current server
  serialization bug; inspect the consuming client's rendering too.
- `backend` is an unconstrained string in the published schema. Ghidra decompile
  only requires `addr`, although one input selector is semantically required.
  Unknown keys are rejected at runtime while the schema omits
  `additionalProperties: false`. There are no published output schemas. Some
  enums are already well represented, such as `TemplateRegion` through `$defs`.
- `run(brief:true, return_screen:true)` fails to attach an image even though
  `screen.png` exists. `brief()` turns `files` into an array; `screen_path()` still
  looks for `files["screen.png"].path`. It then incorrectly suggests the display
  was off or the image was too large. Reproduced using a local Mega Man state,
  with a zero-frame continuation to isolate response formatting.
- Descriptions explicitly return filesystem paths, and there is no MCP artifact
  reader/query operation. Large results switch to a `result_file` wrapper above
  256 KiB. `disasm(text:true)` still returns the full structured line list alongside
  `text` over MCP, although the CLI prints only the compact listing.
- The existing release binary and the current debug binary both report
  `psx 0.1.0`. The release binary's Ghidra help lacks the two new G2 operations.
  Version alone cannot tell an agent which interface it is using. The connected
  tool catalog also lacks those G2 operations; that is a deployment distinction,
  not evidence that the source implementation is missing them.
- The Tomb Raider parser README naturally translates MCP keys into `--media` and
  `--run-dir` flags. Both are rejected by the CLI, whose corresponding arguments
  are positional. This is another predictable source of cross-interface errors.

## Triage of the Tomb Raider notes

| Note | Current assessment | Appropriate response |
|---|---|---|
| B1: required BIOS | Confirmed first-use failure; the required field is in the schema but absent from the startup workflow | Resolve project defaults, enumerate compatible local BIOSes, and preserve exact state identity |
| B2: PAL disc reported NTSC | NTSC label reproduced at frame 900; source labels the final GPU mode, not the disc region | Separate disc hint, BIOS identity, captured display mode and timing history; the notes do not prove a stale mode bit |
| B3: changing screenshot dimensions | Reproduced; claim that 640-wide/high-resolution modes are impossible is incorrect | Preserve raw captures; add display/crop/interlace/aspect metadata and an optional normalized comparison image |
| B4: native/Redux progress drift | Serious reported backend correctness issue; not independently replayed at 1800 frames in this review | Investigate CD/FMVs separately; align comparison checkpoints by observed events and preserve backend identity |
| B5: CD commands beyond run end | Confirmed: frame 900 result includes `frame~1020` | Record actual frame/cycle at the event; stop dividing historical cycles by the final mode's frame length |
| B6: `disc.files` ambiguity | Confirmed naming ambiguity: backing media files versus filesystem entries | Name these `media_files` and `filesystem` statistics explicitly |
| B7: empty watch artifact | Confirmed unconditional creation/reporting | Report whether observation was enabled; return requested artifacts with event counts |
| B8: streamed extraction | Raw-sector behavior is intentional, documented, and marked `format: raw_2352` | Make logical/physical sizes and sector layout prominent; offer explicit extraction formats without discarding payload |
| B9: VAB geometry decoded as WAV | Detection reproduced; alleged false positive not substantiated and strongly contradicted by byte checks | Revalidate the container interpretation before modifying VAB detection |
| B10: parallel Ghidra queries | Still present after G2; locking immediately returns busy | Queue same-analysis requests with deadlines/cancellation; retain protection against external GUI locks |
| B11: silently truncated function | Reproduced exactly on current CLI | Separate function-search budget from listing count, expose truncation and boundary evidence, use matching known Ghidra bodies where available |
| B12: wrong load span | Notes contain an arithmetic error; total transfer volume can exceed a reused buffer's footprint | Label DMA destinations, total transferred bytes, distinct ranges and final residency separately; do not imply final asset location |

P1/P2/P3/P4/P5/P7/P8 all identify real interface costs, although some options are
already documented. `textures` already accepts `out`, `symbols.out` is described as
a prefix in its field schema, `loads` has a run limit, and MCP already has a spill
fallback. Those details do not address the underlying discoverability problem.
P6 needs client/server error-path separation as noted above. P9 mixes hardware
CLUT coordinates with a game-specific global palette convention; a universal
`palette_offset` cannot be inferred from VRAM coordinates alone.

## Important corrections and reproduced evidence

### VAB interpretation

Current `assets scan` of `/PSXDATA/LEVEL2.PSX` reports a VAB at offset 4, with
`header_size: 48160`, `body_size: 381600`, `waveforms: 89` and
`body_included: true`.

Independent parsing of the extracted bytes found:

| Check | Result |
|---|---|
| File size | 1,535,734 bytes |
| First length prefix / computed VH size | 48,160 / 48,160 |
| Declared VAB size | 429,760 = 48,160 + 381,600 |
| Second length prefix / sum of 89 waveform sizes | 381,600 / 381,600 |
| Body range | `0xBC28..0x68EC8`, inside the file |
| ADPCM samples passing block-header and end-flag checks | 89 of 89 |
| Blocks | 23,850; filters 0–4 only |
| Flags | 23,672 ordinary, 89 END, 89 trailing flags=7 blocks |
| Bytes remaining after that chunk | 1,105,966 |

This is strong positive evidence for a waveform body, not an overflow or arbitrary
geometry mistaken for sound. The supplied false-positive script assigns the name
`level` to this second chunk and prints a histogram; it does not invoke psx or
assert that the chunk is geometry. Final game-format attribution would still
benefit from loader/runtime evidence, but there is no basis here for adopting the
proposed rejection rule. Current `body_offset()` already checks source bounds and
ADPCM plausibility for every waveform.

The agent's flat OBJ export also writes every vertex with `z=0`. It is a projection
of a captured draw stream, not recovered world geometry. Asset results should
make this distinction explicit so agents can assess whether the reconstruction
brief has actually been satisfied.

### Frames, displays and sector formats

A fresh 900-frame native run produced 586,722,678 cycles and final GPU mode NTSC.
Snapshots reproduced `(256,240)`, `(640,508)` and `(640,478)` rasters during boot.
The current mode is read directly from the GPU. Average cycles over boot cannot
establish a single constant video mode for that whole interval.

640-pixel horizontal modes and interlaced high-resolution display modes exist;
VRAM has 512 rows. The proposed 368×256 hardware maximum is incorrect. Whether
each captured raster correctly represents output still needs mode-specific
validation. Globally forcing every capture to 320×240 would hide that evidence.
See the [GPU display specification](https://psx-spx.consoledev.net/graphicsprocessingunitgpu/#gp108h-display-mode)
and [display-range description](https://psx-spx.consoledev.net/graphicsprocessingunitgpu/#gp107h-vertical-display-range-on-screen).

Mode 2 Form 2 carries 2,324 user-data bytes per sector, rather than Form 1's
2,048. A blanket 2,048-byte extraction fix could discard valid movie/audio data;
raw sectors also preserve channel/subheader information. See the
[sector layouts](https://psx-spx.consoledev.net/cdromformat/#mode2form2-cd-xa).

For B12, `0x800A3790 - 0x80093790` is **0x10000 = 65,536 bytes**, not 1 MiB.
A stream transferring 1,545,000 bytes through a reused 64 KiB buffer is plausible.
The provenance code tracks CD DMA destinations; it does not follow later CPU
copies or decompression. Its observed buffer address can be correct while the
agent's expected final asset location is elsewhere.

## Recommended implementation order

1. **Protect the evidence loop.** Fix the brief/image combination, disassembly
   truncation reporting and CD event timestamps. Add per-result completeness,
   selected-frame identity, boundary source and validation evidence. Retain the
   precise state/BIOS mismatch diagnostics and G2 byte-bound names.
2. **Make first use deterministic.** Add optional project/context discovery for
   executable build ID, configured workspace, media, local BIOS candidates,
   effective backend and capabilities. Resolve the same defaults automatically
   on ordinary calls. Prefer native for the advertised investigation workflow;
   keep Redux an explicit oracle choice. Resume must honor the state's exact
   BIOS/media/backend identity, then configured preferences, then compatible
   discovered inputs. Never silently choose an arbitrary BIOS from dozens.
3. **Unify input and output contracts.** Pick consistent canonical source/output
   fields, retain compatibility aliases, and support named CLI arguments for MCP
   examples. Distinguish output directory/file/prefix. Publish real enums,
   conditional requirements, bounded query parameters and output schemas.
   Define a shared run result core, with explicit backend-specific capabilities.
4. **Make common agent actions direct.** Accept inline typed input events as well
   as replay files; make press/release timing explicit. Allow continuation from an
   immutable run/checkpoint handle with resolved identities. Add bounded artifact
   reading/querying for JSON, logs, samples and images, so MCP use does not require
   bespoke filesystem scripts for each investigation step.
5. **Make results concise and navigable.** Default to useful summaries with total,
   returned count, truncation/completeness and pagination. Allow filters for files,
   asset kinds, frame ranges and event PCs. Return artifact descriptors and a small
   number of concrete next-call examples. Ensure compact text results actually
   omit duplicate detailed data when requested. Separate media backing files,
   disc filesystem entries, raw captures and normalized previews.
6. **Queue Ghidra work and add workflow regression checks.** Serialise requests
   for the same project without wasting an agent call on routine contention.
   Keep external-edit/GUI-lock safeguards. Track backend pacing and display
   fidelity as a separate correctness workstream with checkpoint-based evidence.

Avoid coupling per-call project selection to process-wide `chdir`: the MCP server
runs concurrent requests. Resolve paths through explicit context instead.

A compact context response should make template creation and parser starters
discoverable, while a scan should clearly say what was inspected and what remains
unsupported. A no-TIM/no-TMD result is about signature coverage, not proof that
models or textures are absent. Surface archive/model/upload/video extension
contracts and reference captures as separate available paths.

## Acceptance scenario

Use a fresh agent/project with only the brief and disc reference. Without reading
psx source or listing the developer's BIOS directory, it should be able to:

1. Discover effective context and create the Bevy starter.
2. Reach a selected checkpoint, see a screenshot, and resume it with inline input.
3. Obtain a bounded sample/watch result, inspect a complete or explicitly partial
   function, name it, and see the verified name on a later trace.
4. Scan assets and understand standard-format coverage and unknown containers.
5. Read or export the resulting artifacts into its project with explicit format,
   frame, source-span and completeness metadata.

Measure failed calls, guessed parameters, unnecessary file-writing steps and
response size. Preserve the simple `symbols -> ghidra_decompile` and
`gpu_next_draw -> textures` loops that the Tomb Raider agent found productive.

## Reproduction commands

```sh
# Silent 32-instruction cutoff; a visible branch targets beyond the returned end.
psx disasm 'media/Tomb Raider.cue::/SLES_004.86' --function 80011E90 --text

# Current VAB classification, followed by independent byte checks described above.
psx assets scan 'media/Tomb Raider.cue::/PSXDATA/LEVEL2.PSX' --json
psx extract 'media/Tomb Raider.cue' /PSXDATA/LEVEL2.PSX -o /tmp/psx-usability-disc-check

# Mode and timestamp interpretation; output goes outside the source tree.
psx native --bios bios/scph7502.bin --disc 'media/Tomb Raider.cue' --frames 900
psx run 'media/Tomb Raider.cue' --bios bios/scph7502.bin --backend native \
  --frames 900 --snapshot-every 150 --out /tmp/psx-usability-tr-run-900 --brief

# Distinguish the installed release from current source-built capabilities.
target/release/psx ghidra --help
target/debug/psx ghidra --help
```

The MCP audit used `initialize`, `tools/list` and real `tools/call` requests.
Catalog and reproduction output are retained locally under `/tmp/psx-agent-usability-*`
and `/tmp/psx-usability-*`; no game bytes or run artifacts are added to this report.

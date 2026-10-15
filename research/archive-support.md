# Archive entries and DOT-T support (2026-10-15)

The first archive task is implemented: a reusable index in `psx-assets`, DOT-T
sector tables, the existing Croc `.DIR` reader using the same entry structure,
entry-bounded asset scanning/conversion and raw entry extraction. Both CLI and
MCP use the same implementation.

## Commands

```sh
# List every entry even when none of its payloads are standard TIM/TMD assets.
psx assets scan "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/RTMD.T" --json

# Extract RTMD entry 0, including sector padding, without decoding its contents.
psx assets extract "../external_agent_tests/kings_field/King's Field.cue::/CD/COM/RTMD.T" \
  --entry 0 -o .psx/kings-field-assets

# Validate a DOT-T table explicitly, including nonstandard header padding.
psx assets scan /path/to/archive.T --archive dot-t --json

# Disable container detection and retain a flat asset scan.
psx assets scan /path/to/file.bin --archive none --json
```

`--entry` takes a zero-based decimal index, requires one selected source file and
cannot combine with `--offset`, `--kind` or `--textures`. It writes exactly the
entry's byte span; empty entries create empty files. Standard asset extraction
without `--entry` retains conversion behavior and adds entry provenance. `--offset`
continues to select an asset by its parent-file byte offset.

MCP uses `assets_scan` / `assets_extract` with `archive: "dot-t" | "none"` and
`entry: 0`; omission means automatic detection. MCP's asset filter is `kinds`,
an array of `tim`, `tmd`, `vab`, `vag`.

## Data contract and validation

`files[].archive` contains `format` (`dot_t` or `croc_dir`) and all `entries`.
Each entry has a stable numeric `index`, optional `name`, and numeric byte
`offset`/`size` within the parent file. Empty entries retain their original index.
Archives remain in scan results even when they have no recognized assets, or
when a kind filter removes every asset hit.

Asset hits preserve the existing hex parent-file `offset`. Their `entry` adds
`index`, `format`, hex `archive_offset`, numeric `size` and optional `name`.
`entry.offset` remains the hex offset *inside* the entry, preserving existing
Croc output semantics. Extraction records also identify their source `file`.
Raw extraction returns an `entries` array with source, format, original entry
record and output path, alongside an empty `assets` array.

The explicit DOT-T parser checks count/table bounds, first payload outside the
table, nondecreasing sector boundaries and final boundary equal to EOF. Equal
boundaries represent empty entries. A zero-entry archive is accepted explicitly.
Automatic detection additionally requires at least one entry and zero padding
to the next sector after the table. These conservative extra checks reduce false
positives in a format without a magic signature. `--archive dot-t` bypasses only
those detection heuristics, not structural validation; `--archive none` opts out.
Automatic indexing is disabled for named RAM dumps or explicit RAM mappings.

Scanning, parsing during conversion and texture discovery use entry bounds;
a candidate asset cannot borrow bytes from the following entry. Ordinary
TIM/TMD/VAB/VAG detection rules are unchanged. Host archives can now resolve
Croc-style sibling `.DIR` files as disc archives already did. Croc checksum
verification remains outside the reader's existing contract.

## Verification

`cargo test -p psx-assets -p psx` passes: 61 tests, with the existing Ghidra-install
and game-state tests ignored. Eight added tests cover malformed tables,
multi-sector headers, empty entries, explicit/detected padding rules, Croc index
bounds, cross-entry asset rejection, ordinary assets, CLI/MCP parity, byte-exact
raw extraction and error cases. Existing TIM/TMD tests continue to pass.

Real-data comparisons against the prior release binary preserve every asset
offset and kind in the five King's Field files, and every Croc asset name:

| File | Entries | Empty | Detected assets, before and after |
|---|---:|---:|---|
| FDAT.T | 70 | 25 | 78 TMD |
| RTMD.T | 9 | 0 | No standard assets; entries now listed/extractable |
| RTIM.T | 75 | 3 | No standard assets; entries now listed/extractable |
| MO.T | 448 | 43 | 405 TMD |
| ITEM0.T | 548 | 145 | 300 TIM, 103 TMD |
| CROCFILE.1 | 648 | — | 32 VAB, preserving names and relative offsets |

Raw extraction of a nonempty entry in each King's Field archive matches the
original source slice byte for byte. Representative TMD conversions from FDAT,
MO and ITEM0, a TIM conversion from ITEM0 and a named Croc VAB conversion complete
successfully, retaining the source bytes and entry provenance. Entry 0 of RTMD
is 217,088 bytes, matching the existing study's bank size.

Local verification reports and final release outputs are under
`.psx/archive-support/`; source media and extracted originals remain ignored.
Authored fixtures contain no game data. No runtime/emulator changes are needed.

## Scope and effort

This delivers queue item 1. Nested traversal, DOT1/DOTLESS, decompression and
custom RTMD/RTIM decoding are separate follow-up changes. Item 2 can now consume
validated entry slices without duplicating the archive parser.

The implementation started at **20:48:39 UTC** and final release verification
completed at **20:58:20 UTC**: **9 minutes 41 seconds of session wall time**, including
the release build (1 minute 17 seconds). The work covers
the first reader, the shared index, CLI/MCP integration, tests and documentation;
it is not an estimate for every archive family or a measurement of future human
implementation time. Most integration work was preserving offsets and bounding
the existing decoders; the table parser itself is small.

The rebuilt `target/release/psx` was used for the final disc-source checks.
Its hash and byte-exact extraction results are in the ignored local
[validation manifest](../.psx/archive-support/validation.json). The five DOT-T
archives contain 1,150 entries in total, including 216 empty entries. All previous
586 TMD and 300 TIM hits retain their offsets; Croc retains its 32 VAB hits, names
and relative offsets. The measured session is evidence that this first bounded
delivery was manageable; it does not establish the cost of unfamiliar variants.

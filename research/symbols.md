# PsyQ function naming

`psx symbols <exe | disc.cue::/PATH> [--sigs external/psx_psyq_signatures]` writes three files:

- `.psx/symbols/<exe>.symbols.json`: the report, plus the source executable and its sha256
- `<exe>.ghidra.txt`: input for Ghidra's `ImportSymbolsScript.py`, one `NAME ADDR f|l` per line
- `<exe>.sym`: `ADDR NAME` lines

## Signature data

The signatures come from `lab313ru/psx_psyq_signatures` @ `e9e46e7e133ef275a79bfce650924f98edb086bc`, which covers PsyQ 2.6–4.7.

- **Local only.** The checkout lives in `external/` and is ignored by git. The repository has no licence, and the signatures are
  derived from Sony's PsyQ libraries, so they are never committed or bundled.
- **Format.** Each version has one JSON file per library. Each object has a byte pattern with `??` for relocated bytes, plus labels at offsets
  within it. Objects with no code have no `sig` and are skipped.

## Matching rules

- **Placement.** Only instruction-aligned placements count. The search is anchored on the object's rarest concrete instruction word.
- **Too short.** Objects with fewer than `--min-concrete` (default 24) non-wildcard bytes are skipped.
- **Ambiguous.** Objects that match in more than one place are listed as ambiguous and not used.
- **Version choice.** The version with the most uniquely matched bytes is chosen. Other versions only fill ranges that are not already covered,
  and their `source` records the version they came from.

## Results (Croc PAL)

| Executable | Chosen version | Objects | Functions | Code covered |
|---|---|---|---|---|
| `CROC.EXE` | 4.0 (166 unique vs 115 for 3.7) | 164 (LIBSND 79, LIBSPU 34, LIBGTE 25, LIBAPI 6, LIBCD 5, LIBETC 5, …) | 412 | 20.3 % |
| `SLES_005.93` (loader) | 4.0 | 34 (LIBCD 15, LIBAPI 5, LIBETC 5, LIBPRESS 2, …) | 241 | 33.5 % |
| `tools/probes/basic` (PSYQo, not PsyQ) | n/a | 0 | 0 | 0 % (negative control) |

## Residency-checked naming

`psx run --symbols` and `psx loads --symbols` accept `.symbols.json` files, which are verified, and `.sym` files, which are unverified.

- **Residency check.** A PC is named only if the executable's bytes around it (±16 bytes) match the run's RAM.
- **Later loads.** In `psx loads`, a PC is also left unnamed if the run's own provenance shows that executable being loaded over
  the PC *after* the event.

Before this rule existed, the BIOS shell code at `0x80067770` (Croc boot, frames 509–523) was misnamed
`_SpuRSetVoiceAttr+0x494` from `CROC.EXE`, which occupies the same addresses later.

With the rule in place, the load spans are named as follows:

- **Sector reads:** `CD_getsector+0xAC` (libcd), in both the loader and `CROC.EXE`.
- **Movie streaming:** `dma_execute+0x184` (libcd).
- **BIOS ROM PCs:** unnamed, because there is no BIOS symbol table yet.

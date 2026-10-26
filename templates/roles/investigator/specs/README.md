# Specs

Behaviour specifications of {{GAME}}, written by the investigator for the
clean implementation. This folder is what crosses to the implementer, and
only through publishing, as the reviewer approved it: the specs, the
[glossary](glossary.md), the [index](INDEX.md), the case files in
`cases/` and the start-state descriptions in `states/`.

## Rules for a spec

- Behaviour in game terms, organised as a player would describe it, not
  by the original's functions.
- Values are observations: say how each was observed (a case, the game's
  data, a measurement). Values that come from the data files are named as
  such, so the implementation reads them from the data rather than
  copying them.
- No code, pseudocode, control flow, addresses or the original's
  identifiers. Field and concept names come from the glossary.
- Small algorithms are given as test vectors, not formulas.
- Every claim is backed by a case or marked *inferred*.
- No advice on how the port should be structured: the implementer decides
  that. A spec organised state by state describes the game; it is not a
  design.

Where an observation came from in the original's code is kept on the
investigator's side, in `notes/provenance.json`, keyed by spec id.

## Kinds

`SPEC-NNNN` specs describe behaviour; `FMT-NNNN` specs describe file
formats on the disc. Both use the same front matter and rules.

## Front matter

```yaml
id: SPEC-0001
title: Stopping from a run
status: draft            # draft | approved
depends: []              # specs this one builds on
cases: [run_stop, run_stop_heldout]
```

A format spec also names the disc files it describes and its record types
with their number of fields, which the ledger weighs it by:

```yaml
files: [LEVELS/*.DAT]
records: {header: 6, room: 14, object: 9}
```

## Case files

`cases/NAME.json` describes one recording of the original, and
`cases/NAME.csv` holds its observations, one row per display field:

| Key | Meaning |
|---|---|
| `id` | the case name |
| `spec` | the spec it supports |
| `role` | `heldout` (recorded after the behaviour was understood, to test it), `calibration` (used to work it out) or `diagnostic` (not a pass/fail test) |
| `subject` | what is observed, in glossary terms |
| `start` | how the port's state is set up: `{"from": "first_row"}` (from the first observation), or `{"state": NAME}`, the published start state `states/NAME.json` |
| `input` | pad input in fields from the start: `{frame, buttons, until}`, `until` exclusive |
| `observations` | the CSV |
| `compare` | the columns a test compares, each with its tolerance in the column's units |
| `fields_per_tick` | display fields per game tick in steady play |
| `timing` | per row, `ticks_run` (ticks completed since the row before) and `reads_field` (the field those ticks ran in and read the pad of) |
| `coverage` | per spec, the columns it compares and the rows it claims |

**Timing.** Row 0 is the subject before any tick. The `tick` column is the
number of ticks the game has completed since row 0; it decides how many
ticks each row is the result of. The game may run ticks unevenly when
drawing falls behind: a row may follow two ticks at once, and several
rows none. Every tick completed between rows `r − 1` and `r` ran during
field `r − 1` and reads that field's pad (`timing` gives both per row).
Check a case runner against `timing` before testing any behaviour.

The investigator records the cases, and records them again whenever the
emulator changes, to check they still hold.

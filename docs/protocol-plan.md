# Plan: the porting protocol in psx

Working towards the brief written by the Tomb Raider II port
(`tr2-rs/docs/psx-protocol-brief.md`, version 2): psx should let an agent
port a PlayStation game by itself, clean-room, with measurable progress.
Everything here is general to PlayStation games; nothing is specific to
Tomb Raider II. Each piece is proven on two games (Tomb Raider II and
Crash Bandicoot) before it is called done.

The brief's own rule orders the work: build what the two loops tested
first (the evidence tools, the case format, publishing, the message check,
the role templates); the reviewer gate, the audits, the percentage and
clean mode come after, because they are still proposals.

Branch: `protocol`. Committed piece by piece.

## Done

- **Field map** (`samples.toml` or `run --map`): globals, struct layouts,
  subjects and arrays by name; `run --sample` takes names and reports
  names only.
- **The game's tick**: `[tick]` in the map counts a store in the core and
  adds a `tick` column to samples.csv, with ticks per field in the report.
- **Finding the tick**: `run --find-tick` groups every CPU store by the
  fields it fell in; `samples.tick.changed_without_tick` checks a marker.
  Guide topic `fields` has the recipe. (Brief item 6.)
- **Waiting on a state**: `run --until NAME=VALUE` (and != < <= > >=).
  (Brief item 7.)
- **Recording a case**: `case record` writes the case JSON, the
  observation CSV with the tick column, and a recipe; `case check`
  records cases again from their recipes and compares them cell by cell.
  Guide topic `cases`. (Brief item 4.)

## Phase 1: evidence tools

1. **Handoff checks** (item 8). A handoff request takes `expect`
   conditions in the `until` syntax (`lara.room=16`, `health>500`); the
   result says whether each held at the hand-back, with the values, and a
   plain verdict on whether to ask again.
2. **Start-state chains and rebuilds** (item 12, and "recovering on its
   own"). Every saved state records how it was made: its parent state (or
   power-on), the input, any `until` conditions, the psx build. `psx
   states rebuild` replays a chain from power-on on the current build and
   compares named samples at the end; old runs are kept by the emulator
   version that made them; `case check` after a rebuild gives the triage
   list (instrument change, or real divergence). Model:
   `tr2-rs/tools/recordings.py rebuild-states`.
3. **Case format additions** the second implementer asked for: timing
   vectors (rows to ticks run and the field each tick read, from the tick
   column) and an optional per-case `coverage` key (per spec, the columns
   and rows it claims).

## Phase 2: the barrier

4. **One leak checker** shared by everything at the barrier: addresses
   and hex constants, the original's symbol names (from psx's own Ghidra
   and PsyQ analysis), code-shaped text, and runs of eight words shared
   with the investigator's notes.
5. **`psx publish --stage` and `psx publish`** (item 3). Specs, cases,
   the sample glossary (names, units, meanings) and start-state
   descriptions are bundled with a hash; a bundle crosses only when an
   approval of that exact hash exists (`reviews/<hash>.json`); every
   publish checks that the implementer's `specs/` is the union of the
   approved bundles. Models: `tr2-rs/tools/publish_cases.py`,
   `tr2-investigate/tools/publish_specs.py`.
6. **The message check as a psx command** (`psx barrier hook`), so the
   implementer's `UserPromptSubmit` hook is one line: the same checker,
   `[cross]` passing with a log entry, `guidance.jsonl` hash-chained.
   Model: `tr2-clean/.claude/hooks/barrier_check.py` and `barrier.json`.

## Phase 3: workspaces and guidance

7. **`template create --role investigator|implementer|reviewer`**, then
   **`psx port init`** (item 1): three repositories side by side, each
   role's `AGENTS.md` (the implementer's with its own architecture
   section), access rules and the message check with the real paths, the
   disc's data files (not the executable) in the implementer's `data/`,
   the protocol settings. Today's single-repository layout stays the
   default. Models: `tr2-investigate/`, `tr2-clean/`.
8. **Guide topics** (section 4): new `protocol`, `cleanroom` and
   `review`; `overview`, `port` and `troubleshooting` changed. Includes
   the defaults for making cases that separate, finding things in RAM,
   the port's own tools, and reporting gaps by the "different game" test.

## Phase 4: measuring and driving

9. **`psx inventory`** (item 9): every function and overlay with its
   instruction count and basic blocks, including functions reached only
   through pointer tables (analysis seeded from RAM snapshots), plus the
   disc's file list. The ledger's denominator.
10. **Corpus coverage** (item 10): runs' `coverage.txt` merged into
    executed blocks per function, so "exercised" is a query and the
    unexecuted blocks are a list to classify.
11. **The ledger and `psx protocol next --role R`** (item 2): units,
    stages and exit checks; questions and choices given as work; audits
    due; an emulator change makes rebuilding the next step.
12. **`psx protocol report`** (`--plain` for the person) (item 11) and
    **`psx audit`** (item 13): the mechanical checks of section 2 (above
    all, every number in the implementer's code appears in a spec, a case
    or the game's data), the report kept with the hashes and commits it
    covered, the verdict appended to the port's `AUDIT.md`.

## Phase 5: clean mode

13. **`psx clean`** (item 5): a separate binary and MCP server that
    physically lacks every tainted command, so the implementer's access
    rules can allow it while denying psx. `record` (an exploratory case
    from a published start), `compare` (against the port's output),
    `heldout` (pass or fail per kept-back case, the runner's output not
    returned) and side-by-side screenshots; answers only in published
    names; every request logged to the implementer's `notes/oracle.jsonl`.

## Defaults taken

- The ledger and protocol settings live in the investigator's repository
  (`protocol.toml`, `ledger.json`); `next` and `report` read only those
  and the repositories' files.
- Clean mode is its own binary crate (`psx-clean`), not a flag: access
  rules allow or deny whole servers.

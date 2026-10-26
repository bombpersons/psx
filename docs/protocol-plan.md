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

- **Handoff checks**: `handoff --expect` in the `until` syntax; the
  result gives each condition's value at the hand-back, a verdict and
  `ask_again`. (Brief item 8.)
- **Start-state chains and rebuilds**: `states make` / `states save` name a
  state (`.psx/states/NAME.bin`) and write its chain of runs from power-on
  (`notes/states/NAME.json`, from the runs' reports); `states rebuild`
  replays chains on the current build (shared steps once), gives the first
  differing step and named values old and new, and with `replace` keeps
  the old states by the build that made them. Guide topic `states`. TR2's
  `courtyard` and `open` rebuild byte for byte from power-on. (Brief item
  12.)
- **Case format additions**: the case's `timing` vectors (per row, the
  ticks run and the field they read) and `coverage` (per spec, the
  compared columns and the rows it claims, rows ending at an `until` on
  the case's columns). `case check` adds them to older cases and keeps
  keys added by hand. (Section 6.)
- **The leak checker** (`psx barrier check`, `leak.rs`, shared by publish
  and the message check to come): addresses, hex constants, code-shaped
  text (C-like statements, MIPS, decompiler output, code blocks), tool
  names and disc serials, the original's symbol names (non-default Ghidra
  names and `psx symbols` output in `.psx`, or given files), names given
  to refuse, pointer-valued CSV columns, and runs of eight words shared
  with the notes but not published. Guide topic `barrier`. tr2-investigate's
  and tr2-clean's specs are clean; its notes and planted leaks in TR2 and
  Crash text are caught.
- **Publishing** (`psx publish stage` / `cross` / `check`): stage takes
  what changed in the investigator's `specs/` since the last crossing
  (specs, glossary, cases with their observations, start-state
  descriptions from the chains; a case marked `kept_back` stays behind),
  runs the leak checker and writes `.psx/bundles/HASH/`, the hash covering
  each file and the version it replaces. `cross` copies a bundle only when
  the reviewer's `reviews/HASH.json` approves it, logging it to the
  implementer's `notes/published.jsonl` (hash-chained); `check` and every
  crossing test that `specs/` is exactly the logged, still-approved
  bundles. Tried on copies of TR2's repositories (adopting tr2-clean's
  existing specs, partial stages, stale bundles, removals, a tampered
  bundle, a rewritten review, an edited log) and on a Crash spec, case
  and state. (Brief item 3.)
- **The message check** (`psx barrier hook`): the implementer's
  `UserPromptSubmit` hook is one line. The same checker, with settings in
  `.claude/barrier.json` (the investigator's repository for its symbols
  and notes; the first hook's keys still read); every message logged to
  `notes/guidance.jsonl`, hash-chained, as guidance, stopped (reasons
  only) or crossing (`[cross]`, and the agent told to make it a question);
  stopped when the check cannot run. TR2's two logged messages pass;
  addresses, tool and library names, copied notes are stopped in TR2 and
  Crash messages, and text already published passes.

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

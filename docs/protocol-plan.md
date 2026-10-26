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
- **The roles and `psx port init`** (brief items 1 and section 5): `port
  init` writes the investigator's, implementer's and reviewer's
  repositories side by side (`investigate/`, `clean/`, `review/`): each
  role's `AGENTS.md`; the investigator's spec rules, glossary sections,
  index, provenance, logs and recipe folders; the implementer's Bevy
  workspace with its own architecture section and its guidance rewritten
  for specs and cases (no reverse-engineering framing; `template update`
  keeps it), `questions/` and `data/` (the disc without `SYSTEM.CNF` and
  every PS-X EXE); access rules per role with the real paths, the message
  hook wired in; the disc and BIOS linked for the investigator;
  `protocol.toml`, which `publish` and the barrier read. `template create
  --role` writes one role alone. Tried on TR2 (four executables left out,
  78 data files) and Crash; publishing with no paths given works from the
  new repositories.
- **The case runner** (section 5, "Both: the case runner"):
  `psx_bevy::testing::Case` loads a case, runs as many ticks before each
  row as its `tick` column says with the pad of the field before, checks
  the ticks it ran against the case's `timing` before judging behaviour,
  and compares only the claims (`coverage`) of the specs the port says it
  implements, listing the rest as waiting; a diagnostic case never fails,
  an exploratory case without a spec is always compared, and
  `write_run` writes the port's own run in the case's form. The
  template's `game/tests/cases.rs` shows it on a case recorded from the
  demo (a caught-up tick, a repeated row, coverage split between two
  specs). tr2-clean's case tests rewritten on it pass the same 62 cases,
  with tr2-investigate's timing vectors; a planted vector and a planted
  cell are caught.
- **Guide topics** (section 4): `protocol` (what complete means, units,
  stages and credits, the loop, making cases that separate, finding things
  in RAM, play-throughs, emulator changes, the port's own tools, reporting
  gaps by the "different game" test; the ledger kept by hand in
  `notes/LOOP.md` until phase 4), `cleanroom` (roles, what may cross, the
  disc, tables in the executable, small algorithms, questions and choices,
  people and agent hosts, records, migrating an existing port) and
  `review` (what the reviewer writes and to whom, the bundle checklist with
  restated rules and unnamed data values, audits, sending things back).
  `overview` gains the clean-room route, `port` says where it sits under
  the protocol and records features as cases, `troubleshooting` covers
  menus that ignore short presses and interrupted analyses. The
  investigator's and reviewer's `AGENTS.md` start from the new topics.
  `overview`'s "start with `psx protocol next`" waits for item 11.

- **`psx inventory`** (item 9): every function of every PS-X EXE on the
  disc and of the overlays in RAM snapshots, with instructions, basic
  blocks, how each was found and how it ends, the gaps no function covers
  (with how many words decode as code), and the disc's file list; written
  to `notes/inventory.json` beside `protocol.toml` with totals appended to
  `inventory_history.jsonl` (for the discovery gate). psx's own analysis:
  the entry, calls in functions found, starts after a return or with a
  prologue, pointers in the executable and in snapshots holding it, traced
  call targets; words a compiler never emits (writes to `$zero`, shifts of
  it, undecodable delay slots) end a candidate. An overlay is code a
  snapshot holds that no executable does, matched to the disc file it was
  loaded from and the part RAM still holds. TR2: all 162 state handlers
  its investigator collected by hand are found from `TOMB2.EXE` alone
  (Ghidra's static analysis found 92), 1271 of Ghidra's 1272 functions
  from its RAM-seeded analysis; Symphony of the Night's `DRA.BIN` at
  `800A0000` and `SEL.BIN` at `80180000` found from one traced snapshot,
  and every instruction traced runs executed in game RAM lies in a found
  function (Crash and Symphony of the Night).
- **Corpus coverage** (`psx coverage RUN...`): traced runs write
  `executed.bin` (each executed RAM word with the instruction it ran as);
  coverage credits each word to the executable or overlay holding a
  stretch of what ran there and writes `coverage.json` beside the
  inventory: per function its blocks executed and not, calls and the runs
  reaching it, with executed code in no function or in no image (and the
  run frames it was called in). The inventory now takes traced call
  targets and executed code only where the code at hand is what ran,
  finds code that ran and was loaded over in the disc's files, splits code
  loaded over part of a file into overlays of its own, keeps the BIOS
  shell apart (`bios_code`), allows loads into the zero register and ends
  candidates at eight nops. TR2's boot, Crash's first level and Symphony
  of the Night's title: every executed word in a function but 6 and 115
  short fragments (TR2, Crash), all 162 TR2 handlers still found; in
  Symphony of the Night the shell is told from the game at the same
  addresses.

## Phase 3: workspaces and guidance

Done.

## Phase 4: measuring and driving

11. **The ledger and `psx protocol next --role R`** (item 2): units,
    stages and exit checks; questions and choices given as work; audits
    due; an emulator change makes rebuilding the next step. `overview`
    then starts with `protocol next`, and `protocol`'s hand-kept ledger
    paragraph goes.
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

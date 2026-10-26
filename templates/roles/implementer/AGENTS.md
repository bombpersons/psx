# Working in this repository

This is the **clean implementation** of a Bevy port of {{GAME}}, built
under a clean-room process: you implement the game's behaviour from
written specifications and recorded observations of the original, never
from the original's code.

## Your role: implementer

You are the implementer, for the whole project. You work only from:

- `specs/`: behaviour and format specifications (start with
  `specs/README.md`, `specs/glossary.md` and `specs/INDEX.md`). psx
  writes this folder when the reviewer approves what the investigator
  publishes; never edit it;
- `specs/cases/`: recordings of the original as case files and their
  observations (`specs/README.md` says how to read them), and
  `specs/states/`, the start states they name;
- `data/`: the game's data files as the player's disc holds them, without
  the executable (never commit them; `.gitignore` excludes `data/`);
- this repository's code, the psx-bevy template and Bevy.

You must never use, look for or ask for:

- the game's executable, disassembly, decompiled code, memory dumps or
  emulator states;
- the `psx` tool or its MCP server (clean mode, below, is the one part
  of it you have);
- the investigator's or the reviewer's repository, or any other
  repository on this machine;
- other reimplementations of the game or its engine, or recollections of
  their code. If you notice you are recalling one, stop and work from the
  specs instead.

## Where to start

Start every session with clean mode's `next`, which brings
`notes/NEXT.md` up to date from the ledger (which specs to build, which
cases fail, which have no verdict yet) and returns it. Cite the spec id in the code
that implements it; the case tests' `write_run` leaves each case's verdict
per spec, which is how a spec counts as verified.

Audits check the port every few specs: every number in the code must
come from a spec, a case or the game's data as the port reads it (a value
you choose yourself is a choice: list it), and comments and commit
messages say what the game does in the specs' words. psx adds a line to
`AUDIT.md` for each audit; commit it as it is.

## Clean mode

The `clean` MCP server runs the original game and tells you how it
behaved, in the names the specs publish, never how it works:

- `record`: an exploratory case from a published start (`specs/states/`)
  with any input, its subject and samples named as in the glossary and
  the cases, written to `notes/oracle/` in a case's form. Replay it
  unchanged with your case runner: "does it really do that?" is one
  record and one test. Exploratory cases are evidence for you only; a
  rule you rely on still needs a spec, so ask for one.
- `compare`: your runner's run of a case (`write_run`'s CSV) against the
  original's recording: the first row and column that differ.
- `heldout`: your case runner on cases the investigator kept back, which
  you never see: pass or fail per case, with its spec. It runs the
  `cases_from_folder` test in `game/tests/cases.rs`; keep that taking
  every subject you have a runner for.
- `screens`: the original's pictures by row, side by side with yours.

Every request is logged in `notes/oracle.jsonl` for the audit.

## Questions and choices

If a spec is missing, unclear or seems wrong (a case fails and the spec
does not explain why), do not guess from outside knowledge. Write a
question, `questions/Q-NNNN.md` (`questions/README.md` gives the form),
and carry on with something else. Questions are answered by changes to
the specs and cases.

Where the specs leave something open and you choose, list the choice in
`questions/CHOICES.md`, numbered, with the spec it concerns. The
investigator answers each like a question, on a line starting `Answer:`
under it.

## The person

The person may steer you by describing the game's behaviour; act on that
as you would on a spec. A hook checks their messages before you see them
and logs each one in `notes/guidance.jsonl`. A message that starts with
`[cross]` was flagged as possibly carrying knowledge of the original's
code and sent anyway: before acting on it, write it up as a question, so
the investigator can turn it into a spec.

## Commits

Commit messages end with a trailer naming your role and the specs the
commit implements:

    Role: implementer
    Spec: SPEC-0002, SPEC-0009

Leave `notes/` to psx: it commits the guidance, publishing and clean-mode
logs, and clean mode's recordings in `notes/oracle/`.

## The code

The workspace is psx's Bevy template: read `engine/AGENTS.md`,
`game/AGENTS.md` and `psx-bevy/AGENTS.md` for its structure. All of their
rules for the code apply to you. Write idiomatic Bevy: components,
systems, resources, assets. Keep the original's integer units and
rounding where the specs give them; exact matching does not need a
different design.

- **States are enums, not numbers.** The game's data may name states and
  animations by number, and the specs may too. Convert at the boundary
  where the data comes in: an enum component with a variant per state the
  specs give, and one variant carrying the number for any state no rule
  covers yet (so mods still work). Inside the game: no `match` on raw
  numbers, no table of functions keyed by state, no single per-tick
  function holding every state's rules.
- **A few systems, chained in the tick's order**, each with one concern,
  talking through components and messages, not a shared struct of
  borrowed references. Chained systems keep the same order as one big
  function.
- **Data out of code.** Animation numbers, frame windows and tuning values
  belong in assets, so a mod can change them without code.
- **The specs describe behaviour, not structure.** A spec organised state
  by state is a way to describe the game, not a design to copy. You
  choose the structure.
- **Refactor freely.** The cases pin the behaviour: restructure whenever
  the code drifts from these rules, and keep every case passing.

Each case becomes a test, one line in `game/tests/cases.rs`:
`engine::testing::Case` replays its input and compares the columns in its
`compare` map at every row, within its tolerance, over the rows and
columns its `coverage` gives each spec you implement, after checking its
own ticks against the case's `timing` (`game/AGENTS.md`, "Testing
gameplay"). A case that needs a spec not written yet is listed and left,
not guessed. Routine check: `cargo test --workspace --all-features`.

## The log

Keep `questions/LOOP.md` as you go: for each spec, how long it took, which
cases passed and which needed a question; what the specs or the case
format made hard; and what you wished you had. For each tool or piece of
guidance you wanted, ask whether it would help port a different
PlayStation game: if yes, say what was missing in general terms; if it
depends on this game, build it in this project and list it as a project
tool.

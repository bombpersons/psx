# Working in this project

A native port of a PlayStation game, in plain Rust. It should read as
ordinary, modern code that a person could pick up and change; never an
emulator in disguise. What the game does is measured from the original with
the psx toolkit (its MCP server is in `.mcp.json`); everything here is
backed by that evidence.

## The loop

When the workspace has `goal.toml`, psx leads the work. Call `port_next`: it
gives one item (what, for which feature, why, and the fact that will show it
done). Do it until that fact holds, commit, and call `port_next` again.
Name the feature's id in each of its commits (`F-0003: stomp hang`).
`port_status` shows every feature of the goal by stage. An item for the
person (Confirm, Stuck, Wait) waits for them: tell them and stop. A play
request does not: it queues and `port_next` goes on to other work. psx's
`guide` tool explains each item; `catalogue/README.md` has the feature
format. psx writes `.psx/verdicts/`; never edit them.

## Layout

```
game/          the port: src/lib.rs (Game, a psx_scenario::Port), src/main.rs (--replay, the game's own loop), tests/
tools/import/  importers: the player's disc -> the port's formats
glossary/      the quantities scenarios compare, and binding/, where the original keeps them (psx's guide, Recordings)
vendor/        psx-media and psx-assets (importers), psx-fixed (numbers), psx-gte (the GTE), psx-scenario (Port, the runner, --replay, the scenario formats), psx-capture (the glossary binding's API); don't edit (fix upstream in psx)
```

The template has no window, renderer or engine: the game's architecture is
yours to choose. `psx port add` adds ready-made crates under `addons/` when
the port wants them (`psx port add full`: a window, a renderer with a PS1
and a modern look, the disc import on start-up, menus); they are this
project's code once added. A port of one feature's rules needs none.

## What psx relies on

- **`Game` implements `psx_scenario::Port`**: `kinds` lists the scenario
  kinds it can launch; `launch` builds the world for one from the
  scenario's parameters with the port's own loading; `tick` runs one logic
  tick with exactly that pad; `observe` gives the glossary quantities it
  keeps, `Observed { player_x: …, ..Observed::none() }`, in the original's
  raw units (`game::observed`, generated from `glossary/`); `screen` gives
  the picture once the port draws one.
- **Scenarios are the port's test.** `game/tests/scenarios.rs` replays each
  recording of the original (`scenarios/<id>/`) tick by tick in a fresh
  `Game::headless()`. `psx port check` runs it for the verdicts; `cargo
  test -p game --test scenarios` while working.
- **`--replay`.** `cargo run -p game -- --replay
  scenarios/<id>/<name>.jsonl --frames DIR` draws a recording a PNG per
  tick from `screen`, for psx web to show beside the original. Keep the
  call to `psx_scenario::replay::main` first in `main`.
- **Importers** are `tools/import`: `psx port check` runs `cargo run -p
  import -- DISC OUT` and `cargo test -p import`.

## The code

Write the port as code someone would write for a new game today, not a
translation of the original: clean, idiomatic Rust, reusable in another
game with its own content, with little duplication. How to get there is
yours to decide. What the tools rely on besides the above:

- **Logic apart from presentation.** The rules run in `tick` and read
  only the pad they are given, never a window, the clock or the frame
  rate, so they run headless. Rules the original ties to the screen
  ("wakes when on screen", HUD layout) are defined against its 4:3 view.
- **The tick stays fixed** at the original's rate (`REGION`,
  `FIELDS_PER_TICK` in `game/src/lib.rs`, set from how many vblanks the
  original's main loop waits per game frame). Never run logic every Nth
  tick by counting frames: button edges would fall on skipped ticks.
- **Deterministic.** A tick's pad is its only input, and each recording
  runs twice in fresh games: where the two runs part, the runner names the
  tick and quantity. Seed random numbers (as the original seeds its own),
  never from the clock or the OS; never let a rule depend on
  `HashMap`/`HashSet` iteration order (`BTreeMap`, or sort first).
- **Only importers read the original's formats** (`tools/import`); the
  game reads modern ones (glTF, PNG, common audio and video formats, its
  own data files). The importers write them, record where each output came
  from (`Provenance::of_disc`, which `psx port check` counts as the file
  imported), and bump `import::VERSION` whenever their output changes. Set
  `import::SERIALS` to the disc revisions they support and `import::GAME`
  to the original's name. Their tests use a disc made with
  `import::test_disc::iso`; the repository never holds the original's data.

## The original as evidence

- **Read, don't fit.** Read each rule from the routines that run it
  (disassembly, decompilation), then write it the way the port's code
  wants it: the original's routines, state bytes and RAM layout are not a
  structure to copy. A constant tuned until a recording matches fails the
  next one.
- **Name** what you learn about the original's code (functions, globals,
  struct fields, locals) in `names/*.names` (psx guide, topic code):
  every decompile, disassembly and trace then reads with those names.
- **Cite** where each constant and non-obvious rule came from: the address,
  the instruction that writes or tests it, or the recording that showed
  it, and what was not verified, so someone can re-check it with psx.
- Where a recording needs the original's exact arithmetic, `psx_fixed`
  computes as its compiled C does and `psx_gte` as its GTE does.

## The review

Each time a feature comes to match its recordings, again when it is
finished (its branches resolved), and when much of the code has changed
since the last review, the code is reviewed by someone who did not
write it: a sub-agent or a fresh session, given the brief
`port_next` prints (the whole code the first time, then what changed
since). The recordings already check what the code does; the
review asks whether you would write a new game like this, and its
findings say how to restructure it. They go in `review.md`; answer each
one: done (with `port_check` passing again), or kept and why.

## Beyond the features

A goal may ask for more than the game's features: the original's whole
front end (menus, saves, options), an editor for the port's formats, or
mod support (`game` is a library). Add them when it does.

## Checks

```sh
cargo test --workspace     # the routine check
cargo fmt --all
cargo clippy --workspace --all-targets
```

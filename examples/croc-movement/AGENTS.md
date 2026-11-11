# Working in this project

A native port of a PlayStation game, in plain Rust. It should read as
ordinary, modern code that a person could pick up and change; never an
emulator in disguise. What the game does is measured from the original with
the psx toolkit (its MCP server is in `.mcp.json`); everything here is
backed by that evidence.

## How the work goes

`goal.toml` says what the person wants. The recordings of the original
say what's right: `port_check` replays them against the port and gives,
for each, the first difference (the original's value and the port's) and
the code that wrote the value; `scenario_show` with `port=true` puts the
port's values beside the original's around it. Passing
them is the test. `port_status` is information: what passes, what the
recordings ran and the port cites, plays waiting or answered. psx's
`guide` tool is the reference for the commands. psx writes
`.psx/verdicts/`; never edit them.

What to do next is yours to choose: say what you picked and why, and go
where the person points when they do. Keep `STATUS.md` current. The person
decides when it's done, by playing it; `scenario_play` asks them to play
something, and it queues, so carry on with other work.

Take one thing per session: a recording's first difference, or one piece
the person asked for. When it's done, or you've found its cause but can't
fix it yet, commit, run `port_check`, write what you learned in `notes/`,
update STATUS.md's "Next, and why" so a session that has never seen this
work can start from it, and stop. The next session starts from STATUS.md.

Ask the person for a play of the goal with `scenario_play` as soon as the
first recordings work: one long play shows far more of the game than short
recordings you make yourself. Use it as the map, not the work. When its
first difference shows something not yet ported, record a few short
recordings of that thing yourself, starting near it (`scenario_record`
with `from` set to the play and a tick before it) and varying how it
happens, and port it until they pass; then come back to the play to see it
get further. A rule built from one moment of a long play rarely holds at the
next, and the short recordings keep it checked once it works.

Ask for a held-out play too (`scenario_play` with `held_out=true`), on a
route your work recordings don't take, and another whenever a piece the
person asked for works. `port_check` judges the port on it and
`port_status` counts it, but you never see it: it shows the person whether
the rules hold beyond the recordings they were built from. The person's
own plays are held out unless they give them to you. When they tell you
one fails, a rule was fitted to what you had: read its code again and
record that case yourself; never try to see the held-out play.

Choose recordings by what they show. `port_status` with `branches=true`
lists the branches in the code the recordings ran that they take only one
way: each is a case no recording has shown yet, and a short recording that
takes the other way checks the rule there. Write the ones no recording can
reach in STATUS.md, with why.

Recordings compare what a player could notice: where things are and which
way they face, what's drawn (the camera, the animation playing and its
frame, the model or sprite shown), and what the game shows or plays (the
HUD's numbers, a sound starting). Add each to `glossary/` and the
scenarios' `[compare]` as soon as it can be measured, and refresh the
recordings, so `port_check` compares it from then on. A recording that
starts failing because of it has found work to do.

The original's counters, timers, flags, phases and script variables are
how it gets there, not what the port must match, so they aren't compared:
the port can reach the same results its own way. To learn a rule, watch
them in the original (debugger watches, the `ram_*` tools) while you trace,
then check what you wrote through what a player could notice. When unsure,
ask whether a player could tell the difference by watching and listening.

Commit as you go and run `port_check` on the commits. It judges only a
commit with no uncommitted changes, and `cargo test` doesn't change what
`port_status` shows.

Recordings don't compare the picture. When what's drawn has changed, run
`port_status` with `screens=true`: it scores how far the port's picture is
from the original's and names the worst moment, for `scenario_show` with
`screens` to show you. Things not drawn yet always count, so watch the
trend.

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
project's code once added. A port of a few rules needs none.

## What psx relies on

- **`Game` implements `psx_scenario::Port`**: `kinds` lists the scenario
  kinds it can launch; `launch` builds the world for one from the
  scenario's parameters with the port's own loading; `tick` runs one logic
  tick with exactly that pad; `observe` gives the glossary quantities it
  keeps, `Observed { player_x: …, ..Observed::none() }`, in the original's
  raw units (`game::observed`, generated from `glossary/`); `screen` gives
  the picture once the port draws one.
- **Scenarios are the port's test.** Each recording of the original
  (`scenarios/<id>/`) is replayed tick by tick in a fresh
  `Game::headless()`. `psx port check` runs them against the game's
  program, which `src/main.rs` serves with `--psx`, for the verdicts;
  `cargo test -p game --test scenarios` (`game/tests/scenarios.rs`) runs
  them the same way while working.
- **`--replay`.** `cargo run -p game -- --replay
  scenarios/<id>/<name>.jsonl --frames DIR` draws a recording a PNG per
  tick from `screen`, for psx web to show beside the original. Keep the
  call to `psx_scenario::replay::main` first in `main`.
- **Importers** are `tools/import`: `psx port check` runs `cargo run -p
  import -- DISC OUT` and `cargo test -p import`, as `psx-port.toml`
  names them.

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

When much of the code has changed, `port_check` says a review is due and
`port_status` gives the brief for someone who didn't write it: a sub-agent
or a fresh session. The recordings already check what the code does; the
review asks whether you would write a new game like this. Findings go in
`review.md`, each answered: done, or kept and why. Nothing waits on it.

## Beyond the recordings

A goal may ask for what no recording checks: the original's whole
front end (menus, saves, options), an editor for the port's formats, or
mod support (`game` is a library). Add them when it does.

## Checks

```sh
cargo test --workspace     # the routine check
cargo fmt --all
cargo clippy --workspace --all-targets
```

## Add-ons

Crates `psx port add` wrote. They are this project's code: use them as they are, change them, or move what you want into game/ and delete the rest. `psx port update` offers newer versions beside them as `.psx-new` files.

- `addons/shell/` (`psx port add shell`): the window (windowed or borderless fullscreen, any size), the fixed-step loop at the port's tick rate, and the pad from keyboard and gamepads with psx play's bindings
- `addons/renderer/` (`psx port add renderer`): wgpu drawing in a PS1 look and a modern look: glTF models, textures, a camera, lights, fog and sprites (`cargo run -p renderer --example view -- MODEL.gltf`)
- `addons/disc-import/` (`psx port add disc-import`): the player's disc imported by tools/import on first start, with a screen that asks for it
- `addons/menus/` (`psx port add menus`): F1 controls and F2 display menus over the game, saved to the player's settings
- `full` (`psx port add full`): the four above wired in game/src/main.rs; game/src/demo.rs stands in for the game's drawing

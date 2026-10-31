# Working in this project

A native Bevy port of a PlayStation game. It should read as ordinary, modern
Rust and Bevy that a person could pick up and change; never an emulator in
disguise. What the game does is measured from the original with the psx
toolkit (its MCP server is in `.mcp.json`); everything here is backed by that
evidence.

## The loop

When the workspace has `goal.toml`, psx leads the work. Call `port_next`: it
gives one item (what, for which feature, why, and the fact that will show it
done). Do it until that fact holds, commit, and call `port_next` again.
Name the feature's id in each of its commits (`F-0003: stomp hang`).
`port_status` shows every
feature of the goal by stage. An item for the person (Confirm, Play,
Stuck) waits for them: tell them and stop. psx's `guide`
tool explains each item; `catalogue/README.md` has the feature
format. psx writes `.psx/verdicts/`; never edit them.

## Layout

```
game/          the port, its assets/ and tests/ (game/AGENTS.md)
tools/import/  importers: the player's disc -> the port's asset formats
glossary/      the quantities scenarios compare, and binding/, where the original keeps them (psx's guide, Recordings)
psx-bevy/      the PS1 look, ticks, input, menus, testing, numbers (num, gte), scenarios and launcher; yours to change (psx-bevy/AGENTS.md)
vendor/        psx-media and psx-assets (importers), psx-fixed (numbers), psx-gte (the GTE), psx-scenario (the scenario formats and generated glue), psx-capture (the glossary binding's API); don't edit (fix upstream in psx)
docs/          template.md: your changes to psx-bevy/
```

## The code

Write the port as code someone would write for a new game today, not a
translation of the original: clean, idiomatic Rust and Bevy, reusable in
another game with its own content, with little duplication. How to get
there is yours to decide. What the tools rely on:

- **Logic apart from rendering.** Logic runs in `FixedUpdate`
  (`GameSystems::Game`) at the game's tick rate, reads `ControllerState`, and
  never reads the window, the camera's pixels or the frame rate, so it runs
  headless. Rules the original ties to the screen ("wakes when on screen",
  HUD layout) are defined against its 4:3 view; `PsxAspect` widens what is
  shown and `PsxAnchor` anchors the HUD.
- **The tick stays fixed** at the original's rate (`REGION`,
  `FIELDS_PER_TICK` in `game/src/lib.rs`).
- **Scenarios are the port's test.** The port registers a launcher for
  each scenario kind, which builds the world from the
  scenario's parameters with the port's own loading, and observes the
  glossary quantities it keeps: `app.observe(|w: &mut World| Observed {
  player_x: …, ..Observed::none() })`, in the original's raw units
  (`game::observed`, generated from `glossary/`; `psx_bevy::scenario`).
  `game/tests/scenarios.rs` replays each recording of the original
  (`scenarios/<id>/`) tick by tick with its pad, in a headless `app` with
  the game's logic. `psx port check` runs it for the verdicts; `cargo
  test -p game --test scenarios` while working.
- **Only importers read the original's formats** (`tools/import`); the
  game reads the port's own (RON, glTF, PNG, WAV/OGG).

## The original as evidence

- **Read, don't fit.** Read each rule from the routines that run it
  (disassembly, decompilation), then write it the way the port's code
  wants it: the original's routines, state bytes and RAM layout are not a
  structure to copy. A constant tuned until a recording matches fails the
  next one.
- **Cite** where each constant and non-obvious rule came from: the address,
  the instruction that writes or tests it, or the recording that showed
  it, and what was not verified, so someone can re-check it with psx.
- Where a recording needs the original's exact arithmetic,
  `psx_bevy::num` computes as its compiled C does (`Num`, `Fixed`,
  `Angle`) and `psx_bevy::gte` as its GTE does.

## The review

Once the goal's features work, the code is reviewed as a whole by someone
who did not write it: a sub-agent or a fresh session, given the brief
`port_next` prints. The recordings already check what the code does; the
review asks whether you would write a new game like this, and its
findings say how to restructure it. They go in `review.md`; answer each
one: done (with `port_check` passing again), or kept and why.

## Beyond the features

A goal may ask for more than the game's features: the original's whole
front end (menus, saves, options), an editor for the port's formats, or
mod support (`game` is a library, and psx-bevy's asset folders layer).
Add them when it does.

## Checks

```sh
cargo test --workspace --all-features    # the routine check
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo build --release --no-default-features   # shipping build
```

Iteration builds link Bevy as a shared library (default feature `dynamic`);
a debug build finds it, and the game's `assets/`, when started from
`target/` too (an MCP client running `target/debug/<game> --mcp`, the
executable named after the game; the package is `game`).

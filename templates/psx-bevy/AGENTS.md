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
`port_status` shows every feature of the goal by stage. An item for the
person (Confirm, Stuck) waits for them: tell them and stop. psx's `guide`
tool, topic `port`, explains each item; `catalogue/README.md` has the feature
format. psx writes `.psx/verdicts/`; never edit them.

## Layout

```
game/          the port: one plugin per feature, its assets/ and tests/ (game/AGENTS.md)
tools/import/  importers: the player's disc -> the port's asset formats
psx-bevy/      the PS1 look, ticks, input, menus, testing and launcher; yours to change (psx-bevy/AGENTS.md)
vendor/        psx-media and psx-assets for the importers; don't edit (fix upstream in psx)
docs/          template.md: your changes to psx-bevy/
```

## The shape of the code

- **One plugin per feature**, named after its catalogue entry, adding only
  the plugins its `depends` lists, and working with only those.
- **Logic apart from rendering.** Logic runs in `FixedUpdate`
  (`GameSystems::Game`) at the game's tick rate, reads `ControllerState`, and
  never reads the window, the camera's pixels or the frame rate. Rendering
  reads logic's state. Rules the original ties to the screen ("wakes when on
  screen", HUD layout) are defined against its 4:3 view; `PsxAspect` widens
  what is shown and `PsxAnchor` anchors the HUD.
- **Mechanics are pure steps:** a function of the state, inputs, params and
  world, called from a system that adapts components to it. The world is a
  trait the feature implements over the port's resources.
- **States are enums** with each state's data in its variant; transitions
  are assignments. No state bytes, function tables, or a CPU-like main loop.
- **Rules apart from content.** Tuning, setup and content are assets with a
  documented contract (RON, glTF, PNG, WAV/OGG), never constants in code.
  Assets hold real numbers (`grip = 1.375`, not `0x16000`). Only importers
  read the original's formats.
- **The tick stays fixed** at the original's rate (`REGION`,
  `FIELDS_PER_TICK` in `game/src/lib.rs`).

## Numbers

Name the rule, then convert the units. Say what the code *does* in one
sentence and implement that sentence. Croc's walk code (0x80043234) is
`v = v - (v >> 2) + target` with `target = 35 << 12`: "close a quarter of the
gap to four times the target each tick", which settles at 140 units/tick.

```rust
// `units` converts Croc units to the port's, once, at the boundary.
/// Top forward speed: the control code asks for 35 (0x800437F8) and closes a
/// quarter of the gap each tick, which settles at 4 × 35 = 140 units/tick.
pub const RUN_SPEED: f32 = units(140.0);
/// Fraction of the gap to the target velocity closed per tick (`v >> 2`).
pub const VELOCITY_BLEND: f32 = 0.25;
```

- A constant is the number it is, with a doc comment giving the raw value
  and where it was read.
- Integer behaviour that changes what the player sees (tick counts,
  thresholds hit on exact values, angles that wrap, truncation to zero) is
  a small named helper with a test, called by its meaning; never raw shifts
  and masks in a system.
- Per-tick units where the original is defined per tick; give per-second
  values for reference.

## Evidence

Every constant and every non-obvious rule says where it came from: the RAM
address and width, the instruction that writes or tests it, or the psx
recording that measured it. Say what was not verified. Someone must be able
to re-check each number with psx.

## Before calling a feature done

Read the feature's code as a stranger would:

- Does each system read as a rule someone would recognise?
- Are states enums with their data in the variants? Are params assets?
- Mirroring the original belongs inside a mechanic's step, where the order
  of operations is the behaviour. Above that, no one function per original
  routine, no resource per RAM address, no hex literal without a name and
  citation.

## Choices a goal can make

Suggested for a complete port, never required. Add one only when the goal
asks for it:

- an engine/game split: a reusable engine crate the game builds on;
- mod crates built on `game` (it is a library);
- the original's whole front end: menus, saves, options;
- an editor for the port's asset formats.

## Checks

```sh
cargo test --workspace --all-features    # the routine check
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo build --release --no-default-features   # shipping build
```

Iteration builds link Bevy as a shared library (default feature `dynamic`),
which only `cargo run` and `cargo test` find: start binaries through cargo,
not from `target/`.

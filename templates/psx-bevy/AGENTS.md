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
psx-bevy/      the PS1 look, ticks, input, menus, testing, numbers (num, gte) and launcher; yours to change (psx-bevy/AGENTS.md)
vendor/        psx-media and psx-assets (importers), psx-fixed (numbers), psx-gte (the GTE); don't edit (fix upstream in psx)
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

Exact mechanics are written once, generic over a `Mode` (`psx_bevy::num`).
`Num<M, W, F>` is a number the original stores in word `W` with `F`
fraction bits. The port runs the mechanic with `Exact`, where it is
`Fixed<W, F>` and computes as the original's compiled C does: `a * b` is
`(a * b) >> F` on the low 32 bits of the product, `a / b` truncates, and
results wrap at `W`. A new game can run the same code with `Float` (`f32`).

```rust
use psx_bevy::prelude::*;

/// Closes a quarter of the gap to four times `target` each tick, so the
/// speed settles at 4 × target (0x80043234: `v = v - (v >> 2) + target`).
fn approach<M: Mode>(v: Num<M, i32, 12>, target: Num<M, i32, 12>) -> Num<M, i32, 12> {
    v - v.div_pow2_floor(2) + target
}
```

- A constant is the number it is (`Num::real(1.375)`, which panics in
  `Exact` on a value the format can't hold), with a doc comment giving the
  raw value and where it was read.
- Integer behaviour that carries meaning is a named operation on `Num` or
  `Angle`: `div_pow2_floor` (the original's `>>`), `div_pow2_trunc`,
  `settle_to_zero`, `snap_to_unit`, `floor_int`, `rescale` (another format),
  `mul_wide` (the 64-bit product), `Angle::normalized`, `sin` and `cos`
  (`GteAngle`: libgte's table). Never raw shifts and masks in a system; a
  rule they don't cover is a small named helper with a test.
- What the original computed on the GTE calls `psx_bevy::gte` (psx's own
  GTE: operations, flags and rounding) with the same inputs.
- Faithful mechanics may use `f32` directly. Convert to `f32` for rendering
  once, at the render boundary (`to_f32`).
- Assets hold real numbers: a `Num` field reads `grip = 1.375` and, in
  `Exact`, refuses a value its format can't hold; an `Angle` reads degrees.
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

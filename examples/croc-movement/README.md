# Croc movement demo

Croc's player movement from *Croc: Legend of the Gobbos* (PAL, SLES-00593),
rebuilt as ordinary Bevy systems on the psx template. Every speed,
acceleration, turn rate and jump rule was measured on the original with the psx
toolkit; the method, RAM map and evidence are in
[`research/case-study-croc.md`](../../research/case-study-croc.md). The world
is placeholder shapes: no original game data is used or needed.

```sh
cargo run                               # arrows walk/turn, X jump, Q/E (L1/R1) sidestep
cargo test -- --nocapture               # compare with the original's trajectories
cargo run --features dev-mcp -- --mcp   # agent driving: game_step / game_screenshot
```

It is a workspace made by `psx template create`: `psx-bevy/` is the
template's crate (replaced by `psx template update`), `engine/` the reusable
engine with its sandbox, `game/` this study and `tools/` the recording
scripts. The study only uses the player, so the engine is still the
template's skeleton.

## Controls (as in the original's default scheme)

| Pad | Keyboard | On the ground | In the air |
|---|---|---|---|
| Up / Down | arrows | walk forward / back off | keep moving forward / back |
| Left / Right | arrows | turn | steer sideways |
| L1 / R1 | Q / E | sidestep (while walking) | turn |
| Cross | X | jump; hold for a higher jump | — |
| Triangle | S | toggle the PS1 look | |

## Where things are

- `game/src/croc/movement.rs`: the rules. Components (`Croc`, `Velocity`,
  `Facing`, `Gait`, `Vertical`, `Ground`) and five systems chained in the
  original's per-tick order: `fall_and_rise`, `walk`, `turn`,
  `snap_to_ground`, `start_jump`. Constants carry the RAM address or function
  that showed them.
- `game/src/croc/scene.rs`: meadow, posts, crystals, capsule Croc, chase camera
  (not the original camera), button HUD.
- `game/tests/trajectories.rs`: replays recorded input sequences through the
  movement rules headless (`engine::testing`, one 25 Hz tick per `run_tick`) and
  compares with the original, tick by tick.
- `game/tests/fixtures/*.csv`: Croc's position and heading per tick from the
  original (psx native core, level 1-1), with the buttons held.
- `tools/record_trajectory.py`: records a new fixture (needs the psx toolkit,
  a PAL BIOS, the disc and an in-level save state).
- `tools/reference_model.py`: the original's integer arithmetic as a literal
  port, kept as evidence; it reproduces every fixture exactly.

The simulation runs at 25 Hz, Croc's logic rate (one tick per two PAL
vblanks; `FIELDS_PER_TICK = 2` in `game/src/lib.rs`), so `game_step` frames are
game ticks.

## Verification

`cargo test -- --nocapture` prints, per scenario, the largest position and
heading difference from the original. The demo keeps fractional units where
the original truncates to whole units each tick, so it drifts by up to about
55 units (a running step is 140) and 5/4096 of a turn over 75 ticks; jumps
leave and reach the floor on exactly the same ticks. The integer reference
model has zero error on all fixtures.

What is not modelled: slopes and walls (the fixtures stay on the flat start
area), the mid-air "hang then dive" move from a second Cross press, the jump
buffered by pressing Cross just before landing, the original camera, and
analog-stick control.

# Croc's movement (a port workspace)

Croc's running, jump and stomp from Croc: Legend of the Gobbos (PAL,
SLES-00593), ported with psx's port loop. This is the workspace as the
porting agent left it when `psx port next` reported the goal done: every
recording of the original passes, including three the person played and
the agent never saw, and the code's review holds. The world is a stand-in:
a flat floor where 1-1 starts Croc, since the level's geometry is not
imported yet.

```sh
cargo run -- --skip-import                    # Croc on the stand-in floor: Cross jumps, Cross again in the air stomps (F1: controls)
cargo test -p game --test scenarios           # replay every recording of the original, tick by tick
cargo test --workspace                        # everything
```

## How it was made

`psx port init` with the disc wrote the workspace; from then on `psx port
next` gave one item at a time, in this order:

1. **The goal** (`goal.toml`): the person asked for Croc's running, jump
   and stomp. The agent catalogued them as features
   (`catalogue/F-0001.md`..`F-0003.md`) and the person confirmed the list.
   `F-0004` (the standing sidestep) is catalogued but outside the goal: it
   moves Croc by his animation's root motion, which needs the animation
   data imported first.
2. **Locate:** `psx code coverage diff` between runs that did the thing and
   runs that did not, from the same state, gave each feature's code; the
   entry lists it under `code`. Croc's rules turned out to live in a
   bytecode script run by the object loop; `notes/player-controller.md`
   maps it.
3. **Record:** the quantities compared (`glossary/`), bound to the
   original's RAM (`glossary/binding/`), recorded per tick from a state at
   the start of 1-1 (`scenarios/start_1_1.toml`, built from the recipes in
   `notes/states/`) with scripted inputs.
4. **Implement and fix:** the port's code in `game/`, checked by `psx port
   check` against each recording until all pass.
5. **Cover:** every conditional branch of each feature's code taken both
   ways by some recording, or listed under `wont_reach` with why and
   confirmed by the person (`goal.toml`: slopes, slippery and moving
   floors, analog mode, room entry).
6. **Review:** someone who did not write the code reviewed it as a whole,
   asking whether one would write a new game like this; `review.md` has
   the findings and how each was answered, over four rounds.
7. **Play:** the person played each feature freely; those recordings
   (`play_*`) were held out, judged only once the code was reviewed.

## Layout

| Path | What it is |
|---|---|
| `goal.toml` | The goal, the person's confirmation and the confirmed `wont_reach` branches |
| `catalogue/` | One entry per feature: what it is in game terms, its code in the original, its recordings |
| `glossary/` | The quantities compared (`croc_x`, `croc_vertical_speed`, ...) and where the original keeps them |
| `scenarios/` | The start state's scenario and its recordings: Croc's quantities and the pad per tick (`.jsonl`), and the branches each run took (`.coverage.json`) |
| `game/src/tick.rs` | The tick's phases, at the original's 25 Hz |
| `game/src/character/` | A character's body: position, facing, rising and falling, the floor |
| `game/src/croc/` | Croc, the player: running and turning, the jump, the stomp |
| `game/src/world.rs` | Levels as placements, and the floor |
| `game/assets/` | Tuning and the start level in RON, with each constant's source |
| `game/tests/scenarios.rs` | Replays each recording through the game's logic, headless |
| `review.md` | The code review and its answers |
| `notes/` | The agent's notes: the script map, coverage work, held-out plays, feedback on psx |

## What is not here

No disc data: the recordings hold the quantities the glossary names, the
coverage files hold the addresses of branches and which way they went, and
`game/assets/` holds constants cited from the code. The save states
(`.psx/`), the disc and the BIOS stay out of git. To carry on with psx
(`psx port status`, `psx port next`), put your own disc in `media/` and a
BIOS in `bios/`, and rebuild the states from `notes/states/` with `psx
states rebuild`.

The template this workspace started from is documented in
[`psx-bevy/README.md`](psx-bevy/README.md) and
[`docs/template.md`](docs/template.md).

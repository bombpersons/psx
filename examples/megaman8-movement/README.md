# Mega Man 8 movement (psx-bevy study)

Mega Man's walk and jump from Mega Man 8 (PAL, SLES-01064), rebuilt as
ordinary Bevy systems from measurements of the original. Made with
`psx template create`; the template's own documentation is in
[`templates/psx-bevy/psx-bevy/README.md`](../../templates/psx-bevy/psx-bevy/README.md).
The method, RAM map and evidence are in
[`research/case-study-megaman8.md`](../../research/case-study-megaman8.md).

```sh
cargo run                                # arrows walk, X (or C) jumps
cargo test -- --nocapture                # trajectories vs the original, max error
VERBOSE=1 cargo test matches -- --nocapture   # every tick, both side by side
cargo run --features dev-mcp -- --mcp    # developer MCP (game_step / game_screenshot)
```

No game data is included. Without it you get a placeholder Mega Man on a
placeholder floor. With your own capture (below), the demo shows the captured
intro-stage frame as a temporary backdrop and draws Mega Man's standing pose
from the decoded texture page.

## Layout

| Module | What it is |
|---|---|
| `src/lib.rs` | The game as a library (`game`): PAL region, 50 Hz logic, `GamePlugin`; `src/main.rs` only opens the window |
| `src/player/movement.rs` | The rules: states (stand, step, walk, jump, fall, land), constants with their RAM/code evidence, and the per-tick pipeline `control → integrate → after_move → collide_with_floor` in `GameSystems::Game` |
| `src/player/mod.rs` | The `Player` entity: `StagePos`, `Velocity`, `Facing`, `PlayerState`, `Grounded`, `WalkGrace` |
| `src/player/sprite.rs` | Placeholder or captured standing pose, mirrored by `Facing` |
| `src/stage.rs` | Stage space (pixels, y down), the `Solid` floor, the scrolling `StageCamera`, the optional captured backdrop |
| `tests/movement.rs` | Headless replay of those recordings through the rules (`game::testing`, one 50 Hz tick per `run_tick`), plus the camera clamp |
| `tests/data/*.csv` | Mega Man's position, velocity, state byte and camera per tick, recorded from the original with `psx run --watch` |
| `tools/mm8_trajectories.py` | Re-records those CSVs from your own disc |

Constants are the game's own per-tick values (16.16 fixed point, 50 Hz PAL
ticks), so the simulation matches the original exactly: the tests replay the
same inputs and require zero position error, the same state on every tick and
the same camera scroll.

## Optional: your own capture

From the psx checkout, with your own disc and BIOS (paths as in the case study):

```sh
psx run --backend native --bios bios/scph7502.bin "media/Mega Man 8.cue" \
    --frames 3900 --replay boot.json --save-state --out .psx/mm8-study/s3900
psx run --backend native --bios bios/scph7502.bin "media/Mega Man 8.cue" \
    --from-state .psx/mm8-study/s3900/state.bin --frames 2 --gpu-frame 1 --out .psx/mm8-study/cap
psx textures .psx/mm8-study/cap
cp -r .psx/mm8-study/cap/textures examples/megaman8-movement/assets/captures/mm8
```

`assets/captures/` is git-ignored. The backdrop is one captured screen pinned
in place: it is reference scaffolding for checking layout, not the level.

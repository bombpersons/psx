# Working in this game starter

The goal is a Rust/Bevy version of a game that reads like a modern, first-time
implementation of it: something a person could pick up later and change with
ordinary Bevy techniques. It must not be an emulator in disguise. The PS1 *look*
comes from the `psx` module; everything else is normal Bevy code.

## Ground rules

- Write idiomatic, modern Bevy 0.20 code: entities and components, one plugin per
  feature, assets through `AssetServer`, queries instead of global state. Do not
  build an emulator, GPU command lists, VRAM managers or a CPU-like main loop.
- 3D: `Mesh3d` + `MeshMaterial3d<PsxMaterial>`. Cameras need `PsxView`.
  Use normal Bevy lights. `DistanceFog` is the default fog; opt into per-vertex
  reciprocal cueing with `PsxMaterial::depth_cue` / `PsxDepthCue` when supported
  by the original's evidence. It overrides camera fog for that material.
- 2D: `Sprite` (with texture atlases) seen by a `Camera2d` + `PsxView`. Add
  `SpriteMaterial<PsxSprite>` for PS1 blend modes, per-texel STP or CLUT
  palettes (swap `palette_row` or edit the palette image); other polygons are
  `Mesh2d` + `PsxMaterial2d`. Alpha ≈0.5 means "STP bit" (README, 2D).
  Keep index images raw (`load_indices` / `psx_indices`), never plain `load`.
  `PsxView` cameras need orders below `SCREEN_PASS_ORDER` (999).
- Blending is in gamma-encoded colour, like the GPU's (README, "Blending in
  encoded colour"): every `PsxView` camera, 2D and 3D, draws into an encoded
  target. So under a `PsxView` use `PsxMaterial`, never `StandardMaterial`
  (it writes linear light and would look too dark).
- Keep gameplay in `FixedUpdate` / `GameSystems::Game`, reading
  `ControllerState`. That is the boundary the developer MCP and the tests step.
- Keep game assets in `assets/`. Do not commit original disc or BIOS data.
  Original `.tim` textures and `.tmd` models (from `psx assets extract`) load
  directly: `asset_server.load("x.tim")` is an `Image`, a `.tmd` is a
  `PsxModel` spawned with `PsxModelRoot` (README, "Original assets").
- Read README.md for MCP startup, screenshot targets and input lifetime.

## Project layout

The game is a library (crate `game`, `src/lib.rs`); `src/main.rs` only opens
the window and parses flags. So tests in `tests/` can use every game module.

```
src/lib.rs            REGION, FIELDS_PER_TICK, GamePlugin (lists your feature plugins), prelude
src/main.rs           window, renderer, --flags; adds GamePlugin
src/<feature>/mod.rs  one plugin per feature: player, stage, enemies, camera, hud...
src/<feature>/...     rules (components + systems, no rendering) apart from presentation
tests/<feature>.rs    headless tick tests against recorded trajectories
tools/                scripts that record evidence (trajectories, tables); not game code
src/psx, shell.rs, controller.rs, viewer.rs, testing.rs, dev_mcp.rs   the template
src/controller/bindings.rs, controls/   rebinding, explicit settings storage and the reusable example menu
```

- Start a new game by deleting `src/demo/`, removing it from `GamePlugin` and
  `lib.rs`, and adding your own feature modules. Migrate the demo tests in the
  same change: replace or remove `tests/player.rs`; adapt or remove
  `tests/dev_mcp.rs::headless_render_edit_step_and_query` and `Client::state`.
  That GPU/MCP test assumes a named demo player, its spawn position, movement
  constants and rendered scene. `#[ignore]` skips execution, not compilation.
  Check the replacement with `cargo test --features dev-mcp --all-targets --no-run`.
- `ControlsPlugin` is independent of the demo. Integrate it into your game's
  options flow (`global_shortcuts: false`, `ControlsMenu::open()`), or use the
  `ControllerBindings`/`BindingStore` API with your own UI. Keep gameplay on
  logical `ControllerState`; physical rebinding belongs at this boundary.
  Insert custom defaults before adding the controls plugin. Modal interfaces
  capture `InputFocus` before `PhysicalInputSystems::Sample`; clear latched input
  and wait for physical release before resuming. See README, "Rebinding".
- Explicitly import `crate::controller::Button` when using Bevy and game preludes
  together: Bevy UI also exports a `Button` type.
- Split each feature into a *rules* plugin (state, physics, decisions; no
  meshes, sprites or `AssetServer`) and a *presentation* part (spawning
  visuals, animation, camera). Rules then run headless in tests.
- Name important entities (`Name::new("Player")`) and derive `Reflect` with
  `#[reflect(Component)]` on gameplay components, so `game_query` can read them.
  In a paused developer build, `game_set_component` can edit selected named
  fields using a unique exact entity name or the id returned by `game_query`.
  Values use that query's JSON representation. Edits validate before applying.

## Tick rate and region

- `lib.rs` sets `REGION` (display: 50 Hz PAL / 60 Hz NTSC) and
  `FIELDS_PER_TICK`. Logic runs at `TickRate::every_nth_field(REGION, FIELDS_PER_TICK)`.
  Set both from evidence: the original's main loop and how many vblanks it waits
  per game frame. Croc (PAL) waits for two: 25 Hz. Mega Man 8 (PAL) runs every
  field: 50 Hz. Per-tick constants only mean the same thing at the same rate.
- Do not run logic every Nth tick by counting `GameClock.frame`: button edges
  (`pressed`) would fall on skipped ticks and be lost. Set the tick rate instead.
- Input is sampled every rendered frame and handed to the next tick, so a tap
  shorter than one tick still arrives as one tick of `pressed`.

## Testing gameplay

`game::testing` runs rules headless for exact ticks with scripted buttons
(`tests/player.rs` is the pattern to copy):

```rust
let mut app = headless_app(game::tick_rate(game::REGION));
app.add_plugins(player::RulesPlugin);
let player = app.world_mut().spawn(Player).id();
for (tick, row) in recorded.iter().enumerate() {
    app.run_tick(&row.buttons);
    let pos = app.world().get::<StagePos>(player).unwrap();
    assert!((pos.x - row.x).abs() <= TOLERANCE, "tick {tick}");
}
```

Record trajectories with the psx toolkit (`psx run --from-state ... --watch`),
store them as CSV under `tests/data/`, keep the recording script in `tools/`.
Compare every tick, and include at least one scenario that was recorded after
the rules were written and never used to fit them.

## From evidence to idiomatic code

The psx toolkit hands you evidence in the original's terms: RAM words in fixed
point, per-tick deltas, shifts and masks from MIPS code. The job is to turn
that into *rules* a reader recognises, not to transliterate the code.

### Name the rule, then convert the units

Write down what a piece of code *does* in one sentence, then implement that
sentence. Convert numbers at the boundary, once, with named constants and small
unit helpers, and cite where each came from.

Croc's walk code (0x80043234) is `v = v - (v >> 2) + target` with `v` in
×4096 fixed point and `target = 35 << 12`. The rule is "ease a quarter of the
way towards four times the target each tick", which settles at 140 units/tick:

```rust
/// Croc units per Bevy unit; display scale only, the rules do not depend on it.
pub const UNITS_PER_BEVY: f32 = 512.0;
pub const fn units(croc_units: f32) -> f32 { croc_units / UNITS_PER_BEVY }
/// An angle in the original's 4096-per-turn units, as radians.
pub const fn turns_4096(value: f32) -> f32 { value * TAU / 4096.0 }

/// Top forward speed: the control code asks for 35 (0x800437F8) and blends a
/// quarter of the gap each tick, which settles at 4 × 35 = 140 units/tick.
pub const RUN_SPEED: f32 = units(140.0);
/// Fraction of the gap to the target velocity closed per tick (`v >> 2`).
pub const VELOCITY_BLEND: f32 = 0.25;

velocity.0 += (target - velocity.0) * VELOCITY_BLEND;
```

Mega Man 8 stores positions and speeds as 16.16 pixels. `0x0005C900` is a
jump speed, `0x4000` gravity. Write them as the numbers they are, with the raw
value and its writer in the doc comment:

```rust
/// Initial upward jump speed: 0x0005C900 = 5.78515625 px/tick (289.3 px/s).
/// Written by 0x8010C9C4 on the first tick of the jump state.
pub const JUMP_SPEED: f32 = 5.785_156_3;
/// Gravity: 0x4000 = 0.25 px/tick² (625 px/s²).
pub const GRAVITY: f32 = 0.25;
```

Choose the units that fit the game:

- **Natural units with `Time`** (metres or pixels per *second*, scaled by
  `time.delta_secs()`, which is one tick in `FixedUpdate`): the default for new
  behaviour and for rules whose exact per-tick shape does not matter (the demo's
  `player.rs`). They survive a tick-rate change.
- **Per-tick units** (px/tick, units/tick) when the original's behaviour is
  defined per tick and you must match it: jump heights, landing ticks, frame
  windows of a 2D action game. A fixed-tick simulation with per-tick constants
  is legitimate; say so in the module doc and give per-second values for
  reference. Both studies did this.
- Keep the original's *coordinate conventions* only where they help matching
  (MM8 keeps stage pixels with y down in a `StagePos` component and maps to
  `Transform` in one sync system). Otherwise use Bevy's: y up, radians.

### When to keep exact integer behaviour

Most fixed-point truncation is noise; Croc's rules use `f32` and differ from
the original by at most 55 units over a 10 000-unit path, which the tests
bound. Keep exact behaviour only where it changes outcomes the player sees:

- The number of ticks something lasts (jump rise of 23 ticks, a 5-tick grace).
- Thresholds hit on exact values (MM8 lands only when the integer part of y
  passes the floor; Croc's turn cap is tested *before* multiplying, so one tick
  overshoots).
- Values that are exactly representable anyway: MM8's constants are multiples
  of 1/256, so `f32` reproduces them bit for bit with no integer code at all.

When you do need integer semantics, wrap them in a small documented helper with
a test, and call it by its meaning:

```rust
/// The floor snap restores only the integer part of y (halfword store at
/// 0x80106E40); the sub-pixel part is kept.
fn snap_to_row(y: f32, row: f32) -> f32 { row + y.fract() }
```

not `pos.y = ((pos.y as i32 & 0xFFFF_0000) | 441 << 16) as f32 / 65536.0` in the
middle of a system. If a literal model of the MIPS code is useful as evidence,
keep it outside the game (Croc: `tools/reference_model.py`) and test the game
against the recorded data, not against the model.

### Cite evidence in comments

Every constant and every non-obvious rule gets a comment saying where it came
from: the RAM address and width of the variable, the function or instruction
address that writes or tests it, or the measurement ("measured: 11 ticks after
landing; not read from the animation data"). Say what was *not* verified.
Someone must be able to re-check each number with the psx toolkit.

### State machines

The original usually keeps a state byte and dispatches a function table once
per tick. Model the states, not the dispatch:

- One `enum` component per independent state (`Vertical { Grounded, Rising {..},
  Falling {..} }`, `PlayerState { Stand, Step { ticks_left }, Walk, Jump, Fall,
  Land { ticks_left } }`), with the data each state needs inside its variant.
  Timers live in the state that uses them.
- A few systems, chained in the order the original makes its decisions within
  a tick (MM8: `control → integrate → after_move → collide_with_floor`; Croc:
  `fall_and_rise → walk → turn → snap_to_ground → start_jump`). Document why
  the order matters (Croc walks with last tick's heading).
- `match` on the enum inside each system; transitions are assignments of a new
  variant. No `state: u8`, no `fn table[state]()`, no giant per-frame function.
- Separate concerns into separate components and systems (facing, vertical
  motion, animation) when the original interleaves them only for speed.

### Captures are reference, not levels

`psx run --gpu-frame` + `psx textures` give a `.psxscene`: one frame of draw
calls in screen space. It is for *checking* your rebuild, never for shipping:

- Rebuild stages from decoded level data: tile sets as images, tile layers as
  grids (sprite chunks or a tilemap crate), collision as data, entity spawn
  lists as components. Find that data with the toolkit (trace the code that
  builds the draw packets back to its source arrays, then `psx loads` to the
  disc file) and decode it into plain files under `assets/`.
- Use the capture to verify: place your camera where the game's was, render,
  and compare against the capture (`--scene ... --painter` for the original's
  draw order). `PsxSceneElement::command` maps elements to GPU commands.
- A capture can stand in as a temporary backdrop while the real stage is not
  decoded yet. Mark it as such in code and in the README.

### Verify

- `cargo test --features dev-mcp` with per-tick trajectory comparisons (above).
  Keep that feature set for routine agent builds/tests/Clippy to reuse dependencies;
  switching it off can rebuild shared Bevy crates through feature unification.
- `cargo build` and `cargo build --features dev-mcp` when changing the shell.
- Visual changes: `game_step` + `game_screenshot`, and look at the image.
  Use `--mcp --headless --software` without a desktop; screenshot `target` must
  be `screen` in headless mode. `tests/dev_mcp.rs` is the opt-in GPU/MCP check.
- Numbers through MCP: `game_query` (README) reads positions and state.
- `cargo fmt` (config in `rustfmt.toml`) and `cargo clippy --features dev-mcp --all-targets`.
- Each copy of this template needs its own target directory. Two copies with
  the same package name (two `psx-game`s, a project and its backup) must not
  share a `CARGO_TARGET_DIR`: both build the same `game` library and overwrite
  each other's, so tests and binaries silently link the other copy's code.
  Keep the default `target/` inside the project, or rename the package.

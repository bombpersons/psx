# Working in the game

`game/` is this title on top of `engine/`: per-type controllers, action
handlers, HUD, menus, save format, level order, and the baked content in
`assets/`. Reusable machinery belongs in `engine/` (read `engine/AGENTS.md`
for the placement rules). The template's crate `psx-bevy/` is built on, and
edited only as `psx-bevy/AGENTS.md` allows.

The goal is a Rust/Bevy version of a game that reads like a modern, first-time
implementation of it: something a person could pick up later and change with
ordinary Bevy techniques. It must not be an emulator in disguise. The PS1 *look*
comes from `engine::psx`; everything else is normal Bevy code.

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
  `Mesh2d` + `PsxMaterial2d`. Alpha ≈0.5 means "STP bit" (`psx-bevy/README.md`, 2D).
  Keep index images raw (`load_indices` / `psx_indices`), never plain `load`.
  `PsxView` cameras need orders below `SCREEN_PASS_ORDER` (999).
- Blending is in gamma-encoded colour, like the GPU's (`psx-bevy/README.md`, "Blending in
  encoded colour"): every `PsxView` camera, 2D and 3D, draws into an encoded
  target. So under a `PsxView` use `PsxMaterial`, never `StandardMaterial`
  (it writes linear light and would look too dark).
- Keep gameplay in `FixedUpdate` / `GameSystems::Game`, reading
  `ControllerState`. That is the boundary the developer MCP and the tests step.
- Keep game assets in `game/assets/`. Do not commit original disc or BIOS data.
  Original `.tim` textures and `.tmd` models (from `psx assets extract`) load
  directly: `asset_server.load("x.tim")` is an `Image`, a `.tmd` is a
  `PsxModel` spawned with `PsxModelRoot` (`psx-bevy/README.md`, "Original assets").
- Read `psx-bevy/README.md` for MCP startup, screenshot targets and input lifetime.

## Project layout

The game is a library (crate `game`, `src/lib.rs`); `src/main.rs` only calls
the engine's launcher. So tests in `tests/` can use every game module.

```
src/lib.rs            REGION, FIELDS_PER_TICK, SCREEN, TITLE, GamePlugins (EnginePlugin + your feature plugins)
src/main.rs           engine::launcher::run(...): window, renderer, --flags
src/<feature>/mod.rs  one plugin per feature: player, stage, enemies, camera, hud...
src/<feature>/...     rules (components + systems, no rendering) apart from presentation
assets/               baked content in the engine's formats (levels, tuning, textures)
tests/<feature>.rs    headless tick tests against recorded trajectories
tests/dev_mcp.rs      opt-in GPU/MCP checks of the real binary
```

Evidence-recording scripts and importers live in the workspace's `tools/`.

- Start a new game by deleting `src/demo/` and `assets/demo/`, removing the
  demo from `GamePlugins` and `lib.rs`, and adding your own feature modules.
  Port `mods/example` to your game in the same change (see "Mods are crates").
  Migrate the demo tests in the same change: replace or remove
  `tests/player.rs`, and adapt or remove `headless_render_edit_step_and_query`
  in `tests/dev_mcp.rs` (it assumes the demo's named player, spawn position,
  tuning and scene). `#[ignore]` skips execution, not compilation. Check with
  `cargo test --workspace --all-features --all-targets --no-run`.
- Per-type controllers register with the engine: `app.register_object_type(id,
  ObjectTypeInfo::new("Name", spawn).with_tuning::<Tuning>())`, systems in an
  `EngineSet`, design numbers in `assets/<type>.ron` through `TuningPlugin`.
  Trigger and script actions: `app.register_action(id, "name", handler)`.
  Placements come from level data (`LevelData` + `SpawnLevel`), not code.
- `ControlsPlugin` is added by the launcher. Integrate it into your game's
  options flow (`ControlsMenu::open()`), or use the
  `ControllerBindings`/`BindingStore` API with your own UI. Keep gameplay on
  logical `ControllerState`; physical rebinding belongs at this boundary.
  Mouse controls read `ControllerState::mouse` (per tick, screen pixels);
  per-frame UI reads `pointer::Pointer`. Test mouse-driven rules with replay
  events carrying `"mouse"` (`Replay::ticks`) or `game_step`'s `mouse`.
  Modal interfaces capture `InputFocus` before `PhysicalInputSystems::Sample`;
  clear latched input and wait for physical release before resuming. See
  `psx-bevy/README.md`, "Rebinding".
- `DisplayPlugin` (F2: resolution, aspect, fullscreen, `PsxLook`) is added by the
  launcher too; open it from your options flow with `DisplayMenu::open()`.
  The screen can change size at runtime, so anchor 2D HUD elements with
  `PsxAnchor` rather than positions computed from `PsxScreen` at startup.
  See `psx-bevy/README.md`, "Screen size".
- Explicitly import `engine::controller::Button` when using Bevy and engine
  preludes together: Bevy UI also exports a `Button` type.
- Split each feature into a *rules* plugin (state, physics, decisions; no
  meshes, sprites or `AssetServer`) and a *presentation* part (spawning
  visuals, animation, camera). Rules then run headless in tests. The demo
  dresses rules-only entities in `Update` (`dress_player`).
- Name important entities (`Name::new("Player")`; the registry names spawned
  objects after their type) and derive `Reflect` with `#[reflect(Component)]`
  on gameplay components, so `game_query` can read them. In a paused
  developer build, `game_set_component` can edit selected named fields using a
  unique exact entity name or the id returned by `game_query`.

## Keep it extensible

The port should make new content easy: new levels, enemies, items and weapons,
a changed player controller. Structure it for that while porting: untangling
near-duplicates later, with every trajectory test still exact, costs far more
than porting shared code once.

- Port shared routines once. When the original runs one routine for many types
  (creature AI, mood, path search and movement; one gun table and firing path;
  pickups; cutscene playback), port it as one module with the per-type
  parameters and hooks the original has (e.g. `src/creature/`), and keep only
  each type's quirks in that type's controller.
- Adding a thing of an existing kind is a registration plus data: a new enemy
  registers a type, supplies its parameters and tuning file, and reuses the
  shared module. If it needs an edit to the shared code or a central `match`,
  the extension path is missing.
- Keep design numbers named and grouped in tuning files, so a variant is a new
  file, not a code change.
- Write down each controller's asset contract in `docs/` (joints, animation
  clip and state numbers, state-change frame ranges, animation commands,
  sound ids it relies on). A replacement player model or a new enemy model,
  with animations that meet it, can then reuse the ported controller, and the
  editor can check imported assets against it.
- Keep each feature plugin usable on its own. The player must run without the
  inventory, HUD or menus; where one feature needs another, make the
  dependency explicit (a shared component or event) or optional, never a
  resource that panics when the other plugin is missing.
- Don't replace ported code with a schema or script while porting. Data-defined
  variants come after parity, on top of the ported code, and must reproduce
  the ported trajectories exactly when given the original's parameters.

### Mods are crates

A mod is a crate that depends on `game` (the workspace's `game` dependency),
adds its own plugins and builds its own binary: a copy of `src/main.rs` whose
`add` adds `game::GamePlugins` (minus what it drops) and the mod's plugins.
It is recompiled, never loaded at runtime. `mods/example` is one; keep the
game convenient to build on:

- Keep feature modules, shared modules, components, tuning types and spawn
  hooks `pub`, so a mod can reuse the shared creature module, call a type's
  own spawn hook or query the player.
- `GamePlugins` is a `PluginGroup` of the feature plugins, so a mod can
  `GamePlugins.build().disable::<HudPlugin>()` and add its own in its place.
  List every feature plugin there, each usable on its own.
- `register_object_type` and `register_action` panic on duplicates to catch
  mistakes. A mod that changes an existing type or action calls
  `replace_object_type` / `replace_action` after adding `GamePlugins`.
- A mod's systems join the same `EngineSet`s, so they run in the original's
  update order.
- Assets are layered (`engine::assets`): the running crate's `assets/` first,
  then folders added with `engine::assets::add_root` before the app is built.
  A mod adds the game's `assets/` and can replace any game file by shipping
  one at the same path. Load files by relative path (`load_ron`,
  `AssetServer`) so the layers apply. A mod may use the original's assets
  (players supply their own disc) or ship only its own.
- Mod type and action ids must not collide with the game's; pick a range
  clear of the original's tables (the example uses 100 and up).
- Keep `mods/example` building, running and passing its tests
  (`cargo test -p example-mod`; its `tests/dev_mcp.rs` renders it). It is a
  small game made only of hand-made assets in `mods/example/assets/`, like
  the sandbox: the ported player controller drives a new character in a new
  level, it adds a type and replaces one, overrides the player's tuning file,
  and every other game feature is disabled. As the port grows, make it use
  the port's real pieces: a new character whose animations meet the player
  controller's contract, an enemy added through the shared creature module.
  It uses only `game`'s public API; if it needs an edit to `game/` or
  anything from the disc, a boundary is in the wrong place.

## Tick rate and region

- `lib.rs` sets `REGION` (display: 50 Hz PAL / 60 Hz NTSC) and
  `FIELDS_PER_TICK`. Logic runs at `TickRate::every_nth_field(REGION, FIELDS_PER_TICK)`.
  Set both from evidence: the original's main loop and how many vblanks it waits
  per game frame. Croc (PAL) waits for two: 25 Hz. Mega Man 8 (PAL) runs every
  field: 50 Hz. Per-tick constants only mean the same thing at the same rate.
- `SCREEN` is the original's display size: 320x240 for most games. Read it
  from the original's display mode and range (some show 368x240 or 512x240).
- Do not run logic every Nth tick by counting `GameClock.frame`: button edges
  (`pressed`) would fall on skipped ticks and be lost. Set the tick rate instead.
- Input is sampled every rendered frame and handed to the next tick, so a tap
  shorter than one tick still arrives as one tick of `pressed`.

## Testing gameplay

`engine::testing` runs rules headless for exact ticks with scripted buttons
(`tests/player.rs`), and compares them with the original
(`engine::testing::trajectory`):

1. Record the original from a checkpoint with a replay file and one `--sample`
   per value: `psx run game.cue --from-state S --frames N --replay
   tests/data/jump.input.json --sample 8015E8AC:fx16.16:y ... --out .psx/jump`.
   Copy its `samples.csv` to `tests/data/jump.samples.csv`.
2. In a test, spawn the recorded starting state (row 0), replay the same input
   and record the same names in the original's units:

```rust
let align = Alignment::new(game::FIELDS_PER_TICK, 0); // starting at row 0
let original = Trajectory::load_psx_samples("tests/data/jump.samples.csv", align)?;
let inputs = Replay::load("tests/data/jump.input.json")?.tick_inputs(align, original.ticks() - 1)?;
let mut app = headless_app(game::tick_rate(game::REGION));
app.add_plugins((EnginePlugin, player::RulesPlugin));
let player = app.world_mut().spawn(Player::at(original.get(0, "y").unwrap())).id();
let rebuild = Trajectory::record(&mut app, &inputs, &["y"], |world| vec![world.get::<StagePos>(player).unwrap().y as f64]);
compare(&original, &rebuild, &[("y", 0.0)])?.assert_passed();
```

A failure prints each column's first divergence, the values around it and,
when the rebuild matches shifted by a tick or two, a latency hint. With more
than one field per tick, input may only change on tick boundaries
(`tick_inputs` refuses anything else). Use tolerance 0 where the rule is exact.
Keep recording commands in the workspace's `tools/`, and include at least one scenario that was
recorded after the rules were written and never used to fit them.

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

- `cargo test --workspace --all-features` with per-tick trajectory
  comparisons (above). Keep that feature set for routine agent
  builds/tests/Clippy. Bevy resolves the same features with and without
  `dev-mcp` (see the alignment block in the root `Cargo.toml`), so switching
  only builds the MCP crates; keep it that way when adding dependencies.
- `cargo run -p engine --example sandbox` and `cargo run -p example-mod`
  still build and run.
- Visual changes: `game_step` + `game_screenshot`, and look at the image.
  Use `--mcp --headless --software` without a desktop; screenshot `target` must
  be `screen` in headless mode. `tests/dev_mcp.rs` is the opt-in GPU/MCP check
  (`cargo test --workspace --all-features -- --ignored`).
- Numbers through MCP: `game_query` (`psx-bevy/README.md`) reads positions and state.
- `cargo fmt --all` (config in `rustfmt.toml`) and
  `cargo clippy --workspace --all-targets --all-features`.
- Each project needs its own target directory. Two projects with the same game
  package name must not share a `CARGO_TARGET_DIR`: both build the same `game`
  library and overwrite each other's, so tests and binaries silently link the
  other copy's code. Keep the default `target/` at the workspace root, or
  rename the package.

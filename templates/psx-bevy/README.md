# PlayStation-style native Bevy game

A standalone Rust / **Bevy 0.20.0** starter for native reimplementations of
PlayStation games. It doesn't need an emulator, a disc or the psx toolkit at runtime.
Requires Rust 1.97.1+ and a desktop GPU driver (or Mesa lavapipe with `--software`).

```sh
cargo run
cargo run -- --software                 # force a software adapter
cargo run -- --region pal               # override lib.rs REGION (display + tick rate)
cargo run -- --controls                 # open the example controls menu (F1 also opens it)
cargo run -- --bindings ./controls.json # explicit settings location, useful for testing
cargo test                              # headless gameplay tests (tests/)
cargo run --features dev-mcp -- --mcp   # developer MCP over stdin/stdout
cargo run --features dev-mcp -- --mcp --headless --software # offscreen GPU, no desktop needed
cargo build --release                   # shipping build; developer MCP is excluded
```

`cargo build`/`cargo run` already compile Bevy at full optimisation (only your
own crate is built at `opt-level = 1`), so they run at close to release speed.
`--release` builds all of Bevy a second time, which takes minutes on a cold
target: use it for shipping and profiling, not for iteration.
Turning `dev-mcp` on for the first time compiles only the MCP crates (rmcp,
tokio, schemars) and your game, about 25 crates: `Cargo.toml` enables the
features those crates would otherwise switch on in shared dependencies such as
serde, so Bevy resolves identically in both builds. Afterwards Cargo keeps both
builds, and switching back and forth is free. If you add a dependency, check
that it doesn't reintroduce a difference (`cargo tree -e features` with and
without the feature). Keeping `--features dev-mcp` on throughout agent iteration
is still simplest; release builds are a separate profile and compile everything
once more.

## Project layout

The game is a library (crate `game`, `src/lib.rs`), so tests in `tests/` and
tools can reach every module; `src/main.rs` only opens the window.

| Path | Contents |
|---|---|
| `src/lib.rs` | `REGION`, `FIELDS_PER_TICK`, `GamePlugin` (your feature plugins), `prelude` |
| `src/demo/` | The example game: `player.rs` (rules, no rendering) and `mod.rs` (scene, HUD). Replace it and its registration in `lib.rs` / `GamePlugin` to start your own |
| `tests/player.rs` | Demo-specific headless tick tests; replace with your game's trajectory checks |
| `tests/dev_mcp.rs` | Opt-in GPU/MCP checks; the player edit/step test must migrate with the demo |
| `src/psx/` | The PS1 look (materials, screen pass, `.psxscene` loader) |
| `src/shell.rs` | `TickRate`, `GameClock`, `GameSystems`, controller sampling |
| `src/controller.rs`, `controller/bindings.rs` | Logical `Button`/`ControllerState`, validated device bindings and settings storage |
| `src/controls/` | Reusable controls menu: draft edits, capture, conflict resolution and PS1-style presentation |
| `src/testing.rs` | `headless_app`, `TickApp::run_tick(s)` |
| `src/viewer.rs` | The `--scene` capture viewer |
| `src/dev_mcp.rs` | Developer MCP (feature `dev-mcp`) |

Write one plugin per feature (player, stage, enemies, camera, HUD), and keep each
feature's rules (state, physics, decisions) apart from its presentation (meshes,
sprites, animation) so the rules run headless in tests. AGENTS.md has the
longer guide, including how to turn reverse-engineering evidence into code.

When removing `src/demo/`, replace or remove `tests/player.rs` and adapt or remove
`headless_render_edit_step_and_query` plus `Client::state` in `tests/dev_mcp.rs`.
The latter references demo movement constants and assumes its player name, spawn
and scene. Ignored tests still compile. Run
`cargo test --features dev-mcp --all-targets --no-run` after replacing the demo
to catch stale imports as well as game test errors.

## The look, without the limits

Game code is idiomatic Bevy: `Mesh3d`, `Transform`, `Camera3d`, `Sprite`,
glTF scenes, `DirectionalLight`, `GlobalAmbientLight`, `DistanceFog`. The
`psx` module only changes how the result looks:

| PS1 trait | How it is reproduced | Switch |
|---|---|---|
| Low resolution | Every `PsxView` camera renders to a 320×240 target (`PsxScreen`), shown letterboxed and nearest-sampled | `PsxScreen` size |
| Vertex wobble | Vertices snap to whole target pixels in the vertex shader | `PsxLook::vertex_snap` |
| Affine texture warp | UVs and colours interpolate linearly in screen space (sent as A·w, divided by w per fragment) | always on |
| Gouraud lighting | Lighting is per vertex, on gamma-encoded colour where white = neutral and textures can be brightened up to 2× | `PsxMaterial::lit` |
| Dither | `PsxMaterial`/`PsxMaterial2d` apply the GPU's 4×4 ordered dither before cutting to 5 bits per channel (sprites are not dithered, as on the PS1) | `PsxLook::dither` |
| 15-bit framebuffer | After all `PsxView` cameras draw, one full-screen pass reduces the screen to 5 bits per channel, plain `Sprite`s included | `PsxLook::quantize` |
| Depth cueing | Optional reciprocal camera-depth cue on vertex colours; ordinary camera `DistanceFog` remains available | `PsxMaterial::depth_cue` / `fog` |
| Semi-transparency | `PsxBlend::{Half, Add, Subtract, AddQuarter}` are the four GPU equations, for 3D, `Mesh2d` and sprites | per material |
| Per-texel STP | With `PsxStp::Texel`, only texels whose alpha is ≈0.5 (the STP bit) blend; the rest are opaque | per material |
| Colour-key transparency | Texels with alpha < 0.25 are discarded | `PsxMaterial::cutout` |
| CLUT palettes | 8-bit index textures looked up through a palette image; swap rows or edit entries | `PsxSprite`/`PsxMaterial2d::palette` |

**Blending in encoded colour.** The GPU blended the 15-bit values in VRAM,
which are gamma-encoded, not linear light: a `Subtract` shadow takes 5-bit
steps off the stored numbers. So every `PsxView` camera, 2D or 3D, draws into
a texture holding encoded colour (`Rgba8Unorm`, Bevy's
`CompositingSpace::Srgb`), and `PsxBlend` on `PsxMaterial`, `PsxMaterial2d`
and `PsxSprite` is the hardware blend on those values. With the 15-bit pass on,
results equal the GPU's 5-bit equations exactly (unit test
`encoded_blend_matches_gpu`). Nothing to configure: pick a blend mode.
Trade-off: Bevy's own 3D materials (`StandardMaterial`) write linear light
and look too dark under a `PsxView`; use `PsxMaterial` there. Stock 2D
(`Sprite`, `ColorMaterial`, text) encodes correctly. The final copy into the
sRGB screen image decodes once, losslessly.

There are no VRAM, texture-page, polygon-count or light-count budgets.
Texture size is your choice, and there are up to 8 directional lights.
A depth buffer replaces the PS1's ordering tables. Semi-transparent
surfaces still sort back to front.

### 3D

```rust
commands.spawn((Camera3d::default(), PsxView, DistanceFog { .. }, Transform::from_xyz(..)));
commands.spawn((
    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
    MeshMaterial3d(materials.add(PsxMaterial::textured(asset_server.load("crate.png")))),
    Transform::from_xyz(0.0, 0.5, 0.0),
));
```

`PsxMaterial` replaces `StandardMaterial`. For glTF scenes, swap materials when
the scene spawns, or build meshes from extracted original model data.

**Lighting:** 1000 lux of `DirectionalLight::illuminance` is full strength (1.0),
and so is a `GlobalAmbientLight::brightness` of 1000. Lighting is gathered into
the `PsxLighting` resource every frame.

**Reciprocal depth cue:** set `PsxMaterial::depth_cue` to blend the lit vertex
colour toward a chosen far colour before texture modulation. The cue uses
forward camera depth in scene units, with screen-space interpolation of the
resulting vertex colours. It works with perspective and orthographic cameras.

```rust
use game::psx::{PsxDepthCue, PsxMaterial};

let material = PsxMaterial {
    // No cue at depth 20, full cue at 100, using a reciprocal curve.
    depth_cue: PsxDepthCue::from_distances(20.0, 100.0, Color::BLACK),
    ..default()
};
```

For coefficients recovered from a game, use
`Some(PsxDepthCue { bias, scale, far_color })`. The factor is
`clamp(bias + scale / max(view_depth, 0.0001), 0, 1)`; `bias` is dimensionless
and `scale` is in scene units (usually negative). For example, King's Field's
`1.25 - 0.625 * setting / depth` uses `bias: 1.25` and
`scale: -0.625 * setting`, after converting the setting into scene units.
The renderer has no game-specific curve constants.

`far_color` is an sRGB **vertex modulation colour**: black darkens, white is
neutral on textures, and values up to 2× white can brighten them. A coloured
target tints the texture; it does not replace every distant texture sample with
one flat colour. Its alpha is unused. This is the GTE colour stage followed by
GPU texture modulation. It uses authored floats, without GTE register precision,
reciprocal approximation or original near-plane divide limits.

`depth_cue: None` is the default. Then `fog: true` uses camera `DistanceFog` as
before; `fog: false` disables that fog. A valid `Some(...)` applies the reciprocal
cue independently of `fog` and overrides `DistanceFog` for this material, so it
is not fogged twice and needs no camera fog component. Invalid/non-finite cue
coefficients or far-colour channels disable the cue; invalid distance ranges
return `None` from the constructor.

The authored `cargo run --example depth_cue` scene shows equal-size panels at
depths 20, 40 and 100 above a large slanted wall. Its developer MCP controls can
change the curve/colour, texture, camera projection and fog while paused:

```sh
cargo build --features dev-mcp --example depth_cue
target/debug/examples/depth_cue --mcp --headless --software
# Opt-in GPU/MCP verification; saves and checks the rendered PNGs:
cargo test --features dev-mcp --test dev_mcp reciprocal_depth_cue_rendering -- --ignored --nocapture
```

Edit the named `DepthCueControls` entity's `DepthCueControls` component using
`game_set_component` (e.g. `fields: {"far_color":[0.2,0.3,0.5]}`).

### 2D

To make a 2D game, spawn a `Camera2d` with `PsxView` and use ordinary
`Sprite`s. Texture atlases, `Transform` scaling and rotation all work.
Coordinates are screen pixels with the origin at the centre and y up.
To mix 2D and 3D, render the 3D camera first (`order: 0`, clearing) and the
2D camera after it (`order: 1`, `ClearColorConfig::None`) for a HUD. For a 2D
background behind 3D, reverse the order.

Load pixel art as sRGB PNGs. Nearest sampling is the default
(`ImagePlugin::default_nearest()`).

For PS1 semi-transparency on a sprite, add `SpriteMaterial<PsxSprite>`
(Bevy's sprite-material extension: atlases, flips and `rect` keep working).
Other 2D polygons are `Mesh2d` + `PsxMaterial2d` (vertex colours are Gouraud
shading and are dithered).

```rust
commands.spawn((
    Sprite::from_image(asset_server.load("glow.png")),
    SpriteMaterial(psx_sprites.add(PsxSprite::blend(PsxBlend::Add))),
));
```

**Alpha convention.** Alpha 0 is transparent (the PS1's colour 0000h),
≈0.5 (128) marks the STP bit, 1 is opaque. `PsxStp::All` (default) blends
every texel of a semi-transparent surface; `PsxStp::Texel` follows the GPU
and blends only STP texels. `Subtract` cannot both replace and subtract in
one draw: pair a `Texel` copy with a `PsxStp::OpaqueOnly` one.

**Palettes (CLUTs).** Keep pixels as 8-bit indices and colours in a palette
image, one palette per row. Swap palettes by changing `palette_row` (flashes,
team colours), or cycle by rotating entries in the palette `Image`'s data.
Index images must stay raw: build them with `psx_indices` or load grayscale
PNGs with `load_indices` (a plain `load` decodes them as sRGB colour).
`psx_palette` builds a palette from 15-bit PS1 colours (`rgb15`, bit 15 =
STP, 0000h transparent). `psx textures` writes captured pages this way too:
`page_*_index.png` plus `clut_X_Y_N.png`.

```rust
let palette = images.add(psx_palette(&[&[0, rgb15(31, 0, 0), rgb15(31, 31, 0)], &[0, 0x7FFF, 0x7FFF]]));
commands.spawn((
    Sprite::from_image(load_indices(&asset_server, "hero_index.png")),
    SpriteMaterial(psx_sprites.add(PsxSprite::indexed(palette, 0))), // row 1 = white flash
));
```

The demo's gem (top left) does all three: palette cycling, a Circle-held
flash palette, and an additive STP highlight. Next to it are a dithered
Gouraud `Mesh2d` and a smooth gradient `Sprite` cut to 15 bits by the screen
pass; Triangle turns the PS1 look off for comparison.


### Captured frames as reference

The psx toolkit can turn one frame of the original game into plain data for
this template:

```sh
psx run --backend native --bios BIOS GAME.cue --frames N+1 --gpu-frame N --out run/
psx textures run/            # texture pages as PNGs + run/textures/scene.psxscene
```

Copy `run/textures/` under `assets/` and view it with
`cargo run -- --scene captures/level1/scene.psxscene`. In game code, spawn
`PsxSceneRoot(asset_server.load(...))`: each drawn element becomes a child
`Sprite` (tiles, aligned sprites) or `Mesh2d` (other polygons) in draw order,
tagged with `PsxSceneElement { command }`, its index in the capture's
`gpu_frame.json`. A capture is a pixel-faithful *reference*: one screen of draw
calls with no collision, layers or entities. Rebuild the stage from decoded
level data and use the capture to check the result (see "From evidence to
idiomatic code" below); do not ship it as the level. Semi-transparent elements use the game's
blend equation (`PsxSprite`, `PsxMaterial2d`, `PsxMaterial`) and, in format 2
scenes, blend only their STP texels like the GPU; `Subtract` elements with
STP texels spawn twice (subtracting and opaque texels). The scene is not
clipped to the original draw area. The loader reads formats 1 and 2; format
1 scenes (older `psx textures`) blend every texel of semi-transparent
elements. The viewer's `--no-dither` / `--no-quantize` switch `PsxLook`
effects off, e.g. for games that disable the GPU's dither (captures record
it per polygon; scenes from older captures assume it is on).

3D games work too. The capture matches each polygon's screen vertices to the
GTE perspective transforms that produced them, recovering view-space 3D
positions. Those polygons spawn as `Mesh3d` + `PsxMaterial` (pre-lit vertex
colours, double-sided, the game's blend mode) under a child group that maps
PS1 view space into Bevy (`PsxScene::view_transform`). A `Camera3d` with
`PsxSceneCamera` gets the game's field of view; it starts where the game's
camera was and can be moved freely. In such scenes, 2D elements drawn before
the first 3D polygon (clears, backdrops) go on `SCENE_BACKGROUND_LAYER` for a
camera behind the 3D one, and later ones overlay it (HUD).

The PS1 drew in painter's order without a depth buffer, so a backdrop polygon
the game drew first but placed near the camera shows in front under depth
testing (its `PsxSceneElement::command`, a low draw index, identifies it).
The `PsxSceneDepth` component on the root picks the mode:

- `PsxSceneDepth::Tested` (default): Bevy's depth buffer. Use it to explore
  the geometry from new camera angles.
- `PsxSceneDepth::Painter`: each 3D element draws over the earlier ones,
  with no depth test, as the game drew them. From the game's camera this
  reproduces the captured frame. Viewer: `cargo run -- --scene ... --painter`,
  and P toggles the mode.

```rust
commands.spawn((PsxSceneRoot(asset_server.load("captures/level1/scene.psxscene")), PsxSceneDepth::Painter));
```

Painter mode sets each element's `PsxMaterial::draw_order` to its index.
You can use that field on your own meshes too: ordered surfaces skip the
depth buffer and draw in ascending order in Bevy's sorted transparent pass.
2D elements stay on their background or overlay camera in both modes.

### Original assets (TIM textures, TMD models)

`psx assets extract` (psx toolkit) writes the original `.tim` / `.tmd` bytes
it finds next to PNG/glTF conversions. Put them under `assets/` and load them
like any asset:

```rust
// Image, RGBA8 sRGB, nearest sampling; colour 0x0000 has alpha 0.
let tex: Handle<Image> = asset_server.load("enemy.tim");          // palette 0
let alt: Handle<Image> = asset_server.load("enemy.tim#Palette2");  // other CLUT rows
// A TMD: its texture pages are decoded from the TIMs you name, uploaded to a
// virtual VRAM where their headers place them (default: `car.tim` if present).
let car: Handle<PsxModel> = asset_server
    .load_builder()
    .with_settings(|s: &mut TmdSettings| s.textures = vec!["car.tim".into()])
    .load("car.tmd");
commands.spawn((PsxModelRoot(car), Transform::default()));
```

- `TimSettings { palette, transparency }`: `Cutout` (default; the GPU's
  rule for opaque polygons), `SemiStp` (STP texels at alpha 0.5) or `Opaque`.
- `TmdSettings { textures, units_per_metre, cull_back_faces }`: `textures`
  may also name a game archive; every TIM inside it is uploaded.
  `units_per_metre` defaults to 64 (as `.psxscene` captures); models are
  rotated so PS1 y-down becomes y-up. Everything is double-sided unless
  `cull_back_faces` (PS1 screen-clockwise front faces).
- `PsxModelRoot` spawns one child per TMD object (`PsxModelObjectIndex`,
  `Name` "ObjectN") and a `Mesh3d` + `PsxMaterial` per material under it:
  lit packets use scene lights through `PsxMaterial::lit`, pre-coloured ones
  do not, semi-transparent packets get the TSB's blend mode. Objects are
  where the file puts them; games usually position them (limbs, doors).
- Meshes are labeled sub-assets (`car.tmd#Object0/Part1`), so they can be
  used with your own materials.
- Viewer: `cargo run -- --model car.tmd [--texture car.tim]... [--cull]`
  frames the model and turns it (`game_step` turns it under the dev MCP).

## Simulation and input

- Gameplay runs in `FixedUpdate`, in `GameSystems::Game`, once per logic tick.
  The tick rate is the game's, not the display's: `lib.rs` sets `REGION`
  (60 Hz NTSC / 50 Hz PAL fields) and `FIELDS_PER_TICK`, giving
  `TickRate::every_nth_field(REGION, FIELDS_PER_TICK)`. Many PS1 games run their
  logic every second field (Croc PAL: 25 Hz); Mega Man 8 PAL runs every field
  (50 Hz). Changing the `TickRate` resource at run time retunes `Time<Fixed>`.
  `GameClock.frame` counts ticks. `psx template create --region pal
  --fields-per-tick 2` writes both constants when creating a project.
- `Res<Time>` in `FixedUpdate` is one tick long, so speeds can be written per
  second (`speed * time.delta_secs()`) or, when the original's per-tick values
  matter, per tick.
- Read input from `ControllerState::{held, pressed, released}(Button::..)`,
  `direction()` and `left_stick`/`right_stick`. Edges are per logic tick.
  Devices are sampled every rendered frame and a press is kept until the next
  tick, so a tap shorter than one tick still counts as held for one tick.
  Do not skip ticks by counting `GameClock.frame`: presses on skipped ticks
  would be lost. Set the tick rate instead.

### Tests

`game::testing::headless_app(tick_rate)` builds an app without window or GPU;
`app.run_tick(&[Button::Cross])` runs exactly one tick with those buttons held.
Add only rules plugins (nothing that loads assets or spawns meshes):

```rust
let mut app = headless_app(game::tick_rate(game::REGION));
app.add_plugins(PlayerPlugin);
let player = app.world_mut().spawn(Player).id();
app.run_ticks(30, &[Button::Right]);
let x = app.world().get::<Transform>(player).unwrap().translation.x;
```

To check rules against the original, record values with `psx run --sample`
and a `--replay` file, keep both under `tests/data/`, and compare tick by tick
with `game::testing::{Alignment, Replay, Trajectory, compare}`. A mismatch
reports the first divergence per column, the values around it and a hint when
the rebuild is a tick early or late. The [module docs](src/testing/trajectory.rs)
explain the field/tick alignment; `AGENTS.md` has the full recipe.

| DualShock | Keyboard | Gamepad |
|---|---|---|
| D-pad | Arrows | D-pad (left stick is analog `left_stick`) |
| Cross / Circle / Square / Triangle | X / C / Z / S | South / East / West / North |
| L1 / R1 | Q / E | Left / right bumper |
| L2 / R2 | 1 / 3 | Left / right trigger |
| Start / Select | Enter / Right Shift | Start / Select |

The demo moves the crate with the D-pad or stick, turns it with L1/R1 and
hops on Cross. Triangle toggles the PS1 effects so you can compare. Replace
`src/demo/`; it shows the wiring (a rules plugin plus presentation), not a
game architecture to copy wholesale.

### Rebinding and the example controls menu

In the demo, **F1** or **Start+Select** opens the controls menu. Arrow keys/D-pad
choose a row; left/right selects the keyboard or gamepad column. Enter/South
starts capture: release existing keys/buttons, then press the new physical input.
Inputs stay logical (`Button::Cross`, `Button::Left`, etc.) in gameplay code.
Start directly in the menu with `cargo run -- --controls`; use
`--bindings PATH` to choose a separate settings file.

- A binding already used by another action offers an explicit swap. Enter/South
  swaps; Esc/East keeps the previous bindings. There is one binding per action
  and device, and a physical button cannot silently trigger two actions.
- Delete/West clears the selected slot. Restore defaults changes the draft.
- **Save and close** writes the profile and applies it only after the write
  succeeds. **Discard and close**, Esc/East while browsing, F1, or Start+Select
  discard the draft. Esc during capture cancels only that binding attempt.
- East is an assignable gamepad button while listening; use Esc to cancel a
  capture, or the opening shortcut to discard the whole menu. F1 (when global
  shortcuts are enabled) and Esc remain reserved in this example UI.
- Gameplay ticks and input pause while the menu owns focus. Closing waits for
  physical controls to become neutral, so confirmation keys and newly captured
  buttons cannot become gameplay presses. Losing window focus discards the draft.
  Injected test/MCP buttons still address logical actions; the menu does not remap
  them. MCP input/step calls return an explicit error while focus is captured,
  and opening a menu interrupts a pending step instead of leaving it waiting.

Profiles use a versioned JSON document, validated for duplicate assignments,
enum values, slot counts and stick deadzone. Invalid profiles are preserved and
leave the configured defaults active. Save is an explicit atomic replacement;
an error keeps the draft open and leaves live bindings unchanged. Settings default
to `$XDG_CONFIG_HOME/<package-name>/controls.json`, otherwise
`~/.config/<package-name>/controls.json` (Windows: `%APPDATA%`). `--bindings PATH`
overrides this in the demo. No profile is created by opening or cancelling a menu.

The menu is a reusable example, independent of `demo/`. Keep it in a finished game
where it fits the options screen, or use the binding API with your own UI:

```rust
use game::controller::{Button, ConflictPolicy, ControllerBindings, PhysicalBinding};
use game::controls::{ControlsMenu, ControlsPlugin};

// Add beside ShellPlugin and PsxPlugin. Your own menu decides when it opens.
app.add_plugins(ControlsPlugin { global_shortcuts: false, ..default() });
app.world_mut().resource_mut::<ControlsMenu>().open();

// Core API, also usable without the example presentation:
let mut bindings = ControllerBindings::default();
bindings.assign(Button::Cross, PhysicalBinding::Key(KeyCode::Space), ConflictPolicy::Reject)?;
```

Configure custom defaults by inserting `ControllerBindings` **before** adding
`ControlsPlugin`; Restore defaults returns to that snapshot, independently of a
loaded profile. Read slots with `key`, `pad_button` or `binding`; use `assign`,
`clear`, and `set_stick_deadzone` to edit them. `BindingStore::load/save` handles
persistence explicitly. `ControlsPlugin { render_ui: false, ... }` runs its real
input/editor logic in headless tests or with a custom presentation. `InputFocus`
and `PhysicalInputSystems::Capture` expose the shell's focus/order boundary for
other modal interfaces. Stick axes retain their original meaning; this menu
rebinds digital buttons, including triggers, rather than analog axes.

The presentation uses ordinary Bevy UI and its embedded default font, with an
isolated `PsxView` camera before the screen pass, so it appears in low-resolution
screenshots and passes through the same framebuffer quantization. If importing
both Bevy and game preludes, explicitly import `game::controller::Button` to
disambiguate Bevy's UI button type.

## From evidence to idiomatic code

Reverse-engineering evidence arrives as fixed-point RAM words, per-tick deltas
and MIPS shifts. Turn it into rules a reader recognises; AGENTS.md has the full
guide with examples from the Croc and Mega Man 8 studies. In short:

- **Name the rule, convert units once.** Croc's `v = v - (v >> 2) + target` is
  "ease a quarter of the way to the target each tick":
  `velocity += (target - velocity) * VELOCITY_BLEND`, with `RUN_SPEED =
  units(140.0)` and a `units()` helper for the original's world units.
  MM8's 16.16 `0x0005C900` is `JUMP_SPEED: f32 = 5.785_156_3` px/tick.
- **Cite evidence** on every constant and rule: RAM address, writer or test
  instruction, or measurement, and what was not verified.
- **Exact integer behaviour only where it shows**: tick counts, thresholds,
  landing rules. Wrap it in a small documented helper with a test; keep any
  literal model of the original code in `tools/`, not in the game.
- **State machines are enums + systems**: one enum component with per-state
  data, a few systems chained in the original's decision order, `match` on the
  enum. No state bytes or per-frame dispatch tables.
- **Captures are reference, not levels**: decode tile sets, tile layers,
  collision and spawn lists into `assets/` and build entities from them; render
  from the game's camera and compare against the capture to verify.
- **Test per tick** against recorded trajectories (`tests/player.rs`).

## Agent driving (`dev-mcp`)

Build with `--features dev-mcp` and run with `--mcp`. This starts a stdio MCP server;
stdout is reserved for JSON-RPC and logs go to stderr. Simulation starts paused.

Add `--headless` to render without X11/Wayland or a window. The normal GPU
pipeline still draws the native-resolution screen target; `--software` selects
a fallback adapter such as Mesa llvmpipe. A working graphics driver is required.
Headless mode also works without MCP, running continuous simulation. It does not
change the game's configured logic tick rate. Status reports `headless: true`.

| Tool | Arguments | Behaviour |
|---|---|---|
| `game_status` | `{}` | Frame, pause state, tick rate, screen size/region, held buttons, renderer adapter |
| `game_step` | `{"frames":30,"buttons":["cross","right"]}` | Holds the buttons for exactly N ticks, then releases them and freezes |
| `game_set_input` | `{"buttons":["start"]}` | Replaces the injected held buttons; `[]` releases them |
| `game_pause` | `{"paused":false}` | Resumes continuous simulation or manual play |
| `game_screenshot` | `{"target":"screen"}` or `"window"` | A real GPU-rendered PNG of the 320×240 target or of the window |
| `game_query` | `{"name":"Player","components":["Transform","Hop"],"resources":["GameClock"]}` | Reflected components (as JSON) of entities whose `Name` contains `name` and/or that have every component in `with`, plus resources |
| `game_set_component` | `{"name":"Player","component":"Transform","fields":{"translation":[2,0.5,0]}}` | While paused, replace selected named fields of one component; return the frame and before/after values |

`game_query` reads numbers instead of pixels, e.g. to compare a position with a
recorded trajectory after `game_step`. It sees components and resources that
derive `Reflect` with `#[reflect(Component)]` / `#[reflect(Resource)]` (they
register themselves). Type names can be short (`Transform`) or full paths;
without `components` it returns every reflected component of each match
(`limit`, default 16 entities). `{"with":["Player"]}` matches by marker.

To place an actor or camera for a comparison, call `game_set_component` with
either `name` (an **exact**, unique entity name) or `entity` (the id string from
`game_query`, including its generation), plus `component` and `fields`. Component
type names can be short or full paths. Values use the JSON format returned by
`game_query`; for example, a `Vec3` is a three-number array. Each selected field
is replaced as a whole, so supply the complete vector, nested struct or enum value.

The editor validates all fields and their concrete types before changing the
component, preserves unselected/ignored fields and does not advance simulation
ticks. It requires a reflected struct with named fields, a mutable existing
component, and supported reflection deserialization. Missing/ambiguous targets,
unknown fields and unsupported values fail with a readable error. Pause first
with `game_pause` if the game is running. Resources, component insertion/removal
and whole-component replacement are outside this tool. `game_query` stays read-only.
The editor is compiled only with `dev-mcp`.

Buttons are `up/down/left/right/cross/circle/square/triangle/l1/r1/l2/r2/start/select`.
Step first, then screenshot, for reproducible observations. The first screenshot
waits until pipelines have compiled. In headless mode use `target: "screen"`;
`target: "window"` immediately returns a clear error because there is no window.

## Checks

```sh
cargo test --features dev-mcp
cargo test --features dev-mcp --test dev_mcp -- --ignored --nocapture # real offscreen GPU/MCP check
cargo clippy --features dev-mcp --all-targets
cargo fmt --check        # rustfmt.toml: 120 columns
```

When changing feature-gated startup code, also run `cargo test` with default
features. Before shipping, run `cargo build --release`. These can compile a
separate dependency feature set/profile, so keep them outside the routine agent
iteration cycle unless the change requires them.

Give every copy of the template its own target directory (the default
`target/` in the project). Two copies with the same package name must not share
a `CARGO_TARGET_DIR`: they build the same `game` library under the same name
and overwrite each other's, so tests and binaries can link the other copy's
game code. Rename the package (`psx template create --name`) or keep separate
targets.

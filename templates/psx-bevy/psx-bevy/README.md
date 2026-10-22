# psx-bevy: the template's crate

The psx template's own crate inside a generated project: the PS1 look
(`psx`), logic ticks and input (`shell`, `controller`), the controls and
display menus (`controls`, `display`), headless and trajectory testing
(`testing`), the `--scene` and
`--model` viewers, the developer MCP (feature `dev-mcp`) and the `launcher`
every game's `main.rs` calls. Bevy 0.20.0; Rust 1.97.1+; a desktop GPU driver
(or Mesa lavapipe with `--software`).

**Owned by the template.** `psx template update` replaces this directory's
untouched files with newer versions (it never touches `engine/` or `game/`),
so build on it where you can. Small, general edits are allowed when a hook or
option is missing; `AGENTS.md` here has the rules, and each edit is listed in
the project's `docs/template.md`. Game code reaches it through `engine::` (the engine re-exports
these modules). See the project's root README for the workspace and commands.

| Module | Contents |
|---|---|
| `psx/` | The PS1 look (materials, screen pass, `.psxscene` loader, TIM/TMD loaders, bitmap font) |
| `shell.rs` | `TickRate`, `GameClock`, `GameSystems`, controller sampling |
| `controller.rs`, `controller/bindings.rs` | Logical `Button`/`ControllerState`, validated device bindings and settings storage |
| `controls/` | Controls menu: draft edits, capture, conflict resolution, saving (`mod.rs`), and a built-in PS1-style view a game can replace (`view.rs`) |
| `display/` | Display menu: resolution, aspect, borderless fullscreen and `PsxLook` switches, saved to `display.json` (`mod.rs`), and a replaceable built-in view (`view.rs`) |
| `settings.rs` | Player settings files: config folder, size-limited reads, atomic writes |
| `pointer.rs` | The mouse on the PS1 screen: `Pointer`, window-to-screen mapping, menu `Hitboxes` |
| `launcher.rs` | Window, renderer, headless mode and the shared command-line flags |
| `testing.rs`, `testing/trajectory.rs` | `headless_app`, `TickApp::run_tick(s)`, replay and per-tick comparison with the original |
| `testing/mcp.rs` | Stdio developer-MCP client for GPU tests (`dev-mcp`) |
| `viewer.rs`, `model_viewer.rs` | `--scene` capture viewer, `--model` TMD viewer |
| `dev_mcp.rs` | Developer MCP (feature `dev-mcp`) |

`examples/depth_cue.rs`, `examples/depth_bias.rs`, `examples/screen_size.rs`
and `tests/dev_mcp.rs` are the crate's own GPU checks.

`cargo build`/`cargo run` already compile Bevy at full optimisation (only the
workspace's crates are built at `opt-level = 1`), so they run at close to
release speed. `--release` builds all of Bevy a second time, which takes
minutes on a cold target: use it for shipping and profiling, not for iteration.
Turning `dev-mcp` on for the first time compiles only the MCP crates (rmcp,
tokio, schemars) and the workspace, about 25 crates: the root `Cargo.toml`
enables the features those crates would otherwise switch on in shared
dependencies such as serde, so Bevy resolves identically in both builds.
Afterwards Cargo keeps both builds, and switching back and forth is free. If
you add a dependency, check that it doesn't reintroduce a difference
(`cargo tree -e features` with and without the feature).

## The look, without the limits

Game code is idiomatic Bevy: `Mesh3d`, `Transform`, `Camera3d`, `Sprite`,
glTF scenes, `DirectionalLight`, `GlobalAmbientLight`, `DistanceFog`. The
`psx` module only changes how the result looks:

| PS1 trait | How it is reproduced | Switch |
|---|---|---|
| Low resolution | Every `PsxView` camera renders to a low-resolution target (`PsxScreen`, 320×240 unless the game says otherwise), shown letterboxed and nearest-sampled | `launcher::Game::screen`; `PsxScreen::set_size` at runtime |
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
use engine::psx::{PsxDepthCue, PsxMaterial};

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

The authored `cargo run -p psx-bevy --example depth_cue` scene shows equal-size panels at
depths 20, 40 and 100 above a large slanted wall. Its developer MCP controls can
change the curve/colour, texture, camera projection and fog while paused:

```sh
cargo build -p psx-bevy --features dev-mcp --example depth_cue
target/debug/examples/depth_cue --mcp --headless --software
# Opt-in GPU/MCP verification; saves and checks the rendered PNGs:
cargo test -p psx-bevy --features dev-mcp --test dev_mcp reciprocal_depth_cue_rendering -- --ignored --nocapture
```

Edit the named `DepthCueControls` entity's `DepthCueControls` component using
`game_set_component` (e.g. `fields: {"far_color":[0.2,0.3,0.5]}`).

**Depth bias:** `PsxMaterial::depth_bias` pushes a depth-tested surface away
from the camera so it loses to coplanar ones: decals, or the unsplit copy a
game draws behind the pieces of a polygon it subdivided, to hide T-junction
cracks. `PsxDepthBias::BEHIND` is enough at any viewing angle; the fields are
GPU depth-bias units (`constant` steps of depth precision plus `slope_scale`
times the polygon's depth slope), positive meaning farther. It has no effect
with `draw_order`. `examples/depth_bias.rs` checks it on the GPU
(`depth_bias_loses_to_coplanar_copy_on_a_custom_screen` in `tests/dev_mcp.rs`).

**Per-frame geometry:** what the original's CPU and GTE did to polygons
before the GPU drew them (subdividing near polygons, LOD, clipping) belongs to
the game's meshes, not to this crate. `PsxMaterial` snaps and affine-maps
whatever vertices it gets, so a mesh rebuilt each frame from the camera
reproduces it, and mesh assets stay writable.

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
psx run --bios BIOS GAME.cue --frames N+1 --gpu-frame N --out run/
psx textures run/            # texture pages as PNGs + run/textures/scene.psxscene
```

Copy `run/textures/` under `game/assets/` and view it with
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
it finds next to PNG/glTF conversions. Put them under `game/assets/` and load them
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

### Layered asset folders (mods)

`assets/` is found the way Bevy finds it (`BEVY_ASSET_ROOT`, else the running
crate, else the executable's folder). A crate built on another one, such as
a mod built on `game`, adds that crate's folder below its own before the app
is built:

```rust
psx_bevy::assets::add_root("../../game/assets"); // relative to the base above
launcher::run(Game { /* ... */ });
```

The launcher then reads every load from the first folder that has the path,
so the mod finds the game's files and replaces any of them by shipping one at
the same path. Synchronous loads use `assets::find` (`engine::config::load_ron`
does). Without added roots, Bevy's own reader is used unchanged.

## Simulation and input

- Gameplay runs in `FixedUpdate`, in `GameSystems::Game`, once per logic tick.
  The tick rate is the game's, not the display's: the game's `lib.rs` sets `REGION`
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
- The mouse is sampled the same way, beside the pad (it is not mapped onto
  buttons): `ControllerState::mouse` has `held`/`pressed`/`released(MouseButton)`
  per tick, `position` on the PS1 screen in screen pixels (origin top-left,
  y down, through the letterbox; `None` off the screen), `world_2d(size)` for
  a default `Camera2d`'s coordinates, `motion` (raw device motion summed over
  the tick, for mouse look) and `wheel` (lines). Menus and other per-frame UI
  read `pointer::Pointer` instead, updated every rendered frame in
  `PhysicalInputSystems::Pointer`. Mouse buttons can also be bound to pad
  actions in the controls menu's MOUSE column (unbound by default).
- Tests and tools inject the mouse through `InjectedInput::mouse`
  (`InjectedMouse`): its buttons add to the real ones and press the actions
  they are bound to, `position` replaces the cursor, and `motion` and `wheel`
  are added on every tick. A replay event may carry
  `"mouse": {"buttons": ["left"], "position": [x, y], "motion": [dx, dy], "wheel": n}`
  (every field optional); read such replays with `Replay::ticks`, which gives
  `TickInput`s for `Trajectory::record` or `TickApp::run_input`
  (`tick_inputs` refuses them rather than drop the mouse). The original had
  no mouse, so `psx run --replay` rejects these files: they are for the
  rebuild's own tests.

### Tests

`engine::testing::headless_app(tick_rate)` builds an app without window or GPU;
`app.run_tick(&[Button::Cross])` runs exactly one tick with those buttons held.
Add only rules plugins (nothing that loads assets or spawns meshes):

```rust
let mut app = headless_app(game::tick_rate(game::REGION));
app.add_plugins((EnginePlugin, PlayerPlugin)); // EnginePlugin orders EngineSet
let player = app.world_mut().spawn(Player).id();
app.run_ticks(30, &[Button::Right]);
let x = app.world().get::<Transform>(player).unwrap().translation.x;
```

To check rules against the original, record values with `psx run --sample`
and a `--replay` file, keep both under `tests/data/`, and compare tick by tick
with `engine::testing::{Alignment, Replay, Trajectory, compare}`. A mismatch
reports the first divergence per column, the values around it and a hint when
the rebuild is a tick early or late. The [module docs](src/testing/trajectory.rs)
explain the field/tick alignment; `game/AGENTS.md` has the full recipe.

| DualShock | Keyboard | Gamepad |
|---|---|---|
| D-pad | Arrows | D-pad (left stick is analog `left_stick`) |
| Cross / Circle / Square / Triangle | X / C / Z / S | South / East / West / North |
| L1 / R1 | Q / E | Left / right bumper |
| L2 / R2 | 1 / 3 | Left / right trigger |
| Start / Select | Enter / Right Shift | Start / Select |

The demo moves the crate with the D-pad or stick, turns it with L1/R1 and
hops on Cross. Triangle toggles the PS1 effects so you can compare. Replace
`game/src/demo/`; it shows the wiring (level data through the engine's type
registry, a rules plugin plus presentation), not a game architecture to copy
wholesale.

### Rebinding and the example controls menu

In the demo, **F1** or **Start+Select** opens the controls menu. Arrow keys/D-pad
choose a row; left/right selects the keyboard or gamepad column. The mouse works
too: hovering selects, clicking a keyboard or gamepad cell starts capture for it
(clicking an action's name only selects it), right click is Esc and the wheel
moves the selection, so a layout can be set without working arrow keys. During
capture, a click on another cell moves the capture there and a click anywhere
else cancels it. Enter/South
starts capture: release existing keys/buttons, then press the new physical input.
After each binding (or confirmed swap) the selection moves to the next action, and
after the last one to **Save and close**, so a full layout can be entered with
Enter and the new keys alone. That helps when the arrow keys arrive as other
codes, as they can over remote desktop. Cancelling a capture stays on the row.
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
- Labels are drawn with `psx::font`, a 6×9 bitmap font (X11 misc-fixed, public
  domain), at one texel per canvas pixel. Bevy's vector text breaks up at this
  resolution; use `font::text_image` for other small in-canvas text too.
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
use engine::controller::{Button, ConflictPolicy, ControllerBindings, PhysicalBinding};
use engine::controls::{ControlsMenu, ControlsPlugin};

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
rebinds digital buttons, including triggers, rather than analog axes. A mouse
slot captures any mouse button, right click included, so only Esc cancels
that capture. Profiles saved before the MOUSE column existed load unchanged.

The presentation uses ordinary Bevy UI and its embedded default font, with an
isolated `PsxView` camera before the screen pass, so it appears in low-resolution
screenshots and passes through the same framebuffer quantization. If importing
both Bevy and game preludes, explicitly import `engine::controller::Button` to
disambiguate Bevy's UI button type.

### Making the menus the game's own

Both menus are split into logic and a view. The logic (`controls/mod.rs`,
`display/mod.rs`) does all input: navigation, mouse, capture, conflicts,
live preview, Save, Discard. The built-in views (`view.rs`) only draw it, in
a plain PS1 style, so the template's menus work from the first `cargo run`.
A finished port draws its own, in the game's style or inside its options
screen, without editing this crate:

1. Turn the built-in view (and, if the game opens the menus itself, the F1/F2
   shortcuts) off from the launcher's `add` closure:

   ```rust
   use engine::{controls::ControlsUi, display::DisplayUi};
   app.insert_resource(ControlsUi { view: false, shortcuts: false })
       .insert_resource(DisplayUi { view: false, shortcuts: false });
   ```

2. Copy `controls/view.rs` and `display/view.rs` into the game as a starting
   point and restyle them. They use only the public API: `ControlsMenu`
   (`is_open`, `selected`, `device`, `shown`, `mode`, `status`, `hint`),
   `controls::{row_label, ROWS, RESTORE, SAVE, DISCARD}`, `DisplayMenu`
   (`is_open`, `row`, `status`), `display::{Row, value, HINT}`.
3. For the mouse, fill `Hitboxes<controls::Item>` / `Hitboxes<display::Target>`
   each frame with the screen rectangles (PS1 screen pixels, as `Pointer`) of
   the rows and slots.
4. Open the menus from the game's options with `ControlsMenu::open()` and
   `DisplayMenu::open()`; `is_open()` turns false when the player saves or
   discards.

`a_game_can_replace_the_built_in_view_with_its_own` in each menu's tests is a
minimal example. Because the logic stays here, later `psx template update`
fixes still reach a port that restyled its menus.

### Screen size, fullscreen and the example display menu

`PsxScreen::set_size` changes the framebuffer at any time. Before that frame
renders, both screen images, the screen pass and every `PsxView` camera follow:
3D keeps its vertical field of view and shows more at the sides (Hor+), and 2D
stays one unit per pixel with the origin at the centre. Anything laid out from
the old size is the game's to move; `PsxAnchor { at, offset }` keeps a 2D entity
at a screen point (`at` is -1..=1 per axis, `(0, -1)` the bottom centre) plus a
pixel offset, which is what the demo's HUD uses. A resize replaces the screen
images with new ones (renderer caches are keyed by image, so resizing in place
could leave the window showing a stale frame), so game code that keeps a clone
of `PsxScreen::image` or `canvas` must read it again; cameras with `PsxView`
are retargeted automatically. `examples/screen_size.rs` and its GPU test check
all of this.

`--fullscreen` starts in borderless fullscreen and **F11** toggles it
(`display::set_fullscreen`).

The `PsxAspect` resource decides how the screen meets the window. `Fixed`
(the default) keeps the screen's size and letterboxes it. `Window` keeps its
height and sets its width to the window's aspect ratio (an even number of
pixels, at most 4096), so any window shape, or a fullscreen monitor, is
filled; resizing the window resizes the screen. Choose it with
`--aspect fixed|window`, the display menu, or by setting the resource.

In the demo, **F2** opens the display menu (`DisplayPlugin`, added by the
launcher beside `ControlsPlugin`): Resolution (the game's size, the same height
at 16:9, and 2×; `launcher::screen_choices`), Aspect (Fixed or Match window;
with Match window, Resolution chooses only the height), Fullscreen, and the
three `PsxLook` switches. Up/Down chooses a row; Left/Right or Enter/South changes it.
With the mouse, hover selects; click a setting's `<` or `>` to step it either
way (the rest of the row steps forwards); right click discards.
Unlike the controls menu, edits apply at once so they can be seen; **Save and
close** writes `display.json` beside `controls.json`, and **Discard and close**,
Esc/East or F2 put back what showed when the menu opened. **Restore defaults**
returns to what the game set up before the file was applied. Start directly in
the menu with `cargo run -- --display`; `--display-settings PATH` chooses the
file. `--aspect`, `--fullscreen`, `--no-dither` and `--no-quantize` win over the file for
that session (`DisplayOverrides`) and are only saved if the player saves. A
missing file keeps the game's defaults; an invalid one is reported, left
untouched and ignored. Pass your own `screens` to offer other sizes, or set
`global_shortcuts: false` and call `DisplayMenu::open()` from your options screen.

## From evidence to idiomatic code

Reverse-engineering evidence arrives as fixed-point RAM words, per-tick deltas
and MIPS shifts. Turn it into rules a reader recognises; `game/AGENTS.md` has the full
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
  collision and spawn lists into `game/assets/` and build entities from them; render
  from the game's camera and compare against the capture to verify.
- **Test per tick** against recorded trajectories (`game/tests/player.rs`).

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
| `game_status` | `{}` | Frame, pause state, tick rate, screen size/region, held buttons, the last tick's mouse, renderer adapter |
| `game_step` | `{"frames":30,"buttons":["cross","right"],"mouse":{"buttons":["left"],"position":[160,120]}}` | Holds the buttons (and optional mouse, as in replays) for exactly N ticks, then releases them and freezes |
| `game_set_input` | `{"buttons":["start"],"mouse":{"motion":[4,0]}}` | Replaces the injected held buttons and mouse; `[]` releases the buttons, omitting `mouse` releases it |
| `game_pause` | `{"paused":false}` | Resumes continuous simulation or manual play |
| `game_screenshot` | `{"target":"screen"}` or `"window"` | A real GPU-rendered PNG of the screen target (`Game::screen`, usually 320×240) or of the window |
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

Run from the workspace root (`--all-features` is `dev-mcp` everywhere):

```sh
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features && \
  cargo test --workspace --all-features -- --ignored --nocapture  # real offscreen GPU/MCP checks
cargo clippy --workspace --all-targets --all-features
cargo fmt --all --check  # rustfmt.toml: 120 columns
```

When changing feature-gated startup code, also run `cargo test --workspace`
with default features. Before shipping, run `cargo build --release`. These can
compile a separate dependency feature set/profile, so keep them outside the
routine agent iteration cycle unless the change requires them.

Give every project its own target directory (the default `target/` at the
workspace root). Two projects with the same game package name must not share
a `CARGO_TARGET_DIR`: they build the same `game` library under the same name
and overwrite each other's, so tests and binaries can link the other copy's
game code. Rename the package (`psx template create --name`) or keep separate
targets.

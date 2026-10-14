# PlayStation-style native Bevy game

A standalone Rust / **Bevy 0.20.0** starter for native reimplementations of
PlayStation games. It doesn't need an emulator, a disc or the psx toolkit at runtime.
Requires Rust 1.97.1+ and a desktop GPU driver (or Mesa lavapipe with `--software`).

```sh
cargo run
cargo run -- --software                 # force a software adapter
cargo build --release                   # developer MCP is excluded
cargo run --features dev-mcp -- --mcp   # developer MCP over stdin/stdout
```

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
| Depth cueing | Per-vertex distance with the camera's `DistanceFog` | `PsxMaterial::fog` |
| Semi-transparency | `PsxBlend::{Half, Add, Subtract, AddQuarter}` are the four GPU equations, for 3D, `Mesh2d` and sprites | per material |
| Per-texel STP | With `PsxStp::Texel`, only texels whose alpha is ≈0.5 (the STP bit) blend; the rest are opaque | per material |
| Colour-key transparency | Texels with alpha < 0.25 are discarded | `PsxMaterial::cutout` |
| CLUT palettes | 8-bit index textures looked up through a palette image; swap rows or edit entries | `PsxSprite`/`PsxMaterial2d::palette` |

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
shading and are dithered). Without a 3D camera on the screen, blending works
on gamma-encoded colour like the GPU's; with one, Bevy composites in linear
light, so a blend of very different colours (a dark `Subtract` shadow on
bright sand) comes out weaker than on the PS1.

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


### Captured frames as a starting point

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
`gpu_frame.json`. Use it as a pixel-faithful reference layout, then replace
pieces with real game entities. Semi-transparent elements use the game's
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

- Gameplay runs in `FixedUpdate`, in `GameSystems::Game`, at the region's
  field rate. That is 60 Hz for `Region::Ntsc` and 50 Hz for `Region::Pal`, set in `main.rs`.
  Many PS1 games simulate at half that rate; count ticks with `GameClock.frame`.
- Read input from `ControllerState::{held, pressed, released}(Button::..)`,
  `direction()` and `left_stick`/`right_stick`. Edges are per simulation tick.

| DualShock | Keyboard | Gamepad |
|---|---|---|
| D-pad | Arrows | D-pad (left stick is analog `left_stick`) |
| Cross / Circle / Square / Triangle | X / C / Z / S | South / East / West / North |
| L1 / R1 | Q / E | Left / right bumper |
| L2 / R2 | 1 / 3 | Left / right trigger |
| Start / Select | Enter / Right Shift | Start / Select |

The demo moves the crate with the D-pad or stick and turns it with L1/R1.
Triangle toggles the PS1 effects so you can compare. Replace `src/demo.rs`;
it is a wiring example, not an architecture.

## Agent driving (`dev-mcp`)

Build with `--features dev-mcp` and run with `--mcp`. This starts a stdio MCP server;
stdout is reserved for JSON-RPC and logs go to stderr. Simulation starts paused.

| Tool | Arguments | Behaviour |
|---|---|---|
| `game_status` | `{}` | Frame, pause state, screen size/region, held buttons, renderer adapter |
| `game_step` | `{"frames":30,"buttons":["cross","right"]}` | Holds the buttons for exactly N ticks, then releases them and freezes |
| `game_set_input` | `{"buttons":["start"]}` | Replaces the injected held buttons; `[]` releases them |
| `game_pause` | `{"paused":false}` | Resumes continuous simulation or manual play |
| `game_screenshot` | `{"target":"screen"}` or `"window"` | A real GPU-rendered PNG of the 320×240 target or of the window |

Buttons are `up/down/left/right/cross/circle/square/triangle/l1/r1/l2/r2/start/select`.
Step first, then screenshot, for reproducible observations. The first screenshot
waits until pipelines have compiled.

## Checks

```sh
cargo test
cargo clippy --features dev-mcp
cargo build --release
```

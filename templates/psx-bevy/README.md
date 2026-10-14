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
| Affine texture warp | UVs and colours interpolate in screen space (`@interpolate(linear)`) | always on |
| Gouraud lighting | Lighting is per vertex, on gamma-encoded colour where white = neutral and textures can be brightened up to 2× | `PsxMaterial::lit` |
| 15-bit colour + dither | The fragment shader applies the GPU's 4×4 ordered dither, then quantises to 5 bits per channel | `PsxLook::dither` |
| Depth cueing | Per-vertex distance with the camera's `DistanceFog` | `PsxMaterial::fog` |
| Semi-transparency | `PsxBlend::{Half, Add, Subtract, AddQuarter}` are the four GPU equations | per material |
| Colour-key transparency | Texels with alpha < 0.5 are discarded | `PsxMaterial::cutout` |

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
pieces with real game entities. Semi-transparency is approximated with 50%
alpha, and the scene is not clipped to the original draw area.

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

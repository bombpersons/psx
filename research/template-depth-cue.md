# Configurable per-vertex depth cue (2026-10-15)

The Bevy template now exposes `PsxMaterial::depth_cue: Option<PsxDepthCue>`.
It evaluates `clamp(bias + scale / view_depth, 0, 1)` at each vertex, blends the
lit vertex colour toward a configurable far colour, and sends the result through
the existing affine interpolation and texture modulation. This is a shared
rendering capability; the shader has no King's Field constants or data loading.

`view_depth` is positive forward camera-space Z, in scene units, clamped to
0.0001 before division. It is obtained from the view transform, so orthographic
cameras work too. Radial distance and perspective `clip.w` are not substituted
for view depth. The far colour uses sRGB vertex-modulation units; on textured
surfaces it tints/modulates the texture rather than replacing its pixels with a
uniform fog colour. Alpha is unchanged. Dither, transparency, lights and ordinary
texture modulation retain their existing stages.

`None` preserves the existing `fog` / camera `DistanceFog` path. A valid `Some`
overrides that path for the material and works independently of `fog`, including
on a camera without `DistanceFog`. Non-finite coefficients or non-finite/negative
far-colour RGB disable the cue and return to the ordinary path. An explicit
zero scale is valid (a constant cue); disabling uses `None`, so game-specific
zero-setting conventions belong in the game's conversion code.

`PsxDepthCue::from_distances(near, far, far_color)` provides a convenience curve
with zero cue at `near` and full cue at `far`. It returns `None` for invalid
ranges. Direct coefficients remain available when recovered from a game.
Usage and an authored scene are included in
[the template README](../templates/psx-bevy/README.md#3d).

## PS1 and King's Field evidence

The GTE's RTPS/RTPT projection commands produce a clamped interpolation factor
from `DQA * (H / SZ) + DQB`; its colour commands interpolate toward far colour
before the GPU modulates texture samples. See the primary hardware research in
[PSX-SPX's GTE documentation](https://psx-spx.consoledev.net/geometrytransformationenginegte/).
These mechanisms support a configurable feature, rather than a single game's
hardcoded fog-distance constant.

The King's Field PAL study's saved `GAME.EXE 0x80061008` pseudocode sets DQA to
`(-0x140 * argument1) / argument2` and DQB to `0x01400000`. Caller `0x8002FBC0`
passes `((setting & 0x7fff) >> 1, 200)`. With recorded projection H=200 this gives
the curve (before the caller's integer truncation)
`1.25 - 0.625 * setting / view_z`. FDAT[48]'s lighting presets include setting
16000; the game's source unit conversion in the existing study is 1024 per
scene unit. Thus this example is:

```rust
Some(PsxDepthCue {
    bias: 1.25,
    scale: -0.625 * 16000.0 / 1024.0,
    far_color: Color::BLACK,
})
```

The generic renderer does not reproduce GTE register truncation, reciprocal
approximation, saturation flags or near-plane division limits. Comparing the
authored float curve with the in-tree `psx-gte` RTPS implementation at ten
depths gave a maximum factor difference of 0.0005371094. The reference used
identity rotation, translation `[0,0,Z]`, zero input vertex, H=200, DQA=-12800
and DQB=0x01400000:

| Z (PS1 units) | GTE IR0 (divide by 4096) |
|---:|---:|
| 2000 | 0 |
| 4096 | 0 |
| 8192 | 120 |
| 12500 | 1841 |
| 16000 | 2560 |
| 20000 | 3073 |
| 20480 | 3120 |
| 30000 | 3754 |
| 40000 | 4095 |
| 60000 | 4096 |

These are software-GTE reference values, not newly measured hardware output.
They are retained in a template test, with a factor tolerance of 0.00055. A
second authored range, 20..100, gives bias=1.25 and scale=-25; this uses the same
functional form with an illustrative setting of 40 scene units. No whole-game
pixel match is claimed: lighting, camera poses, visibility and game-specific
material choices remain part of the game study.

## Authored rendering verification

`examples/depth_cue.rs` is a standalone authored comparison scene: three
equal-size panels at depths 20/40/100 above a large wall sloping from depth 20
on the left to 100 on the right. `DepthCueControls` is reflected and can be edited
through developer MCP while paused. The example is included by `psx template
create` and does not require game assets or an emulator.

The opt-in `reciprocal_depth_cue_rendering` test launches the example over real
stdio MCP with `--headless --software`, steps it, edits controls and captures
320×240 PNGs. It verifies:

- Known near/intermediate/far colours, including clamps.
- Uniform shading across equal-depth panels at different radial distances.
- Screen-linear interpolation of endpoint colours across the slanted wall;
  applying the reciprocal after interpolating depth would fail the samples.
- A camera fog component does not apply a second fog effect to an active cue.
- Disabling the cue restores ordinary camera fog, and `fog: false` disables it.
- A blue far colour blends red surfaces toward blue.
- A red far colour modulates a grey texture before sampling/output quantization.
- Perspective and orthographic versions of the authored pattern are pixel-identical.
- The actual setting-16000 example produces the expected rendered panel values.

Local validation uses Vulkan llvmpipe. Reciprocal, coloured and textured images
were inspected visually. Mesa EGL and inaccessible gamepad diagnostics are
environment messages; the Vulkan shader pipelines and captures succeed.
Screenshots/logs are saved under the `/tmp/psx-depth-cue-<pid>/` path printed by
the test. The default demo's separate headless MCP test checks the ordinary
lit/fogged path as well.

The example and GPU test now live in the psx-bevy repository
(<https://github.com/bombpersons/psx-bevy>); run the first five commands there.

```sh
cargo build --features dev-mcp --example depth_cue --offline
cargo test --features dev-mcp --offline
cargo test --features dev-mcp --test dev_mcp --offline -- --ignored --nocapture
cargo fmt --check
cargo clippy --features dev-mcp --all-targets --offline -- -D warnings
cargo test -p psx -p psx-assets --all-targets --offline
```

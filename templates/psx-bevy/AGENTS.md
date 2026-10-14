# Working in this game starter

- Write idiomatic, modern Bevy 0.20 code: entities and components, plugins per feature,
  assets through `AssetServer`, and queries instead of global state. Do not build an
  emulator, GPU command lists or VRAM managers. The `psx` module supplies the PS1
  look on top of normal Bevy rendering.
- 3D: `Mesh3d` + `MeshMaterial3d<PsxMaterial>`. Cameras need `PsxView`.
  Use normal Bevy lights and `DistanceFog`.
- 2D: `Sprite` (with texture atlases) seen by a `Camera2d` + `PsxView`. Add
  `SpriteMaterial<PsxSprite>` for PS1 blend modes, per-texel STP or CLUT
  palettes (swap `palette_row` or edit the palette image); other polygons are
  `Mesh2d` + `PsxMaterial2d`. Alpha ≈0.5 means "STP bit" (README, 2D).
  Keep index images raw (`load_indices` / `psx_indices`), never plain `load`.
  `PsxView` cameras need orders below `SCREEN_PASS_ORDER` (999).
- Keep gameplay in `FixedUpdate` / `GameSystems::Game`, reading
  `ControllerState`. That is the boundary the developer MCP steps.
- When reimplementing an original game, encode observed behaviour (from psx
  toolkit evidence: RAM watches, traces, extracted assets) in plain Rust types
  and systems. Note where a value or rule came from in a comment, such as the
  RAM address or the function that wrote it, so others can re-verify it.
- To match an original 2D frame, start from a capture: `psx run --gpu-frame` +
  `psx textures` give a `.psxscene` that `PsxSceneRoot` spawns as ordinary
  sprites and meshes tagged with their GPU command (README, "Captured frames").
  Rebuild with real entities from there; it is reference data, not an engine.
  To compare against the original frame pixel for pixel, use
  `PsxSceneDepth::Painter` (viewer: `--painter`). The default depth-tested
  mode can show early-drawn backdrops in front.
- Keep game assets in `assets/`. Do not commit original disc or BIOS data.
  Original `.tim` textures and `.tmd` models (from `psx assets extract`) load
  directly: `asset_server.load("x.tim")` is an `Image`, a `.tmd` is a
  `PsxModel` spawned with `PsxModelRoot` (README, "Original assets").
- Read README.md for MCP startup, screenshot targets and input lifetime.
- Verify both `cargo build` and `cargo build --features dev-mcp` when
  changing the shell; check visual changes with `game_step` + `game_screenshot`.

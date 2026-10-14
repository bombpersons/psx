# Working in this game starter

- Write idiomatic, modern Bevy 0.20 code: entities and components, plugins per feature,
  assets through `AssetServer`, and queries instead of global state. Do not build an
  emulator, GPU command lists or VRAM managers. The `psx` module supplies the PS1
  look on top of normal Bevy rendering.
- 3D: `Mesh3d` + `MeshMaterial3d<PsxMaterial>`. Cameras need `PsxView`.
  Use normal Bevy lights and `DistanceFog`.
- 2D: `Sprite` (with texture atlases) seen by a `Camera2d` + `PsxView`.
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
- Read the template's README (`templates/psx-bevy/README.md` in the psx
  repository) for MCP startup, screenshot targets and input lifetime.
- Verify both `cargo build` and `cargo build --features dev-mcp` when
  changing the shell; check visual changes with `game_step` + `game_screenshot`.

## This project

- Croc's movement rules live in `src/croc/movement.rs`; each constant cites
  its evidence (RAM address or function in the PAL `CROC.EXE`). Keep them in
  natural units (Croc units via `units()`, radians via `turns_4096()`, per
  25 Hz tick) rather than the original's fixed-point.
- `cargo test -- --nocapture` replays `tests/fixtures/*.csv` (recorded from the
  original) and must stay within its bounds after any rule change.

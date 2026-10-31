# Working in the game

`game/` is the port: its code, its assets in `assets/` and its tests in
`tests/`, built on `psx-bevy/` (the project's copy of the PS1 look,
ticks, input and menus; change it when the original works differently, see
`psx-bevy/AGENTS.md`). The root `AGENTS.md` has the rules for the code's
shape, numbers and evidence; this file has the Bevy specifics.

## Layout

The game is a library (crate `game`, `src/lib.rs`); `src/main.rs` only calls
psx-bevy's launcher, so tests and sandboxes can use every module.

```
src/lib.rs            REGION, FIELDS_PER_TICK, SCREEN, TITLE, GamePlugins (the game's plugins)
src/main.rs           psx_bevy::launcher::run(...): window, renderer, --flags; turns on the disc import
src/config.rs         load_ron and TuningPlugin: hand-editable RON under assets/
src/disc.rs           GameData state: import from the player's disc on first start
assets/               hand-made content (tuning, setup)
assets/imported/      imported from the player's disc on first start (git-ignored)
tests/                headless tick tests; dev_mcp.rs is the opt-in GPU check
```

Start the port by deleting `src/demo/` and `assets/demo/`, removing the demo
from `GamePlugins`, and replacing or removing the tests that use it
(`tests/player.rs`, the player checks in `tests/dev_mcp.rs`).
`#[ignore]` skips running, not compiling: check with
`cargo test --workspace --all-features --all-targets --no-run`.

## Bevy

- Idiomatic Bevy 0.20: entities, components, systems and plugins, assets
  through `AssetServer`, queries instead of global state.
- Keep logic (state, physics, decisions; no meshes, sprites or
  `AssetServer`) apart from presentation, which dresses what logic spawns
  in `Update` (the demo's `dress_player`). Logic then runs headless.
- 3D: `Mesh3d` + `MeshMaterial3d<PsxMaterial>`; cameras need `PsxView`. Use
  normal Bevy lights and `DistanceFog`; opt into per-vertex cueing with
  `PsxMaterial::depth_cue` when the original does it.
- 2D: `Sprite` under a `Camera2d` + `PsxView`; `SpriteMaterial<PsxSprite>`
  for PS1 blend modes, per-texel STP or CLUT palettes; other polygons are
  `Mesh2d` + `PsxMaterial2d`. Keep index images raw (`psx_indices`,
  `load_indices`), never plain `load`. `PsxView` cameras need orders below
  `SCREEN_PASS_ORDER` (999).
- Blending is in encoded colour, like the GPU's: under a `PsxView` use
  `PsxMaterial`, never `StandardMaterial`.
- Import `psx_bevy::controller::Button` explicitly: Bevy's prelude has a
  `Button` too.
- Name important entities (`Name::new("Player")`) and derive `Reflect` with
  `#[reflect(Component)]`, so the developer MCP's `game_query` can read them
  and `game_set_component` edit them.
- The screen can change size: anchor HUD elements with `PsxAnchor`.
- `psx-bevy/README.md` has the details: MCP startup, screenshot targets,
  input, rebinding, screen size.

## Data from the disc

The game imports the original's data itself on first start, from the
player's disc image next to it (`src/disc.rs`, running `import::import` from
`tools/import`). Players need neither psx nor an export step.

- Importers are the only code that reads the original's formats. They write
  the port's formats under `assets/imported/`, report progress, and record
  where each output came from (`Provenance::of_disc`, which `psx port
  check` counts as the file imported). A test that compares an import with
  what the game built from it marks the file with `import::checking`. Bump `import::VERSION` whenever their output
  changes.
- Set `import::SERIALS` to the disc revisions the importers support (the boot
  executable in `SYSTEM.CNF`) and `import::GAME` to the original's name.
- World setup that needs imported files runs on `OnEnter(GameData::Ready)`;
  `GameSystems::Game` already waits for it.
- Tests use a disc made with `import::test_disc::iso` (`tests/disc.rs`);
  tests that need the real disc are opt-in.
- Restyle the disc and progress screens in `src/disc.rs` to fit the game.

## Input and menus

- Gameplay reads the logical `ControllerState`; physical rebinding is
  `ControllerBindings`. Mouse rules read `ControllerState::mouse`.
- The launcher adds the controls (F1) and display (F2) menus. Open them from
  the game's options with `ControlsMenu::open()` and `DisplayMenu::open()`.
  To give them the game's look, turn the built-in views off (`ControlsUi`,
  `DisplayUi`), copy the views into `game/` and restyle them there, never
  inside psx-bevy.

## Tick rate and region

- `REGION` sets the display rate (PAL 50 Hz, NTSC 60 Hz); logic runs at
  `TickRate::every_nth_field(REGION, FIELDS_PER_TICK)`. Set both from the
  original's main loop: how many vblanks it waits per game frame (Croc PAL
  waits for two: 25 Hz).
- `SCREEN` is the original's display size, from its display mode (most show
  320x240).
- Never run logic every Nth tick by counting frames: button edges would fall
  on skipped ticks. Set the tick rate instead.

## Tests

`psx_bevy::testing` runs rules headless for exact ticks with scripted
buttons (`tests/player.rs`). `tests/scenarios.rs` replays each scenario
recording of the original tick by tick (`psx_bevy::scenario`); a failure
names each quantity's first tick out of tolerance.

Captures (`psx run --gpu-frame`, `.psxscene`) are for checking the rebuild,
never for shipping: levels come from the disc's data through the importers.

## Verify

- `cargo test --workspace --all-features`, `cargo fmt --all`,
  `cargo clippy --workspace --all-targets --all-features`. Keep `dev-mcp` on
  for routine builds; Bevy resolves the same features either way.
- Visual changes: `game_step` and `game_screenshot`, and look at the image
  (`cargo run --features dev-mcp -- --mcp --headless --software`; screenshot
  target `screen` when headless; an MCP client may run the built
  `target/debug/<game> --mcp --headless --software` directly). `tests/dev_mcp.rs` is the opt-in GPU check
  (`cargo test --workspace --all-features -- --ignored`).
- Each project needs its own target directory: two projects with the same
  game package name must not share a `CARGO_TARGET_DIR`.

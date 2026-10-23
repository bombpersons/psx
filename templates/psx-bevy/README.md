# PlayStation-style native Bevy game

A standalone Rust / **Bevy 0.20.0** workspace for a native port of a
PlayStation game, created by `psx template create`. It doesn't need an
emulator or the psx toolkit at runtime: on first start the game imports its
data from the player's own disc image placed next to it, and asks for one if
there is none (see "Game data from the disc"). Requires Rust 1.97.1+ and a
desktop GPU driver (or Mesa lavapipe with `--software`).

```sh
cargo run                                # the game (default member: game/); imports from the disc on first start
cargo run -- --disc PATH                 # import from this disc image (or folder)
cargo run -- --skip-import               # start without importing (the demo runs without disc data)
cargo run -p import -- DISC              # run the import by hand, into game/assets/imported
cargo run -- --software                  # force a software adapter
cargo run -- --region pal                # override REGION (display + tick rate)
cargo run -- --controls                  # open the controls menu (F1 also opens it)
cargo run -- --display                   # open the display menu (F2 also opens it)
cargo run -- --fullscreen                # borderless fullscreen (F11 toggles)
cargo run -- --aspect window             # screen width follows the window's shape
cargo run -p engine --example sandbox    # the engine's own tiny game
cargo run -p example-mod                 # a mod: a tiny game built on game/
cargo run -p editor                      # the editor for the engine's formats (Tools > Agents lets agents in)
cargo run -p editor -- --mcp --headless --software  # the editor's MCP on stdio, offscreen
cargo test --workspace                   # headless tests: engine, game, mod, importers
cargo run --features dev-mcp -- --mcp --headless --software  # developer MCP, offscreen
cargo test --workspace --all-features    # routine agent check (dev-mcp everywhere)
cargo build --release                    # shipping build; developer MCP is excluded
```

## Layout

| Path | Owner | Contents |
|---|---|---|
| `psx-bevy/` | this project, updated by the template | The base the engine builds on: the PS1 look, ticks and input, controls and display menus, testing, viewers, developer MCP, launcher. Change it to suit the game and list changes in `docs/template.md`; `psx template update` refreshes the files you haven't changed ([AGENTS.md](psx-bevy/AGENTS.md), [README](psx-bevy/README.md)) |
| `engine/` | this project | This game's engine family: object type and action registries, update-order sets, level and tuning formats, `examples/sandbox.rs`. No names, numbers or assets from this game ([AGENTS.md](engine/AGENTS.md)) |
| `game/` | this project | This title: per-type controllers, action handlers, HUD, menus, baked assets in `game/assets/`, trajectory tests ([AGENTS.md](game/AGENTS.md)) |
| `mods/example/` | this project | An example mod: a small game built on `game` from hand-made assets, proving the game can be extended without editing it ([game/AGENTS.md](game/AGENTS.md), "Mods are crates") |
| `editor/` | this project | The editor for the engine's formats: an egui desktop tool with File/Edit/View menus, outline, view, inspector and status bar, starting with level files, and an MCP server (stdio and HTTP) so agents can build levels with you and test the editor. Depends on `engine` only; shares the game's Bevy build (`engine` depends on the editor's UI and MCP crates for that) ([AGENTS.md](editor/AGENTS.md)) |
| `tools/import/` | this project | Importers: the player's disc → engine formats, with provenance. A library the game runs on first start (no Bevy), plus `cargo run -p import -- DISC` |
| `vendor/` | psx template | `psx-media` (CUE/BIN, ISO, CHD, ISO9660) and `psx-assets` (TIM, TMD, VAB/VAG): copies of the toolkit's crates for the importers. Replaced by `psx template update` ([README](vendor/README.md)) |
| `docs/` | this project | `engine-model.md` (what the original treats as data vs code), `evidence.md`, `template.md` (changes to `psx-bevy/`) |
| `.psx-template.toml` | psx template | Which template version this is and which files it owns |

The demo in `game/src/demo/` shows the intended shape: level content is data
(`game/assets/demo/level.ron`) spawned through the engine's type registry, each
object type is code registered by id, design numbers are in a tuning file, and
a trigger in the level runs a registered action. Replace it with your game.

## Game data from the disc

The repository never holds the original's data. `game/src/disc.rs` imports
it on start:

1. `game/assets/imported/import.json` from the current `import::VERSION`:
   start the game.
2. Otherwise look for a `.cue`, `.chd`, `.iso` or `.bin` next to the game
   (`--disc PATH`, the crate folder and the workspace root under `cargo run`,
   the executable's folder when shipped) whose boot executable is in
   `import::SERIALS`, and run `import::import` on it with a progress screen.
   The output is written beside the old import and swapped in when complete.
3. No disc: show "Put your disc next to the game" and look again every second.

Game code that needs imported files starts on `OnEnter(GameData::Ready)`.
Tests and mods use `GamePlugins` as is, which imports nothing and starts
ready. `assets/imported/` is git-ignored.

## Template updates

```sh
psx template update --dry-run   # what would change
psx template update             # replace unmodified psx-bevy/ and vendor/ files
```

Files under `psx-bevy/` or `vendor/` that you haven't changed are replaced
with this psx build's versions. Ones you changed are left alone and the new
version is written beside them as `<file>.psx-new`: merge it into your file
(`docs/template.md` lists your changes), then delete the `.psx-new`. Files
the template only seeded (everything else) are never touched; when their
template version changed, the update copies it under `.psx/template-update/`
for you to compare.

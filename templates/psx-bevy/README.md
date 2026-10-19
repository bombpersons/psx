# PlayStation-style native Bevy game

A standalone Rust / **Bevy 0.20.0** workspace for a native port of a
PlayStation game, created by `psx template create`. It doesn't need an
emulator, a disc or the psx toolkit at runtime. Requires Rust 1.97.1+ and a
desktop GPU driver (or Mesa lavapipe with `--software`).

```sh
cargo run                                # the game (default member: game/)
cargo run -- --software                  # force a software adapter
cargo run -- --region pal                # override REGION (display + tick rate)
cargo run -- --controls                  # open the controls menu (F1 also opens it)
cargo run -- --display                   # open the display menu (F2 also opens it)
cargo run -- --fullscreen                # borderless fullscreen (F11 toggles)
cargo run -- --aspect window             # screen width follows the window's shape
cargo run -p engine --example sandbox    # the engine's own tiny game
cargo test --workspace                   # headless tests: engine, game, importers
cargo run --features dev-mcp -- --mcp --headless --software  # developer MCP, offscreen
cargo test --workspace --all-features    # routine agent check (dev-mcp everywhere)
cargo build --release                    # shipping build; developer MCP is excluded
```

## Layout

| Path | Owner | Contents |
|---|---|---|
| `psx-bevy/` | psx template | PS1 look, ticks and input, controls and display menus, testing, viewers, developer MCP, launcher. Updated by `psx template update`; edit sparingly and list edits in `docs/template.md` ([AGENTS.md](psx-bevy/AGENTS.md), [README](psx-bevy/README.md)) |
| `engine/` | this project | This game's engine family: object type and action registries, update-order sets, level and tuning formats, `examples/sandbox.rs`. No names, numbers or assets from this game ([AGENTS.md](engine/AGENTS.md)) |
| `game/` | this project | This title: per-type controllers, action handlers, HUD, menus, baked assets in `game/assets/`, trajectory tests ([AGENTS.md](game/AGENTS.md)) |
| `tools/import/` | this project | Offline importers: files extracted with `psx` → engine formats, with provenance |
| `docs/` | this project | `engine-model.md` (what the original treats as data vs code), `evidence.md`, `template.md` (edits to `psx-bevy/`) |
| `.psx-template.toml` | psx template | Which template version this is and which files it owns |

The demo in `game/src/demo/` shows the intended shape: level content is data
(`game/assets/demo/level.ron`) spawned through the engine's type registry, each
object type is code registered by id, design numbers are in a tuning file, and
a trigger in the level runs a registered action. Replace it with your game.

## Template updates

```sh
psx template update --dry-run   # what would change
psx template update             # replace unmodified template files
```

Files under `psx-bevy/` that you edited are left alone and the new version is
written beside them as `<file>.psx-new`; reapply your change from
`docs/template.md` to it. Files the template only seeded
(everything outside `psx-bevy/`) are never touched; when their template
version changed, the update reports a diff to apply by hand.

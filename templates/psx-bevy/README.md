# PlayStation-style native Bevy game

A standalone Rust / **Bevy 0.20.0** workspace for a native port of a
PlayStation game, created by `psx port init`. It doesn't need an
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
cargo test --workspace                   # headless tests: game, psx-bevy, importers
cargo run --features dev-mcp -- --mcp --headless --software  # developer MCP, offscreen
cargo test --workspace --all-features    # routine agent check (dev-mcp everywhere)
cargo build --release --no-default-features  # shipping build: Bevy linked in, developer MCP excluded
```

## Layout

| Path | Owner | Contents |
|---|---|---|
| `psx-bevy/` | this project, updated by the template | The base the game builds on: the PS1 look, ticks and input, controls and display menus, testing, viewers, developer MCP, launcher. Change it to suit the game and list changes in `docs/template.md`; `psx template update` refreshes the files you haven't changed ([AGENTS.md](psx-bevy/AGENTS.md), [README](psx-bevy/README.md)) |
| `game/` | this project | The port, its assets in `game/assets/`, headless tests ([AGENTS.md](game/AGENTS.md)) |
| `tools/import/` | this project | Importers: the player's disc → the port's formats, with provenance. A library the game runs on first start (no Bevy), plus `cargo run -p import -- DISC` |
| `vendor/` | psx template | `psx-media` (CUE/BIN, ISO, CHD, ISO9660) and `psx-assets` (TIM, TMD, VAB/VAG): copies of the toolkit's crates for the importers. Replaced by `psx template update` ([README](vendor/README.md)) |
| `docs/` | this project | `template.md` (changes to `psx-bevy/`) |
| `.psx-template.toml` | psx template | Which template version this is and which files it owns |

The demo in `game/src/demo/` shows the pieces working together: feature
plugins (player, spinner) whose logic runs headless, the scene that dresses
them, and level and tuning in RON files (`game/assets/demo/`). Replace it
with your game; [AGENTS.md](AGENTS.md) has what the port's code is held to.

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

`game/src/disc.rs` is a working example of this, the game's to change or
leave out. Game code that needs imported files starts on
`OnEnter(GameData::Ready)`.
Tests and sandboxes use `GamePlugins` as is, which imports nothing and starts
ready. `assets/imported/` is git-ignored.

## Template updates

```sh
psx port update --dry-run   # what would change
psx port update             # replace unmodified psx-bevy/ and vendor/ files
```

Files under `psx-bevy/` or `vendor/` that you haven't changed are replaced
with this psx build's versions. Ones you changed are left alone and the new
version is written beside them as `<file>.psx-new`: merge it into your file
(`docs/template.md` lists your changes), then delete the `.psx-new`. Files
the template only seeded (everything else) are never touched; when their
template version changed, the update copies it under `.psx/template-update/`
for you to compare.

In clean mode, where the implementer may not run `psx`, the clean server's
MCP `port_update` does the same: it touches only the template's own code.

After an update:

- Resolution fails on `bevy_dylib` (no matching version): cargo's cached
  crates index predates the Bevy release the template now names. Delete the
  stale entry (`~/.cargo/registry/index/*/.cache/be/vy/bevy_dylib`) and build
  again.
- Doctests stop with `libstd-*.so` not found: with dynamic linking a test
  loads Rust's standard library as a shared library, and that toolchain's
  rustdoc did not put it on the loader path. Run them with
  `LD_LIBRARY_PATH="$(rustc --print target-libdir)" cargo test --workspace --all-features`
  (`DYLD_FALLBACK_LIBRARY_PATH` on macOS), or leave the doctests out with
  `--lib --bins --tests`.

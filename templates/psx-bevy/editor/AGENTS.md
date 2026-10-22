# The editor

A desktop editor for the engine's formats, the tool new content is made with.
It starts as a shell that edits the engine's `LevelData`: File, Edit and View
menus, an outline, a top-down view, an inspector and a status bar. Grow it
with the port until it can build a new level, and eventually a total
conversion, from nothing.

```sh
cargo run -p editor                                   # a new, empty level
cargo run -p editor -- game/assets/demo/level.ron     # open a file
cargo test -p editor                                  # document model, headless
```

## Rules

- **Engine only.** The editor depends on `engine/`, never on `game/` or a mod.
  It lists types from registry metadata (`ObjectTypeInfo` names, reflected
  tuning types), so it works for this game, the sandbox and any mod. Needing
  `game/` is a boundary bug in the engine: fix the engine.
- **A modern desktop tool.** Full window resolution, mouse and keyboard,
  ordinary egui panels, menus and fields. Never psx-bevy's PS1 renderer, low
  resolution screen, bitmap font, dithering or pad menus. 3D viewports, when
  they come, render with standard Bevy materials and cameras.
- **One format at a time, once it has settled.** When a format lands in the
  engine and its trajectory tests pass, the editor learns to create, open,
  show and save it in the same change.
- **Creatable, not only loadable.** File > New makes a valid file of every
  format. Fields not yet understood stay named, with a documented default.
- **Untouched saves are exact.** Opening and saving without an edit writes the
  file back byte for byte (`Document::to_text`). Edited files are rewritten as
  pretty RON, so comments in them are lost; say so in the format's docs if
  people hand-edit those files too.
- **Every edit is undoable.** Inspector and view edits are recorded by
  comparing the level before and after the frame (one step per gesture);
  commands go through `edit` in `lib.rs`. Keep it that way for new formats.

## Layout

| File | Contents |
|---|---|
| `src/main.rs` | The window and command line |
| `src/lib.rs` | `EditorPlugin`: menus, shortcuts, commands, confirm dialogs, status bar |
| `src/document.rs` | The open file, undo history and selection: plain data, tested without a window |
| `src/panels.rs` | Outline, top-down view, inspector |
| `src/files.rs` | In-app Open and Save As dialogs over the project's `.ron` files |
| `tests/document.rs` | Open, edit, undo and save round trips |

## Growing it

- **A new format:** a document kind beside `LevelData` (an enum in
  `Document`, or one `Document` per open tab), its outline and inspector, and a
  New entry. Add a round-trip test like `untouched_save_writes_the_file_back_byte_for_byte`
  using one of the project's own files.
- **Type names:** build an `App` with `EnginePlugin` plus the plugins that
  register types (or read an exported types file) to label ids in the outline
  and offer a type picker, instead of raw numbers.
- **3D view:** render the level with ordinary Bevy meshes and a standard
  camera into the central panel's rect (`Camera::viewport`), beside or in
  place of the top-down view.
- **Assets:** import glTF, PNG and WAV through `tools/import`, never through
  code that only the editor has.

## One Bevy build

The game and the editor share one compiled Bevy. Cargo resolves features
per build, so if the editor turned on Bevy features the game doesn't, every
switch between `cargo run` and `cargo run -p editor` would recompile most of
Bevy. `engine/` therefore depends on `bevy_egui` too, which makes both builds
resolve identical features. Keep that dependency even though the engine's
code doesn't use it.

When adding a dependency to the editor, check it keeps them identical:

```sh
cargo build && cargo build -p editor && cargo build   # the last two should compile only the editor and the game
```

If a dependency turns on features of a crate the game also builds (Bevy,
wgpu, winit, image, serde...), enable the same features in the workspace
`Cargo.toml` so both builds agree, or choose another crate.

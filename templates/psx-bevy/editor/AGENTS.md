# The editor

A desktop editor for the engine's formats, the tool new content is made with.
It starts as a shell that edits the engine's `LevelData`: File, Edit and View
menus, an outline, a top-down view, an inspector and a status bar. Grow it
with the port until it can build a new level, and eventually a total
conversion, from nothing.

```sh
cargo run -p editor                                   # a new, empty level
cargo run -p editor -- game/assets/demo/level.ron     # open a file
cargo test -p editor                                  # document model and MCP edits, headless
cargo test -p editor --all-features -- --ignored      # the real binary through its MCP (GPU)
```

Agents use it through its MCP server (see MCP below), so a person and an agent
can build a level together, and an agent can test the editor the way a
person uses it.

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
- **Agents can do what a person can.** Everything the editor can create or
  change has MCP tools in the same change: reading it in `mcp/mod.rs`,
  editing it in `mcp/edit.rs` (through `edit`, so each call is one undo step),
  and a check in `tests/mcp.rs`. New panels are named in `Screen` so tools can
  find and crop them.

## Layout

| File | Contents |
|---|---|
| `src/main.rs` | The window and command line |
| `src/lib.rs` | `EditorPlugin`: menus, shortcuts, commands, confirm dialogs, status bar |
| `src/document.rs` | The open file, undo history and selection: plain data, tested without a window |
| `src/panels.rs` | Outline, top-down view, inspector |
| `src/files.rs` | In-app Open and Save As dialogs over the project's `.ron` files |
| `src/mcp/mod.rs` | The MCP server: transports, the queue to the main thread, reading and viewing tools, screenshots, headless mode |
| `src/mcp/edit.rs` | Editing tools: open, save, undo, and each format's add, edit and delete (unit-tested here) |
| `src/mcp/input.rs` | Testing tools (`dev-mcp`): mouse and keyboard into egui, widgets from its accessibility tree, window size |
| `src/mcp/agents.rs` | Tools > Agents: the HTTP server switched on and off, and how to connect |
| `tests/document.rs` | Open, edit, undo and save round trips |
| `tests/mcp.rs` | The real binary, headless, through the MCP: agent edits, played-in input, screenshots (`--ignored`, GPU) |

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
- **MCP tools:** for a new format, `editor_level` learns a part for it, and
  `edit.rs` gets `<format>_add/edit/delete` tools (or what fits: a box of
  cells, a link between two things) in the style of the level's: indices as
  `editor_level` lists them, fields left out keep their values, refused with a
  reason rather than half applied. A new view gets `editor_locate` support,
  so tests can click what it draws. Keep the server's instructions (in
  `get_info`) describing the formats and units in a few sentences.

## MCP

The editor serves MCP tools in three sets, on two transports:

```sh
# An agent starts the editor itself and talks over stdio (stdout carries nothing else).
cargo run -p editor -- --mcp --headless --software --size 1280x800
# The testing tools too (developer builds):
cargo run -p editor --features dev-mcp -- --mcp --headless --software
# An agent joins an editor someone has open, over HTTP (or Tools > Agents).
cargo run -p editor -- --mcp-listen 127.0.0.1:7878 game/assets/demo/level.ron
```

`--headless` (stdio only) runs without an OS window: the window is only a size
and the editor draws into an image.

**HTTP.** Tools > Agents switches the server on, and shows the address, the
token and the `claude mcp add` command, each with a Copy button. Only
loopback addresses are served, requests from web pages (an `Origin` header)
or for other host names are refused, and every request needs the bearer
token. The port, token and on/off state are kept in the config folder
(`psx-editor/agents.ron`; rename `APP` in `mcp/agents.rs` after the game).
The menu bar shows *• Agents on*, or *• Agent at work* while one is asking.
`--mcp-listen` and `--mcp-token` apply for one run without saving.

| Set | Tools | Served |
|---|---|---|
| Reading and viewing (`mcp/mod.rs`) | `editor_status`, `editor_level`, `editor_locate`, `editor_select`, `editor_screenshot` | always |
| Editing (`mcp/edit.rs`) | `level_add/edit/delete_object`, `level_add/edit/delete_trigger`, `editor_undo`, `editor_open`, `editor_save` | always |
| Testing (`mcp/input.rs`) | `editor_input`, `editor_widgets`, `editor_window` | `--features dev-mcp`, stdio only |

- **Editing works beside a person.** Each call is one undo step and the status
  bar says what it did ("agent: ..."). Calls are refused, with the reason,
  while the person is part way through a drag, typing in a field, or has a
  dialog open; their mouse and keyboard keep working throughout. Undo is
  shared: `editor_undo` undoes whoever made the last step. `editor_open`
  refuses to drop unsaved changes unless told to discard them.
- **Testing goes through egui's own input.** `editor_input` plays mouse and
  keyboard a frame at a time (`click`, `drag`, `key: "Ctrl+Z"`, `text`...) and
  answers once the editor has seen it; `editor_widgets` finds what to click by
  label from egui's accessibility tree. While the testing tools are served,
  only injected input reaches the editor, so tests see the same editor every
  run. When a panel reads Bevy's input itself (a 3D camera), make `inject`
  write Bevy's input messages too.
- **Screen positions are window pixels**, origin top-left; level positions are
  metres. Relative paths are the editor's working directory (the project
  root under `cargo run`).

## One Bevy build

The game and the editor share one compiled Bevy. Cargo resolves features
per build, so if the editor turned on features of crates Bevy uses that the
game doesn't (or the other way round), every switch between `cargo run` and
`cargo run -p editor` would recompile most of Bevy. So the two depend on each
other's extras, unused:

- `engine/` depends on `bevy_egui` and the MCP server's crates (rmcp, tokio,
  tokio-util, axum). The game compiles them once and never links them.
- `editor/` depends on `arrayvec` and `flate2` with the features the game's
  disc reader (psx-media's CHD support) turns on.

Keep those dependencies even though the code beside them doesn't use them.

When adding a dependency to the editor or the game, check it keeps them
identical. Cargo keeps every variant it has built, so "nothing recompiled"
proves little in a used target folder. This lists the crates both builds use
but build differently, and prints nothing when they agree:

```sh
for build in "" "-p editor"; do
  cargo build $build --message-format=json | grep -o 'lib[^/"]*\.rlib' | sort -u; echo end
done | awk 'BEGIN { n = 0 } /^end$/ { n++; next }
  { crate = $0; sub(/-[0-9a-f]+\.rlib$/, "", crate); has[n, crate] = 1; built[n, $0] = 1; if (n) names[crate] = 1 }
  END { for (c in names) if (has[0, c]) { shared = 0
    for (k in built) { split(k, p, SUBSEP); f = p[2]; sub(/-[0-9a-f]+\.rlib$/, "", f); if (p[1] == 1 && f == c && built[0, p[2]]) shared = 1 }
    if (!shared) print c } }'
```

`cargo tree -e features -i CRATE -p psx-game` (and `-p editor`) shows who
turns a feature on when they differ.

If a dependency turns on features of a crate the game also builds (Bevy,
wgpu, winit, image, serde...), enable the same features in the workspace
`Cargo.toml` so both builds agree, or choose another crate.

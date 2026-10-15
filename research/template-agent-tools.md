# Headless rendering and component editing (2026-10-15)

The shared `templates/psx-bevy` starter can run its real renderer without a
desktop. `psx template create` embeds the updated sources, documentation and
opt-in GPU/MCP integration test into generated projects.

```sh
cd templates/psx-bevy
cargo run --offline --features dev-mcp -- --mcp --headless --software
```

`--headless` disables Winit, creates no primary window, prevents exit on an empty
window list and runs the schedule at the selected video field rate. Logic still
uses the game's `TickRate`. The game's cameras and PSX screen pass render to their
ordinary offscreen images. The window-only display camera/quad are omitted.

The developer MCP still starts paused. `game_status` reports headless mode and
the actual adapter. `game_screenshot` returns the rendered 320x240 screen PNG;
asking for a window screenshot immediately reports that no primary window exists.
The first screen capture waits for views and compiled pipelines as before.

## Editing a comparison pose

One tool call can place a named actor while paused:

```json
{"name":"Player","component":"Transform","fields":{"translation":[2,0.5,0]}}
```

Call this as `game_set_component`. Alternatively pass `entity`, the generation
aware id string from `game_query`, instead of an exact, unique `name`. The result
includes the unchanged tick number, target identity and selected fields before
and after the edit. A following screenshot renders the new state normally.

The editor checks pause/active-step state, target uniqueness, component mutability,
field existence and typed reflection deserialization before taking mutable world
access. Only selected fields change; unselected and ignored data are retained.
Named struct components are supported, including vector and enum fields whose
types support reflection deserialization. Each field value is a full replacement
using the JSON representation returned by `game_query`. Resources and structural
ECS edits are excluded. All editing code is gated by `dev-mcp`; `game_query`
remains read-only.

## Validation

Run normal rules/reflection tests without a GPU, then opt into real rendering:

```sh
cargo build --offline
cargo test --offline
cargo test --offline --features dev-mcp
cargo test --offline --features dev-mcp --test dev_mcp -- --ignored --nocapture
cargo clippy --offline --features dev-mcp --all-targets -- -D warnings
cargo fmt --check
```

The GPU test launches the actual game binary over stdio MCP with DISPLAY and
WAYLAND_DISPLAY unset. It checks a nonempty PNG, paused position edits and queried
values, a changed rendered scene, twelve exact movement ticks, rejection of an
invalid mixed patch, rejection of edits while running and the headless window
screenshot error. It saves before/edit/step PNGs, status and logs under a printed
`/tmp/psx-template-headless-<pid>/` directory. The binary lookup follows the
generated package name rather than assuming `psx-game`; the movement expectation
uses the configured region/tick rate.

Local GPU validation used Vulkan with Mesa llvmpipe (LLVM 19.1.7, 256 bits), a CPU
adapter. The before/after PNGs were also inspected visually. Mesa's unavailable
EGL backend and inaccessible physical gamepad devices produced stderr diagnostics;
the Vulkan renderer, injected input and MCP operations all completed successfully.

Unit tests separately cover wrong vector lengths, invalid integer ranges, missing
fields/components/types, immutable components, duplicate names, stale entity ids,
an active step, preservation of ignored data and reading edited state through
`game_query`. The template has 26 passing default tests and 31 with `dev-mcp`,
plus the separately run GPU test. Toolkit template-embedding and CLI/MCP tests
also pass.

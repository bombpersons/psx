template_create creates a standalone Rust/Bevy workspace: psx-bevy/ (the
PS1 look, ticks, input, menus and launcher; the project's copy, changed to
suit the game and listed in docs/template.md; template_update refreshes its
untouched files), engine/ (object
type and action registries, update-order sets, level and tuning formats, a
sandbox example), game/ (a demo to replace), mods/example/ (a mod built on
game/) and tools/import/. It can be
created in the game folder itself. Explicitly pass the original's display
region (ntsc/pal); choose fields_per_tick from measured game logic cadence, not
the display region alone. Read the generated AGENTS.md files (root, engine/,
game/) and READMEs for the architecture and command-line flags. For a whole
port, read guide topic port first.

Build ordinary Bevy entities, components and feature plugins using decoded game
assets. Keep gameplay rules in FixedUpdate (an EngineSet inside
GameSystems::Game) and consume logical ControllerState actions. Register each
object type's controller with the engine; put placements and tuning in assets.
Routines the original shares between types become shared systems and
components, with each type's registration supplying its parameters. The shared
controls menu can handle physical rebinding;
the finished game decides where that menu belongs in its options flow.

The goal is a fresh game implementation whose rules are backed by evidence.
Convert original units at the boundary and cite measured constants and writers.
Keep integer semantics where they affect outcomes or feel. Do not recreate an
emulator or translate CPU instructions into a disguised game loop.

Use captured .psxscene files with the template's --scene viewer as visual
reference. Reconstruct levels from decoded geometry/layout/collision and entity
data. A captured frame does not supply hidden geometry, world-space depth or game
rules. A temporary captured backdrop should be labeled as such.

The generated developer MCP supports exact game_step calls, game_query and
game_screenshot. Headless GPU runs use --mcp --headless --software and screenshot
target screen. Compare rebuilt behavior against recorded per-tick trajectories,
including cases not used to fit the rules, and inspect rendered output: run from
a checkpoint with a replay file and sample (one NAME per value), keep
samples.csv and the replay under game/tests/data/, then use
engine::testing::{Alignment, Replay, Trajectory, compare} in a test. Sample row N
is the state after N fields; tick k uses the input held at field
start + k*fields_per_tick. The generated game/AGENTS.md has the full recipe. Keep each
generated project's target directory separate to avoid linking another copy's game.

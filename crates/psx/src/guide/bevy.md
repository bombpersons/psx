template_create creates a standalone Rust/Bevy game starter. Explicitly pass the
original's display region (ntsc/pal); choose fields_per_tick from measured game
logic cadence, not the display region alone. Read the generated AGENTS.md and
README for the actual installed template's architecture and command-line flags.

Build ordinary Bevy entities, components and feature plugins using decoded game
assets. Keep gameplay rules in FixedUpdate / GameSystems::Game and consume logical
ControllerState actions. The shared controls menu can handle physical rebinding;
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
including cases not used to fit the rules, and inspect rendered output. Keep each
generated project's target directory separate to avoid linking another copy's game.

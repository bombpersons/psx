template_create writes a standalone Rust/Bevy workspace: psx-bevy/ (the PS1
look, ticks, input, menus and launcher; the project's copy, changed to suit
the game and listed in docs/template.md), game/ (the port, with a demo to
replace) and tools/import/ (the importers). It can be created in the game
folder itself. For a whole port use port_init instead (guide topic port),
which writes the same template with a goal and catalogue.

Pass the original's display region (ntsc/pal) and choose fields_per_tick
from the original's main loop, not the region alone. The generated
AGENTS.md (root and game/) has the rules: one plugin per feature, logic in
FixedUpdate apart from rendering, states as enums, content as assets,
constants named with the raw value and where it was read.

Build ordinary Bevy entities, components and plugins on decoded assets. Do
not recreate an emulator or translate CPU instructions into a disguised game
loop. Captured .psxscene files are visual reference for the template's
--scene viewer; levels come from the disc's data through the importers.

The generated developer MCP supports exact game_step calls, game_query and
game_screenshot (--mcp --headless --software; screenshot target screen).
Compare rebuilt behaviour against cases recorded from the original (guide
topic cases; psx_bevy::testing::Case in game/tests/cases.rs) and look at
rendered output. Keep each project's target directory separate, so one
game's library is never linked into another.

port_init writes a standalone Rust/Bevy workspace: psx-bevy/ (the PS1
look, ticks, input, menus and launcher; the project's copy, changed to suit
the game and listed in docs/template.md), game/ (the port, with a demo to
replace) and tools/import/ (the importers). It can be created in the game
folder itself. For a whole port use port_init instead (guide topic port),
which writes the same template with a goal and catalogue.

Pass the original's display region (ntsc/pal) and choose fields_per_tick
from the original's main loop, not the region alone. The generated
AGENTS.md (root and game/) has what the code is held to: clean, idiomatic
and reusable, logic in FixedUpdate apart from
rendering, and each constant's source cited.

Build ordinary Bevy entities, components and plugins on decoded assets. Do
not recreate an emulator or translate CPU instructions into a disguised game
loop. Captured .psxscene files are visual reference for the template's
--scene viewer; levels come from the disc's data through the importers.

The generated developer MCP supports exact game_step calls, game_query and
game_screenshot (--mcp --headless --software; screenshot target screen).
Compare rebuilt behaviour against scenario recordings of the original
(docs/scenarios.md; game/tests/scenarios.rs) and look at
rendered output. Keep each project's target directory separate, so one
game's library is never linked into another.

A port is built as two things from the start: an engine (the reusable machinery
the game runs on, usable for a different game without the original's content
or rules) and this game on top of it. Matching the original and reusing the
engine don't conflict when the engine is cut where the original cuts: data the
original interprets from tables becomes engine formats; code the original runs
per object type stays code, ported faithfully. Invented abstractions (a generic
enemy schema, a trap scripting language, a physics component replacing
hand-written movement) lose update order, rounding and special cases.
Restructuring a finished port is much harder, so set the structure up first.

Evidence: the disc and psx are the only sources. Back every ported behaviour
with runtime observations or binary evidence and say which in docs/evidence.md.

Work order:
1. context, then boot the game and save reference checkpoints.
2. Write docs/engine-model.md from evidence before gameplay code: what the
   world is made of (rooms, tiles, BSP, heightmap, polygon soup) and how
   collision and visibility derive from it; whether movement is animation- or
   code-driven; how objects are defined (type table, control function per
   type, which fields are tabled); how levels script events (trigger tables,
   bytecode, hardcoded); how a frame runs (update order, fields per tick).
3. template_create into the game folder (it keeps the disc and an existing
   AGENTS.md). The workspace is psx-bevy/ (template-owned, don't edit),
   engine/ (registries, update-order sets, level and tuning formats, a
   sandbox example), game/ (demo to replace) and tools/import/. Reshape the
   engine skeleton to match engine-model.md; keep the sandbox building.
4. Importers in tools/import convert extracted files into engine formats
   (normal units, RON/PNG/WAV/glTF) under game/assets, recording provenance.
   Validate structurally: every source byte accounted for, every reference in
   range. Raw formats live only in importers.
5. Port feature by feature: record the original (replay + sample), place the
   code by the rules in engine/AGENTS.md, add a trajectory or screenshot
   check, run the sandbox.
6. Keep docs current: what is verified, against what, what is provisional.

Placement, in short: needed by another game on this engine -> engine/, named
generically; read from a table in the original -> data the engine interprets;
hardcoded per type -> a registered controller in game/; a design number inside
a controller -> its tuning file. Never in engine/: this game's names, asset
paths, table magic ids or checks for which game is running. Unknown type ids
and actions are reported, never mapped to something else.

The sandbox (cargo run -p engine --example sandbox) is a tiny second game made
only from engine/ and hand-written assets. If a feature can't be used there
without reaching into game/, move the boundary, not the sandbox.

Keep an editor growing with the port: once a format exists in the engine, the
editor can open, show and save it, through engine/ alone (registry metadata
lists the game's types). Grow it a format at a time, after the format has
settled and the matching trajectory tests pass. Saving an unedited level
reproduces the file exactly. An editor that needs game/ is a boundary bug.

Fidelity is the contract: every restructuring keeps every trajectory test
passing exactly. If a refactor changes a number, find out from evidence
whether the refactor is wrong or the test relied on an accident. Compare
rendered frames against the original and look at them.

Template updates: template_update replaces untouched psx-bevy/ files, writes
<file>.psx-new beside edited ones and copies changed seeded files under
.psx/template-update/ for you to merge. Projects from before the workspace
layout: move the template modules into psx-bevy/, then `psx template adopt`.

Done means: the port matches recorded behaviour and looks like the original;
a new game could add a crate like game/ and author engine-format assets
without editing engine/; levels, object setup and tuning change by editing
assets, in files or in the editor.

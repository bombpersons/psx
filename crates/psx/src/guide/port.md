A port has three goals, in order: it behaves exactly like the original; it is
organised so that new levels, enemies, items and weapons, and changes to the
player's controller, are easy to make; and it has an editor that can author new
content, up to a total conversion. Each goal rests on the one before: an
extension point that changes a ported number is a bug, and the editor can only
author what the engine expresses as data.

A port is built as two things from the start: an engine (the reusable machinery
the game runs on, usable for a different game without the original's content
or rules) and this game on top of it. Matching the original and reusing the
engine don't conflict when the engine is cut where the original cuts: data the
original interprets from tables becomes engine formats; code the original runs
per object type stays code, ported faithfully. Don't replace ported code with
a generic layer while porting (an enemy schema, a trap scripting language, a
physics component replacing hand-written movement): such layers lose update
order, rounding and special cases. Restructuring a finished port is much
harder, so set the structure up first.

Organise for extension as you port. Port shared routines once: when the
original runs one routine for many types (creature AI and movement, pickups,
guns, cutscene playback), port it as one shared module with the per-type
parameters and hooks the original has, and keep only each type's quirks in its
own controller. Copying it into every controller makes every later enemy or
gun a copy-paste job. Each kind of content gets one extension path, a registry
or table entry plus per-type data or code, never an edit to a central match.
Design numbers live in tuning files. Document, per controller, the contract an
asset must meet to use it (joints, animation clips, states, state-change
frame ranges, animation commands, sound ids), so a replacement player model
or a new enemy model can reuse the ported code. Keep each feature plugin
usable on its own: the player must not need the inventory or HUD to run, and
dependencies between features are explicit or optional.

Evidence: the disc and psx are the only sources. Back every ported behaviour
with runtime observations or binary evidence and say which in docs/evidence.md.

Work order:
1. context, then boot the game and save reference checkpoints.
2. Write docs/engine-model.md from evidence before gameplay code: what the
   world is made of (rooms, tiles, BSP, heightmap, polygon soup) and how
   collision and visibility derive from it; whether movement is animation- or
   code-driven; how objects are defined (type table, control function per
   type, routines shared between types, which fields are tabled); how levels
   script events (trigger tables, bytecode, hardcoded); how a frame runs
   (update order, fields per tick).
3. template_create into the game folder (it keeps the disc and an existing
   AGENTS.md). The workspace is built on psx-bevy, a git dependency
   (github.com/bombpersons/psx-bevy; general changes go through a [patch]
   checkout, listed in docs/template.md), with engine/ (registries,
   update-order sets, level and tuning formats, a sandbox example), game/
   (demo to replace), mods/example/ (a mod built on game/), editor/, tools/import/
   and vendor/ (template-owned psx-media and psx-assets for the importers). Reshape the engine skeleton to match
   engine-model.md; keep the sandbox building.
4. Importers in tools/import convert the disc's files into engine formats
   (normal units, RON/PNG/WAV/glTF) under game/assets/imported, recording
   provenance. The game runs them itself on first start from the player's
   disc (game/src/disc.rs; set import::SERIALS, bump import::VERSION when the
   output changes), so players need only the game and their disc.
   Validate structurally: every source byte accounted for, every reference in
   range. Raw formats live only in importers.
5. Port feature by feature: record the original (replay + sample), place the
   code by the rules in engine/AGENTS.md, add a trajectory or screenshot
   check, run the sandbox.
6. Keep docs current: what is verified, against what, what is provisional.

Placement, in short: needed by another game on this engine -> engine/, named
generically; read from a table in the original -> data the engine interprets;
shared by several types in the original -> one module with per-type
parameters; hardcoded per type -> a registered controller in game/; a design
number inside a controller -> its tuning file. Never in engine/: this game's
names, asset paths, table magic ids or checks for which game is running.
Unknown type ids and actions are reported, never mapped to something else.

The sandbox (cargo run -p engine --example sandbox) is a tiny second game made
only from engine/ and hand-written assets. If a feature can't be used there
without reaching into game/, move the boundary, not the sandbox.

Keep the editor (editor/, an egui shell that starts with level files; see
editor/AGENTS.md) growing with the port: once a format exists in the engine, the
editor can open, show and save it, through engine/ alone (registry metadata
lists the game's types). Grow it a format at a time, after the format has
settled and the matching trajectory tests pass. Saving an unedited level
reproduces the file exactly. An editor that needs game/ is a boundary bug.

The editor is a modern desktop application, not part of the game's look. It
runs at the window's full resolution, is driven by mouse and keyboard, and
uses ordinary UI (panels, menus, text fields, drag and drop; egui or Bevy UI
at native scale). Don't build it on psx-bevy's renderer, presentation or
controls menu: no low-resolution framebuffer, dithering, PS1 bitmap font or
pad-driven menus. Its viewports may render levels and models, but the tool
itself should look like a current level editor, not a PlayStation game.

The editor's end state is the tool new content is made with. It creates levels
from nothing (File > New, geometry and collision tools, objects placed from a
library of registered types, triggers, tuning), imports models, textures and
sounds from common formats (glTF, PNG, WAV) through the importers, and
authors animations with the metadata controllers rely on (state changes,
commands, clip numbers), checking a model and its animations against the
contract of the controller it is given to. Animation-driven movement only
works with that metadata, so a new character is a model plus animations that
meet the contract.

Everything a total conversion changes without new behaviour is data the game
loads (level list and order, levels, models, textures, sounds, text, object
setup, tuning), so the editor can author all of it. Give the game a launch
option to run from another asset folder, so such content runs without a new
crate. New behaviour stays code, in a mod crate, and the editor lists its
types from registry metadata.

Mods are crates, built by recompiling, not loaded at runtime. A mod depends on
game/ (a library), adds its plugins and builds its own binary with the same
launcher call. It can add types, actions, levels and tuning, or replace one of
the game's features, without editing game/. The template provides the pieces:
game::GamePlugins is a plugin group a mod can disable a feature in;
replace_object_type and replace_action change a registration deliberately (an
accidental duplicate still panics); asset folders are layered, so a mod adds
the game's assets/ under its own and can replace any file. game/ keeps its
feature plugins, shared modules and components public and each feature plugin
usable on its own. A mod may load the original's assets (players supply their
own disc) or ship its own. mods/example proves it, the way the sandbox proves
the engine: a small game made only of hand-made assets, the ported player
controller driving a new character in a new level. Port it along with the demo,
and grow it with the port (a character whose animations meet the player's
contract, an enemy added through the shared module).

Fidelity is the contract: every restructuring keeps every trajectory test
passing exactly. If a refactor changes a number, find out from evidence
whether the refactor is wrong or the test relied on an accident. Compare
rendered frames against the original and look at them.

After parity, configurable layers can go on top of the ported code, never in
place of it: data-defined variants of ported behaviour (an enemy that reuses
the shared creature module with new parameters, a new pickup kind). Given the
original's parameters, each must reproduce the ported trajectories exactly.

Template updates: template_update moves the psx-bevy git tag in Cargo.toml
(not a fork, branch or local path), replaces untouched vendor/ files, writes
<file>.psx-new beside edited ones and copies changed seeded files under
.psx/template-update/ for you to merge. Projects with an old psx-bevy/
folder are switched to the git dependency; its untouched files are removed
and edited ones reported (moved_to). Workspaces without .psx-template.toml:
`psx template adopt`, then update.

Settings menus: psx-bevy's controls and display menus are logic plus a
built-in view. Once the game's own front end exists, turn the built-in views
off (ControlsUi, DisplayUi), copy view.rs into game/ and restyle it or merge it
into the game's options screen, so they look like part of the game. Don't
restyle them inside psx-bevy (psx-bevy's README, "Making the menus the
game's own").

Done means: the port matches recorded behaviour and looks like the original; a
new enemy, item or weapon of an existing kind is a new entry plus data, without
editing the shared code it runs on; a mod crate can add to or change the game
without editing game/, and the example mod runs from hand-made assets alone; a
new game could add a crate like game/ and author engine-format assets without
editing engine/; levels, object setup and tuning change by editing assets, in
files or in the editor; and the editor can build a new level from nothing.

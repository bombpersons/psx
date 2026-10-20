# Working in the engine

`engine/` is the reusable machinery this game runs on. Someone should be able
to make a *different* game with it, without the original's content and without
this game's rules. It builds on `psx-bevy` (re-exported as `engine::psx`,
`engine::shell`, `engine::testing`, ...); build on it rather than editing it
(`psx-bevy/AGENTS.md` says when an edit is fine).

## Cut it where the original cuts

Most PS1 games already separate:

- **Data the engine interprets**: level geometry, collision, animation tables,
  object placement, trigger and script tables, setup values.
- **Code written per object type**: the player's state handlers, each enemy's
  AI, special traps and bosses.
- **Code shared between types**: routines the original runs for a whole family
  (every creature's AI info, mood, path search, turning and movement; one
  firing path for every gun; one pickup routine). This is a cut too: it
  becomes one module with per-type parameters, in `game/` when it is this
  game's rules, here when another game would need it.

If the engine's formats express what the original's tables express, and
per-object code stays as code, generalising costs no accuracy. Accuracy breaks
when ported code is replaced by abstractions the original doesn't have (a
generic "enemy behaviour" schema, a scripting language for traps, a physics
component that replaces hand-written movement rules): they lose update order,
timing within a tick, integer rounding and special cases. Configurable layers
like these can come after parity, built on top of the ported code, and must
reproduce its numbers exactly when given the original's parameters.
`docs/engine-model.md` records, from evidence, what the original treats as
data, as shared code and as per-type code; this crate follows it.

## Placing code

Decide where something goes before writing it:

1. **Would a different game on this engine need it?** Then it goes here,
   named generically (`Character`, `ObjectType`, `TriggerAction`), not after
   this game's characters or items.
2. **Did the original read it from a table?** Then it's data in an asset that
   engine code interprets. Don't hardcode it.
3. **Did the original run one routine for several types?** Then port it once,
   as a shared module whose per-type parameters and hooks are the ones the
   original has. Don't copy it into each controller.
4. **Did the original hardcode it per object type?** Then it's a controller in
   `game/`, registered with the type registry and ported faithfully, quirks
   included. Don't generalise it into a schema.
5. **Is it a design number inside a controller** (speed, damage, timer)? Put it
   in a per-type tuning file the controller reads (`config::TuningPlugin`).
   Leave structural details (update order, rounding) in code.

Never here: this game's title, character or item names, asset paths, magic
ids from the original's tables, or `cfg`s and branches that test which game is
running.

## Extension points

The skeleton is a starting point; reshape it to match `docs/engine-model.md`.

- `schedule.rs`, **`EngineSet`**: named sets in the original's update order,
  inside `GameSystems::Game`. Game systems join a set rather than ordering
  themselves ad hoc. Rename and reorder the placeholders from evidence.
- `objects.rs`, **object type registry**: type id → spawn hook, plus metadata
  (name, tuning resource). Adding a type means registering it, never editing a
  central `match`. Unknown ids are counted in `UnknownObjectTypes`.
  `replace_object_type` swaps a registered entry (for mods).
- `triggers.rs`, **action registry**: action id → one-shot handler system.
  Unknown actions are logged and counted in `UnknownActions`, never ignored or
  mapped to something else. `replace_action` swaps a registered handler. `TriggerVolume` is a placeholder detector: replace
  it with the original's trigger model and keep the registry.
- `level.rs`, **`LevelData`**: what a level holds, in the engine's format.
  Grow it with what the original's levels actually contain.
- `config.rs`: RON loading for tuning and tables, through the layered asset
  folders (`engine::assets`), so a mod's files replace the game's.

Every kind of content the original dispatches on (object types, pickups,
weapons, trigger actions, cutscenes, cameras, sounds; follow how the original
dispatches each) is added the same way: an entry in a registry or table plus
per-type data or code. A new enemy or weapon of an existing kind should need
no edit to the shared code it runs on.

## Formats

- Normal units: `f32` metres or a documented world unit, radians, seconds or
  ticks, linear colour where it applies. State conversions once, in the
  importer (`tools/import`) and in `docs/`.
- Readable files (RON for tables, PNG, WAV, glTF where they fit), so people
  can author them by hand or with ordinary tools, and an editor can open and
  save them.
- The editor that opens them is a modern desktop application: full window
  resolution, mouse and keyboard, ordinary UI. It does not use psx-bevy's PS1
  renderer, bitmap font or pad menus, and should not look like a PlayStation
  game (see the psx `guide` topic `port`). Its end state creates content from
  nothing, up to a total conversion, so every format must be creatable, not
  only loadable. Fields not yet understood stay named, with a documented
  default a new file can use, rather than opaque blobs copied from the
  original.
- Exact enough that trajectory tests still match. Where float conversion would
  change behaviour (fixed-point accumulation, rounding), keep the integer
  quantity and document why.
- Keep registry metadata (names, reflected tuning types) complete: it is how
  tools list and inspect the game's types without depending on `game/`.

## The sandbox keeps the engine honest

`examples/sandbox.rs` is a second, tiny game built only from this crate and
the hand-written files in `assets/sandbox/`: one room, one controllable
character, one object type with a trivial controller, one trigger. It must not
depend on `game/` or on anything extracted from the disc.

Build and run it in every verification pass:

```sh
cargo run -p engine --example sandbox
cargo run -p engine --features dev-mcp --example sandbox -- --mcp --headless --software
```

If a feature of the port forces you to touch the sandbox in a way a new game's
author wouldn't expect, or the sandbox can't be made without reaching into
`game/`, the boundary is in the wrong place. Fix the boundary, not the sandbox.

## Checks

`cargo test -p engine` (registry and trigger tests), then the workspace checks
in the root README. Every restructuring must keep every game trajectory test
passing exactly.

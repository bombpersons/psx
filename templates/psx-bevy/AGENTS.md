# Working in this project

A native Bevy port of a PlayStation game, built as an engine and a game from
the start. Read the guide for the crate you are working in:

- `engine/AGENTS.md`: what belongs in the engine, the placement rules, the
  sandbox, formats people can author.
- `game/AGENTS.md`: turning reverse-engineering evidence into idiomatic Bevy
  code, keeping it extensible, mods (`mods/example`), tick rate, trajectory
  tests, verification.
- `tools/import/`: the importers, run by the game on first start from the
  player's disc (`game/src/disc.rs`); set `GAME`, `SERIALS` and bump
  `VERSION` there. `vendor/` holds the template's copies of psx-media and
  psx-assets for them; don't edit it (fix upstream in psx).
- `editor/AGENTS.md`: the editor for the engine's formats, a modern desktop
  tool (egui) that depends on `engine/` only and grows with the port.
- `psx-bevy/`: the template's crate, updated by `psx template update`. Build
  on it from `engine/` and `game/`; edit it only as `psx-bevy/AGENTS.md`
  allows, and list every edit in `docs/template.md`. Its README documents
  the PS1 look, input, testing and the developer MCP.

The psx toolkit's `guide` tool (topic `port`) has the porting workflow.
Routine check: `cargo test --workspace --all-features`.

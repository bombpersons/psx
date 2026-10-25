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
  tool (egui) that depends on `engine/` only and grows with the port. Its MCP
  server lets agents build levels with a person and test the editor; every
  new editor feature gets tools there too.
- `psx-bevy/AGENTS.md`: the base the engine is built on (the PS1 look,
  ticks, input, menus, testing, launcher). It is this project's copy:
  change it to suit the game, and list each change in `docs/template.md` so
  `psx template update` stays easy to merge. Its README documents the PS1
  look, input, testing and the developer MCP; `cargo doc --open -p psx-bevy`
  has its API.

The psx toolkit's `guide` tool (topic `port`) has the porting workflow.
Routine check: `cargo test --workspace --all-features`.
Iteration builds link Bevy as a shared library (default feature `dynamic`),
which only `cargo run` and `cargo test` find: start binaries through cargo,
not from `target/`. Shipping build: `cargo build --release
--no-default-features`.

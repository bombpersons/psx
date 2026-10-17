# Working in this game

This project was made with `psx template create`. The template's guides apply:
`game/AGENTS.md` (idiomatic Bevy, one plugin per feature, rules separate from
presentation, "From evidence to idiomatic code") and `engine/AGENTS.md` (what
belongs in the engine). Don't edit `psx-bevy/`; `psx template update` replaces it.

- The game is the library in `game/src/lib.rs` (crate `game`): `REGION` is PAL and
  `FIELDS_PER_TICK` is 2, so logic runs at Croc's 25 Hz and button edges match
  the original's.
- `game_query` (developer MCP) reads Croc's state, e.g.
  `{"with":["Croc"],"components":["Transform","Vertical"]}`.
- Keep game assets in `game/assets/`. Do not commit original disc or BIOS data.

## This project

- Croc's movement rules live in `game/src/croc/movement.rs`; each constant cites
  its evidence (RAM address or function in the PAL `CROC.EXE`). Keep them in
  natural units (Croc units via `units()`, radians via `turns_4096()`, per
  25 Hz tick) rather than the original's fixed-point.
- `cargo test -- --nocapture` replays `game/tests/fixtures/*.csv` (recorded from the
  original) and must stay within its bounds after any rule change.

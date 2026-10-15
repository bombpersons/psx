# Working in this game

This project was made with `psx template create`. The template's AGENTS.md
(`templates/psx-bevy/AGENTS.md` in the psx repository) applies: idiomatic Bevy,
one plugin per feature, rules separate from presentation, and its guide "From
evidence to idiomatic code".

- The game is the library in `src/lib.rs` (crate `game`): `REGION` is PAL and
  `FIELDS_PER_TICK` is 1, so logic runs at 50 Hz like the original.
- The rules live in `src/player/movement.rs`. Constants are the original's
  exact 16.16 per-tick values (all exact in `f32`); each cites its RAM or code
  address. Keep it a per-tick simulation: the tests compare every tick.
- `cargo test -- --nocapture` replays `tests/data/*.csv` (recorded from the
  original) and must stay at zero error after any rule change.
- `game_query` (developer MCP) reads `StagePos`, `PlayerState` and the
  `StageCamera` resource, e.g. `{"with":["Player"],"resources":["StageCamera"]}`.
- The captured backdrop is a stand-in; a real stage comes from decoded tile
  and collision data (research/case-study-megaman8.md, section 5).
- Keep game assets in `assets/`. Do not commit original disc or BIOS data.

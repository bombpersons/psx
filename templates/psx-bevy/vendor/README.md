# Vendored psx crates

Copies of crates from the [psx toolkit](https://github.com/bombpersons/psx),
so the game needs neither the toolkit nor any download:

- `psx-media`: CUE/BIN, ISO and CHD disc images, ISO9660, `SYSTEM.CNF`, PS-X
  EXE, for importing from the player's disc (`tools/import`,
  `game/src/disc.rs`).
- `psx-assets`: TIM, TMD, VAB/VAG and SPU-ADPCM decoding.
- `psx-fixed`: numbers for exact mechanics, `Fixed` with the original's
  width and rounding or `f32` from one generic mechanic (`psx_bevy::num`).
- `psx-gte`: the GTE's operations, flags and rounding as psx emulates them
  (`psx_bevy::gte`).
- `psx-mechanics`: the mechanic, interface, trace, scenario and recording
  formats, read by step replay (`psx_bevy::replay`) and scenarios
  (`psx_bevy::scenario`).

They belong to the template: `psx template update` replaces them. Unlike
`psx-bevy/`, they are not for changing here: if one needs a fix, fix it in
the psx repository. A local edit is kept on update and the new version is
written beside it as `.psx-new`.
They are excluded from this workspace's members, so `cargo test --workspace`
doesn't run their tests (those run in the psx repository).

License: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).

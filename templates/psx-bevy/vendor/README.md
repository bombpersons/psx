# Vendored psx crates

Copies of two crates from the [psx toolkit](https://github.com/bombpersons/psx),
so the game can import from the player's disc (`tools/import`,
`game/src/disc.rs`) without the toolkit or any download:

- `psx-media`: CUE/BIN, ISO and CHD disc images, ISO9660, `SYSTEM.CNF`, PS-X EXE.
- `psx-assets`: TIM, TMD, VAB/VAG and SPU-ADPCM decoding.

They belong to the template: `psx template update` replaces them. Unlike
`psx-bevy/`, they are not for changing here: if one needs a fix, fix it in
the psx repository. A local edit is kept on update and the new version is
written beside it as `.psx-new`.
They are excluded from this workspace's members, so `cargo test --workspace`
doesn't run their tests (those run in the psx repository).

License: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`).

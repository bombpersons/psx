# psx-bevy: the base this game is built on

The PS1 look, logic ticks, controller input, the controls and display menus,
testing, the developer MCP and the launcher. It came from the psx template,
and it is this project's copy: change it to suit the game. A port that needs
the original's rendering tricks (its own sorting, clipping, subdivision,
blending quirks) makes them here.

Before changing it, check whether the change belongs somewhere else:

- Something only this title needs (its rules, numbers, assets): `game/`.
- Machinery for this engine family that sits on top of psx-bevy (new object
  kinds, level formats, render helpers built from psx-bevy's pieces):
  `engine/`, wrapping or re-exporting what is here.
- A launcher flag or setup step: plugins in the `add` closure of
  `launcher::Game`, or build the app with `launcher::app` and adjust it
  before `run()`.

Change psx-bevy itself when the change is to how the PS1 look, ticks, input,
menus or testing work: a different pipeline, a new material option, the
original's way of drawing something.

- List each change in `docs/template.md` with why. `psx template update`
  replaces the files you haven't changed and writes the new version beside
  each one you have, as `<file>.psx-new`; the list makes those merges easy.
- To merge a `.psx-new`: bring the template's changes into your file while
  keeping yours (or take the new version and reapply the change listed in
  `docs/template.md`), then delete the `.psx-new`. Deleting it marks the file
  merged.
- Keep its tests passing: `cargo test --workspace --all-features`. The GPU
  checks (`examples/depth_cue.rs`, `examples/depth_bias.rs`,
  `examples/screen_size.rs`, `tests/dev_mcp.rs`) run with
  `cargo test -p psx-bevy --all-features -- --ignored --nocapture`.
- A fix or option any PS1-style game would want can be offered to the psx
  toolkit too, so later templates ship it.

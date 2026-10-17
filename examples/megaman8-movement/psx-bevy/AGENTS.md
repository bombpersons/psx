# psx-bevy: don't edit

This crate belongs to the psx template. `psx template update` replaces it
with newer versions; local edits are not merged, only reported (the new file
lands beside yours as `<file>.psx-new`).

To change behaviour, build on it instead:

- New rendering, input or test helpers for this engine family: add them to
  `engine/`, wrapping or re-exporting what is here.
- Game-specific behaviour: `game/`.
- A launcher flag or setup step the game needs: add plugins in the `add`
  closure of `launcher::Game`, or build the app with `launcher::app` and
  adjust it before `run()`.

If something here is genuinely wrong or missing, note it for the psx toolkit
(it is fixed upstream and arrives with the next update) and work around it in
`engine/` meanwhile.

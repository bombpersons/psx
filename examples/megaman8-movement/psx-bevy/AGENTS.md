# psx-bevy: edit sparingly, and record every edit

This crate belongs to the psx template. `psx template update` replaces the
files you haven't touched with newer versions. A file you edited is left as
it is, and the new version lands beside it as `<file>.psx-new` for you to
merge. Edits are allowed; the aim is that updates stay easy to merge.

First try building on it without editing:

- New rendering, input or test helpers for this engine family: add them to
  `engine/`, wrapping or re-exporting what is here.
- Game-specific behaviour: `game/`.
- A launcher flag or setup step the game needs: add plugins in the `add`
  closure of `launcher::Game`, or build the app with `launcher::app` and
  adjust it before `run()`.
- Geometry the original changed per frame (polygon subdivision, LOD,
  clipping) is mesh data the engine owns. `PsxMaterial` snaps and
  affine-maps whatever vertices it gets, so rebuild the mesh rather than
  changing the shader.

Edit here when that can't be done cleanly, e.g. a missing hook, pipeline
option or launcher setting:

- Only general PS1 machinery. Nothing specific to this game: that goes in
  `game/`, or in `engine/` behind a hook the game fills.
- Keep edits small and local. Add rather than rewrite; don't move, rename
  or reformat code; keep the existing public API working.
- List every edited file in `docs/template.md` with the change and why.
- On update, merge each `<file>.psx-new` into your file (take the new
  version and reapply the change listed in `docs/template.md`), then delete
  the `.psx-new`.
- If the edit would help any game, note it for the psx toolkit as well, so
  it can arrive upstream and the local edit can be dropped.

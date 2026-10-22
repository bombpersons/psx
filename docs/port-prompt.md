# Example port prompt

A starting `AGENTS.md` for a folder holding a game's disc image. The workflow
details live in psx (`guide` topic `port`) and in the generated workspace's
`AGENTS.md` files, so the prompt only states the goal and the rules of
evidence.

```markdown
# Port this PlayStation game

You have `psx`, a toolkit for reverse engineering PlayStation games. This
folder holds the game's disc image.

Goal: a Bevy port that looks and plays as close as possible to the original,
built as a reusable engine plus this game on top of it, with an editor for
the engine's formats that grows alongside the port.

- Start with psx `context`, then `guide` topic `port`, and follow it.
- Create the project here with `template_create` (it keeps this file and the
  disc) and work in its workspace.
- The disc is an asset source only. The game imports its data from the
  player's disc on first start (`tools/import`, `game/src/disc.rs`); nothing
  from the disc is committed, and the code must not need the disc or BIOS
  to build.
- Evidence: use only this disc and psx. Don't consult other reimplementations,
  decompilations or fan documentation of this game. Back every ported
  behaviour with runtime observations or binary evidence, and record which in
  `docs/evidence.md`.
- Keep the editor (`editor/`) in step: when a format lands in the engine,
  the editor learns to create, open, show and save it, depending on
  `engine/` only.
- The editor is a modern, high-resolution desktop application for mouse and
  keyboard. Don't reuse psx-bevy's PS1 renderer or menus for it; it should
  look like a current editor, not a PlayStation game.
```

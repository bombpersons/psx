# The catalogue

One file per feature of the game, named by its id: `F-0001.md`,
`F-0002.md`, and so on. A feature is one capability a player would name:
"car handling on tarmac", "Croc's jump", "the race HUD".

```markdown
---
id: F-0007
title: Car handling on tarmac
area: driving
depends: [F-0002, F-0005]   # car setup data, track surfaces
mechanics: []               # named and captured after the confirmation
scenarios: []
---
A paragraph in game terms: what the player sees and does, and what is in
and out of this feature.
```

- `id` is the file's name. `title`, `area` and the paragraph are required.
- `depends` lists the features this one cannot work without. The goal's
  features and everything they depend on are its closure: psx works it out.
- A feature is its own when someone could want it without its neighbours,
  and when it has behaviour of its own. "Physics" is too coarse, "the
  friction curve" too fine.
- `mechanics` lists the steps the original runs for the feature, each
  captured under `mechanics/<id>/` (docs/mechanics.md). It may stay empty
  until the goal is confirmed; `psx port next` asks for them then.

`goal.toml` lists the features the person wants. `psx port confirm` shows
the list, the closure and any problems; the person confirms it with
`psx port confirm --yes`. After that the list may grow; taking a feature
out needs the person to confirm again.

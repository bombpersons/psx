# The catalogue

One file per feature of the game, named by its id: `F-0001.md`,
`F-0002.md`, and so on. A feature is one capability a player would name:
"car handling on tarmac", "Croc's jump", "the race HUD".

```toml
+++
id = "F-0007"
title = "Car handling on tarmac"
area = "driving"
depends = ["F-0002", "F-0005"]   # car setup data, track surfaces
code = ["0x8002D810"]
questions = ["does the handbrake change grip, or only steering?"]

[recordings]
work = ["tarmac/full_lock"]
held_out = ["tarmac/lap_one"]

[wont_reach]                     # branches no recording can reach, and why
"0x8002D8A4" = "the debug camera's car: no game mode sets it"
+++
A paragraph in game terms: what the player sees and does, and what is in
and out of this feature.
```

The front matter is TOML between `+++` lines. Top-level keys come before
the `[recordings]` and `[wont_reach]` tables.

- `id` is the file's name. `title`, `area` and the paragraph are required.
- `depends` lists the features this one cannot work without. The goal's
  features and everything they depend on are its closure: psx works it out.
- A feature is its own when someone could want it without its neighbours,
  and when it has behaviour of its own. "Physics" is too coarse, "the
  friction curve" too fine.
- `code` lists the original's functions that are the feature, by entry
  address, found with psx (`code coverage diff`, watches): each must be a
  function the inventory has.
- `recordings` names the scenario recordings the feature is checked
  against, as `<scenario>/<name>`: `work` while it is built, `held_out`
  once it matches. This is the only place a feature names them; its
  scenarios are those its recordings come from.
- `wont_reach` gives the branches of its code no recording can reach, each
  a conditional branch in one of its functions, with why. The person
  confirms them with `psx port confirm --yes`; one added or changed later
  needs confirming again.
- `questions` lists what is not known yet.

All but `id`, `title` and `area` may stay empty until the goal is
confirmed; `psx port next` asks for them.

`goal.toml` lists the features the person wants. `psx port confirm` shows
the list, the closure, the won't-reach branches and any problems; the
person confirms it with `psx port confirm --yes`. After that the list may
grow; taking a feature out needs the person to confirm again.

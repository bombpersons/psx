# The catalogue

One file per feature of the game, named by its id: `F-0001.md`,
`F-0002.md`, and so on. A feature is one capability a player would name:
"car handling on tarmac", "Croc's jump", "the race HUD".

```toml
+++
id = "F-0007"
title = "Car handling on tarmac"
area = "driving"
code = ["0x8002D810"]
assets = ["/TRACKS/TARMAC1.TRK"]  # what its recordings need to replay
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
- Features have no dependencies. When one needs another's rules, its
  recordings fail there, and port check's pointer names the other's code;
  `goal.toml` lists every feature the goal needs.
- A feature is its own when someone could want it without its neighbours,
  and when it has behaviour of its own. "Physics" is too coarse, "the
  friction curve" too fine.
- `code` lists the original's functions that are the feature, by entry
  address, found with psx (`code coverage diff`, watches, the pointer):
  each must be a function the inventory has. It is your reading of the
  original, revised as you learn: add a function when its quantities
  turn out to come from it, take one out when it turns out to be
  another's. A function may be several features' (one state machine for
  jump and climb); its branches are covered once, by any of their
  recordings.
- `recordings` names the scenario recordings the feature is checked
  against, as `<scenario>/<name>`: `work` while it is built, `held_out`
  once it matches. This is the only place a feature names them; its
  scenarios are those its recordings come from.
- `wont_reach` gives the branches of its code no recording can reach, each
  a conditional branch in one of its functions, with why. A range
  (`"0x8002D800..0x8002D860"`, the end excluded) names a block of them
  no recording reaches: it answers for the branches in it no recording
  ran, and a recording that runs one makes it a problem.
  `psx port confirm` says how many branches each range answers for. The
  person confirms them with `psx port confirm --yes`; one added or changed later
  needs confirming again. Code taken out takes its entries with it, and
  an entry moved with its code to another feature keeps its
  confirmation.
- `assets` names the disc files its recordings need to replay (a track, a
  level), as `goal.toml`'s `assets` names them (`formats/README.md`): psx
  asks for them to be imported and checked before the feature's own work,
  whatever the goal says about the rest of the disc.
- `questions` lists what is not known yet.
- What drawing shows is checked through quantities, not screens: the
  camera, animation and the HUD are features of their own, with their
  quantities in the glossary and their recordings. `psx scenario show
  --screens` puts both games' screens side by side for you to look at.

## Code no feature ports

Every function the goal's recordings run is some feature's code or listed
in `not_ported.toml`, by entry or address range (the end excluded), with
why:

```toml
"0x80010088" = "memcpy: the port has its own"
"0x80050000..0x80058000" = "libgte's helpers: the port does its own maths"
```

Library code, the CD and sound drivers, the GPU's command building and
debug code go here; the port has its own of those, or none. A feature is
reviewed only once the code its recordings run is all accounted for. When
code listed here is on the path to quantities that differ, the pointer
says so.

All but `id`, `title` and `area` may stay empty until the goal is
confirmed; `psx port next` asks for them.

`goal.toml` lists the features the person wants. `psx port confirm` shows
the list, the won't-reach branches and any problems; the
person confirms it with `psx port confirm --yes`. The first list is a
guess made before you know the game: recut it as you learn.

## Merging and splitting

Merging and splitting are edits: write the new entries, move the
recordings and the code (with their `wont_reach` entries) to them, and
change `goal.toml`'s list. Every recording a goal feature has named stays
named by one while its file is there (psx keeps the list in
`.psx/goal_recordings.json`, and one left unnamed is a problem); a work
recording no longer wanted is deleted. The person's held-out plays stay
held out: one deleted or moved to work asks them to confirm again.
Signs a cut may not hold: a feature's pointer keeps finding other
features' code, a feature has no recording of its own, or features
claiming the same functions each run code only the others claim. The
review asks whether the features cover the goal's description.

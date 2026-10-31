# Design: porting by recordings

How psx ports a PlayStation game, or a part of one: what the person asks
for, how the agent finds and records the original's behaviour and assets,
how the port is checked against the original, and how progress and the
next thing to do are known.

This replaces the earlier design of the same name (porting by goals and
mechanics, in git history), which captured each of the original's routines
as a mechanic and replayed it exactly. That proved routines bit for bit at
a cost of a session or more each. Porting Croc's running and jumping
against recordings instead took an afternoon and matches the original on
every tick of its recordings; Tomb Raider II's port reached Lara's whole
controller the same way. Recordings, especially ones a person plays, are
the test: when the port and the original agree tick by tick through a run,
the behaviour is the same, however the port implements it.

## 1. Goals

1. **Port what the person wants**: one feature (a character controller),
   several, or the whole game.
2. **Reusable, modern code.** Idiomatic Rust and Bevy: plugins,
   components, systems; assets in modern formats (PNG, glTF, WAV/OGG,
   RON). A feature works with the person's own content, so it can be the
   base of a new game.
3. **Exact where it matters.** Behaviour matches the original's
   recordings, to a tolerance set per quantity.
4. **Progress you can see, and where to look next**, worked out from
   evidence psx can check, never from what the agent says.
5. **Cheap.** One interactive session at a time; state on disk; a short
   guide.

## 2. The loop

1. **The person says what they want.** The agent proposes the features it
   implies; the person confirms the list (`goal.toml`).
2. **Find the code.** Coverage diff (a recording doing the thing against
   one not doing it), write watches on the values involved, disassembly
   and decompilation. The routines found are the feature's code.
3. **Decide what to compare.** The glossary names the quantities (Croc's
   x, y, z, heading), and its binding says where the original keeps them.
4. **Record.** Scenario recordings of the original: scripted by the agent,
   or played by the person (section 6). Each recording gets the
   quantities at the end of every tick.
5. **Implement**, reading the original's code. Every rule cites where it
   came from: an address, or the recording that showed it when the code is
   out of reach (a script interpreter's bytecode).
6. **Compare** the port with each recording, tick by tick.
7. **Ask `port next`** for the next item, until there is none.

### Rules

- **Read, don't fit.** A constant tuned until a curve matches passes the
  recordings it was tuned on and fails the next one. Constants come from
  the code; when only a recording shows a rule (Croc's stomp hang, 15
  ticks), the citation says so.
- **Hold recordings out.** Some recordings are only checked once the
  agent says a feature matches; the person's play-throughs are the best
  ones. A feature is not done until its held-out recordings pass.
- **Compare every tick.** The end state summarises; the first tick that
  differs is where to look.
- **The world a recording needs.** A feature tested on its own runs in a
  stand-in world (a flat floor), and only recordings that fit the stand-in
  count for it. A full port loads the original's level and starts from the
  recording's state; that is work of its own, tracked as a feature.

## 3. The model

| Thing | What it is | Where |
|---|---|---|
| Goal | the features the person wants | `goal.toml` |
| Feature | one capability a player would name: "Croc's jump", "the race HUD", "level geometry" | `catalogue/<id>.md` |
| Glossary | named quantities, and the binding that reads them from the original | `glossary/` |
| Scenario | a place to start (a state) and what is compared there, with tolerances | `scenarios/<id>.toml` |
| Recording | the original's quantities per tick for one input, scripted or played | `scenarios/<id>/<name>.jsonl` |
| Review | findings on a feature's code by someone who did not write it, each answered | `reviews/<id>.md` |
| Verdict | psx's result of the port against one recording: pass, or the first tick and quantity that differ | `.psx/verdicts/` |
| Format | a disc file's format, read by an importer | `formats/<id>.md`, `tools/import` |

A catalogue entry, its TOML front matter between `+++` lines and then a
paragraph in game terms:

```toml
+++
id = "F-0003"
title = "Croc's jump and stomp"
area = "movement"
depends = ["F-0002"]              # running
code = ["0x8002D810", "0x8002D8D4", "0x80028688", "0x800286F4", "0x80028734"]
questions = ["does the floor snap (y - floor < 131) apply while rising?"]

[recordings]
work = ["level_1_1_jump/short_and_long", "level_1_1_jump/stomp"]
held_out = ["level_1_1_walk/free_play"]

[wont_reach]                      # branches no recording can reach, and why
"0x8002D820" = "under water: part kind 1 (F-0011)"
+++
```

The entry is the one place a feature names its recordings: a scenario
does not name a feature, and the feature's scenarios are those its
recordings come from. `code` is found with psx (coverage diff, watches),
not guessed: each address is a function the inventory has. Each
`wont_reach` address is a conditional branch in one of those functions,
and needs the person's confirmation (`port confirm` keeps the entries they
confirmed, reasons and all).

## 4. Assets

The same loop, with RAM and VRAM as the evidence:

1. **Inventory the disc**: every file, with the standard formats found by
   `assets scan` (TIM, TMD, VAB, XA, STR).
2. **Find each unknown file's loader**: `--trace-cd` and `loads` show
   which file goes where in RAM; a watch on that buffer shows the code that
   parses it; disassembly gives the format (`formats/<id>.md`).
3. **Write the importer** (`tools/import`), converting to modern formats.
4. **Check it against what the game built**: decoded textures against the
   VRAM the game uploads, parsed structures against the RAM the game
   builds after loading, geometry against a GPU capture of the same view.

Each disc file is unknown, format found, imported, checked, or unused (the
game never loads it).

## 5. Progress: `port status` and `port next`

### Stages

| Stage | When |
|---|---|
| Located | its code is found |
| Recorded | it has recordings |
| Matching | every working recording passes |
| Reviewed | matching, every branch of its code covered by a recording or marked won't-reach, no question open, and its code reviewed by someone who did not write it, with every finding answered |
| Done | reviewed, and its held-out recordings pass |

Coverage is the union of what the original ran across a feature's
recordings, within the feature's code. "Done" needs every branch resolved,
not a percentage, so the uncovered ones always say what to record next.

### The review

The recordings check behaviour, not shape: a line-by-line transliteration
of the assembly passes them as well as a rewrite into rules. The agent
that wrote a feature, fresh from its disassembly, is the worst judge of
whether the code reads as rules, so someone who has not seen that work
reads it: a sub-agent or a new session, given the brief `port next` prints.
The reviewer reads the feature's catalogue entry, its code and how it sits
in the rest of the game (duplication with other features, something that
belongs shared), but not the disassembly, and judges it against the
workspace's AGENTS.md. Its findings go in `reviews/<id>.md`:

```markdown
---
commit: 3f2a91c
files: [game/src/croc/jump.rs, game/src/croc/mod.rs]
---
## Stomp is a bool beside the jump state
`stomping: bool` lives next to `JumpState`; a stomp is a state of the jump.
**Answer:** fixed in 7c01d2e: `JumpState::Stomp { hang }`.

## One system per original routine
...
```

The writer answers every finding: fixed, or kept and why. The review
holds while its files are unchanged since its commit; a change to them
asks for another.

### `port status`

```
goal: Croc's movement                      3/7 features done
assets  42/118 files checked · 9 importers · 31 unknown

running      done      9/9 pass (2 held out) · branches 94/94 resolved
jumping      matching  7/7 pass · held out: none yet · 41/48 branches
               uncovered: rise 0x8002D96C (CROSS let go after easing starts)
               open: floor snap while rising?
stomp        matching  3/3 pass · 22/30 branches
collision    recorded  2/14 pass · first: walk_and_turn tick 32 croc_y
swimming     located   no recordings
```

### `port next`

One item, in this order:

1. The port cannot replay a recording (its world or start state is
   missing): build that.
2. A feature without its code: record with and without it, diff the
   coverage.
3. A failing recording, the earliest divergence first.
4. An open question.
5. An uncovered branch: a recording that reaches it, phrased as what to do
   ("jump, and let go of CROSS once the rise eases"). The agent records it
   when it can script it; otherwise it is the person's.
6. A feature whose own work is finished (matching, branches resolved, no
   question open) without a current review: the review's brief, or the
   unanswered findings.
7. A reviewed feature without held-out recordings: the person's.
8. Assets: an unknown file, then an importer not yet checked.

Items for the person (**Play**, **Confirm**) stop the agent until the
person answers. When `port next` has nothing left, the goal is done.

## 6. Recordings the person plays

`psx scenario play <id> --name <name>` (the handoff, folded into scenarios) opens
the game at the scenario's state with a request ("run along the wall and
stomp onto the ledge"); the person plays and the run becomes a recording,
with the same frame origin as a scripted one. `--from <recording>`
continues from the end of an earlier one, so a long play-through can be
split.

## 7. The tools

[feature-inventory.md](feature-inventory.md) lists what psx keeps, merges
and removes: twelve commands (`context`, `bios`, `guide`, `read`, `disc`,
`assets`, `code`, `ram`, `run`, `states`, `scenario`, `port`, and `mcp`
to serve them). The emulator and its run options (watch, sample, trace),
disassembly and decompilation, coverage, the glossary and scenarios,
assets, the template and its developer MCP stay; mechanics (capture,
traces, replay, audits, interfaces) and cases go.

## 8. Decisions

1. A feature is done only when every branch of its code is resolved
   (covered or won't-reach), not at a coverage threshold.
2. Mechanics are removed from psx. The exact Rust already written for Croc
   stays in that workspace's folders; nothing further is planned for it.
3. A feature's code is reviewed by someone who did not write it before it
   is done; its own author's self-review is not enough.

The work to get there is in [recordings-plan.md](recordings-plan.md).

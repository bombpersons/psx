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
| Review | findings on the port's code as a whole by someone who did not write it, each answered | `review.md` |
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
not guessed: each address is a function the inventory has. When the rules
are a script the game interprets, `code` also names stretches of the
programs the glossary's binding declares (`croc+0x1803..0x18A0`), covered
by the bytes read and the decisions' ways
([script-coverage-design.md](script-coverage-design.md)). Each
`wont_reach` address is a conditional branch in one of those functions.
A `wont_reach` address range (`"0x8002D800..0x8002D860"`, the end
excluded) answers for a block no recording reaches: the branches in it
that no recording ran, so a branch taken one way still needs its own
entry or a recording, and a recording that runs a branch in the range
makes it a problem. Its branches must all be in the functions `code`
lists. Each `wont_reach` entry needs the person's confirmation (`port confirm` keeps the entries they
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
game never loads it). A format is `formats/<id>.md` (TOML front matter:
its files and the code that parses them, then the layout); a file psx
finds whole as a standard format needs none. `port check` runs the
importers on the disc (imported: an output's provenance names the file)
and their tests (checked: a passing test marked it).

How much is extracted is the person's choice. An asset name is a disc path
or archive entry (`/MAPS/*.WLD`, `/CROCFILE.1:CROC.*`) or `format:<id>`,
every file of that format. `goal.toml`'s `assets` is `true` (every file),
`false`, or a list of names (`["format:level", "format:car"]`), confirmed
with the features. A catalogue entry's `assets` names what its recordings
need to replay (1-1's geometry for "Croc on slopes"); those come in
whatever the goal says. A `format:` id no doc has yet is first a format to
find; a name reaching into a file no doc calls an archive, that archive's
format; a name that matches nothing is a problem. An archive's entries are
assets of their own: its format doc says `archive = true`, and the
importer lists the entries (`import::index`), which `port check` asks for.

## 5. Progress: `port status` and `port next`

### Stages

| Stage | When |
|---|---|
| Located | its code is found |
| Recorded | it has recordings |
| Matching | every working recording passes |
| Reviewed | matching, every branch of its code covered by a recording or marked won't-reach, no question open, its assets checked or unused, and the code reviewed by someone who did not write it, with every finding answered |
| Done | reviewed, and its held-out recordings pass |

Coverage is the union of what the original ran across a feature's
recordings, within the feature's code: its machine code's branches, and
its script stretches' bytes and decisions. The other goal features' work
recordings that pass at HEAD count too: a foundation feature's code (the
controller, collision) runs in every recording, and a branch another
feature's passing recording took both ways needs no recording of its own.
Only passing ones count, so the port is known to match the original
there; status says how many branches were resolved that way. "Done" needs every branch resolved,
not a percentage, so the uncovered ones always say what to record next.

### The review

The recordings check behaviour, not shape: a line-by-line transliteration
of the assembly passes them as well as a rewrite into rules. So the
review leaves behaviour alone and asks one question of the code: would
you write a new game like this? The agent that wrote it, fresh from the
disassembly, is the worst judge, so someone who has not seen that work
reads it: a sub-agent or a new session, given the brief `port next`
prints. The reviewer reads the port's code as a whole (`game/`, `tools/`)
and not the catalogue, the notes or the disassembly, and judges it as code
written for a new game today: how it is divided, names, how readily it
takes another character, level or rule, duplication, and what is shaped
by the original rather than chosen. Each finding says what to change and
the shape to choose instead; what the code does stays, and the recordings
prove it still does. The findings go in `review.md`:

```markdown
+++
commit = "3f2a91c"
features = ["F-0001", "F-0002"]
finished = ["F-0001"]
+++
## One plugin per feature
Running, the jump and the stomp are four plugins sharing one step order;
a character controller written today is one plugin whose moves are
states of one machine, so a new move is a state, not a plugin.
**Answer:** done in 7c01d2e: `CrocController` with `Move::{Run, Jump, Stomp}`.

## Components documented by the original's offsets
...
```

The writer answers every finding: done, or kept and why. The review holds
while the code is unchanged since its commit. A change (the restructuring
its answers made, among others) asks for a follow-up: a reviewer reads
only the diff since that commit, keeps the findings and answers, moves the
commit on and adds new findings.

One review covers the whole code, not one per feature. Croc's movement
showed why: each feature's reviewer read that feature's slice and checked
it against its catalogue entry, so nobody saw that running, the jump and
the stomp had become four plugins where a game written from scratch would
have one character controller; and a whole review after every fix, with a
comment changed in a file three features share, brought six rounds.

It comes each time a feature comes to match its recordings, and again
when it is finished (branches resolved, no question open), not only
once the goal's features all have: a whole port reviewed only at the end
would be one huge review, and a shape chosen badly for the first feature
would carry every feature built on it. At the match, because `port next`
puts every feature's fixes before any feature's cover work: on Croc five
features matched and none finished, so 43 commits went by unreviewed.
Most of a feature's code is written once it matches. At the finish too,
because the cover work can add as much again: on Croc every feature had
matched and been reviewed, then the slides and a collision refactor
(+931/-534 lines over 18 commits) came with no review due until the
last feature finished. The review's front matter lists the features
matching when it was written (`features`) and those finished then
(`finished`); a matching or finished feature it does not list, with the
code changed since, asks for a follow-up. A feature that matches and
finishes at once asks once. Building the next feature changes the code
without asking for one, until the commits since the review have added
and removed `review_lines` lines of it (goal.toml, 800 by default): then
a follow-up comes whatever the features do. Features that share code (a
character controller under running, walking and crouching, each compared
exactly) can need most of it written before any of them matches, as
Tomb Raider Chronicles' locomotion did. Uncommitted work does not count.
Findings are answered as they come. The follow-up reads the change in the shape of the
code around it, so the whole is still what is judged.

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

0. A feature's assets (its entry's names) not checked or unused: Format,
   Index, Import, Check, since its recordings cannot replay without them.
1. The port cannot replay a recording (its world or start state is
   missing): build that.
2. A feature without its code: record with and without it, diff the
   coverage.
3. A failing recording, the earliest divergence first.
4. An open question.
5. An uncovered branch: a recording that reaches it, phrased as what to do
   ("jump, and let go of CROSS once the rise eases"). The agent records it
   when it can script it; otherwise it is the person's.
6. The goal's assets: a name that covers nothing yet, an unknown file,
   an archive not listed, then an importer not yet checked.
7. Every feature's own work finished (matching, branches resolved, no
   question open) and the code without a current review: the review's
   brief, a follow-up's, or the unanswered findings. The same comes
   before item 0 when a feature comes to match its recordings, or
   finishes, and the review does not list it so, when the code changed by
   `review_lines` lines in the commits since the review, and whenever
   findings have no answer.
8. A feature without held-out recordings, the code reviewed: the person's.

**Confirm** and **Stuck** stop the agent until the person answers. A
**Play** item does not: the agent starts the play request, which joins a
queue, and `port next` goes on to other work, skipping features that wait
on a play; when the person answers, a **Read** item comes first. **Wait**
is left only when nothing but unanswered plays remains. When `port next`
has nothing left, the goal is done.

## 6. Recordings the person plays

`psx scenario play <id> --name <name>` (the handoff, folded into scenarios) asks
the person to play from the scenario's state with a request ("run along
the wall and stomp onto the ledge"); the run becomes a recording, with the
same frame origin as a scripted one. `--from <recording>` continues from
the end of an earlier one, so a long play-through can be split.

A request joins the project's queue (`.psx/handoffs/<run>/`) and the call
returns at once. The person answers when they can: in `psx web`, in the
browser or a window, or with `psx play` (the waiting requests of every
project, played by number or id). They can decline one with a note, which
reaches the agent (why, or what to ask instead). `--status` reads the
answer and marks it read; `--wait` blocks for it up to 600 s.

## 7. The tools

[feature-inventory.md](feature-inventory.md) lists what psx keeps, merges
and removes: thirteen commands (`context`, `bios`, `guide`, `read`, `disc`,
`assets`, `code`, `ram`, `run`, `states`, `scenario`, `port`, `message`,
and `mcp` to serve them). The emulator and its run options (watch, sample, trace),
disassembly and decompilation, coverage, the glossary and scenarios,
assets, the template and its developer MCP stay; mechanics (capture,
traces, replay, audits, interfaces) and cases go.

`psx web [ROOT…]` is the person's view of the ports, beside the thirteen: a
page on 127.0.0.1 (port 7878; `--port`, `--forget ROOT`), one tab per
project (the roots given are remembered). It shows what wants
the person across projects (an item that is theirs, the loop guard near
its limit, the same call failing again and again, passing recordings
fewer than their best, no work for an hour); `port next`'s item and
`port status`'s counts with their history from `.psx/progress.jsonl`;
a live log of every psx call (`.psx/calls.jsonl`, results in
`.psx/calls/`, written by the CLI and MCP alike in any project with a
`.psx`) grouped by the agent that made it, each drawn as a card for the
person (screens, plots, tables, code, the item handed out) with the raw
result folded away; the play requests, which the person plays in the
page on psx-core or in a window, or declines; a chat with the agent; and
each recording, replayed on psx-core into frames
with its quantities plotted over the ticks and, from the last `port
check`, the port's values where they differ and its first failure
marked. The template's report keeps the port's values for a recording
that failed for that.

Messages (`.psx/messages.jsonl`) never interrupt the agent: what the
person writes reaches it in the result of its next psx call, CLI or MCP,
under `messages`, and it answers with `psx message` (`--reply-to` for a
thread) when it has something to say. `psx message --wait` (CLI only)
blocks until the person writes and prints it, so an agent with nothing
else to do can run it in the background and be woken by the answer. `.psx/messages.seen.json` keeps how
far each side has read, for the page's unread count and its seen marks.

## 8. Decisions

1. A feature is done only when every branch of its code is resolved
   (covered or won't-reach), not at a coverage threshold.
2. Mechanics are removed from psx. The exact Rust already written for Croc
   stays in that workspace's folders; nothing further is planned for it.
3. The code is reviewed by someone who did not write it before a feature
   is done; its own author's self-review is not enough. The review is of
   the code as a whole and of its design, not of whether it matches.

The work to get there is in [recordings-plan.md](recordings-plan.md).

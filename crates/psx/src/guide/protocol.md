The porting protocol is the procedure for porting a PlayStation game so that
following it to the end gives a port that matches the original on every
recorded playthrough, names every part no playthrough reached, and is
written by people who never read the original's code. It is the
investigator's procedure; guide topic cleanroom gives the roles and the
barrier, and review the reviewer's part. psx port init sets up the three
repositories (guide topic barrier).

## What complete means

The original is finite, so completion is measured against inventories of
the disc, not a feature list:
- code: every function in the executable and its overlays, with its
  instruction count and basic blocks;
- formats: every file on the disc and every record type inside them;
- the corpus: recorded playthroughs, each pad input from a start state that
  rebuilds from power-on (guide topic states). Its checklist is fixed at the
  start: every level start to finish, every secret, every enemy alive and
  dead, every way to die, every menu page, save and load, game over, the
  attract demo, and a fast run of each level.

A port is complete when every corpus recording replays in it with matching
observations; every basic block the corpus executes is claimed by a
verified unit; every block it never executes is classified (dead, debug
only, unreachable on this disc, or needs a recording, which reopens the
corpus); and every format round-trips through the importer and the editor
byte for byte. That is the guarantee: faithful on everything recorded, and
an explicit list of everything that is not.

## Units, stages, the ledger

A unit is what moves through the stages: a code unit is one function,
weighted by its instructions (tiny helpers grouped with their caller); a
format unit is one record type, weighted by its fields times ten; a system
unit is a behaviour across functions with its own tests (the player's
movement, the camera, a menu, saving), listing the code units it covers.
Specs and cases are written per system unit; progress is counted per code
and format unit.

Each stage has an exit check, so progress is never a judgement call:
0 discovered (in the inventory); 1 exercised (a corpus recording executes
it, or it is classified); 2 specified (an approved spec covers it);
3 evidenced (its cases exist and check against the original, case check);
4 implemented (port code cites the spec); 5 verified (its cases pass in the
port); 6 integrated (every corpus recording reaching it matches; a format
round-trips in the editor). A unit whose tests start failing drops back.
Credits per stage 0.05, 0.25, 0.40, 0.65, 0.85, 1.0 from stage 1; complete
is the weighted mean of the credits, for code, formats and the whole. An
estimated date only means something once the inventory stops growing: while
it grew more than 5% in the last four weeks, give none.

## The inventory

inventory takes the code and format inventories' starting point: every
function of every PS-X EXE on the disc, with its instructions and basic
blocks, and the disc's file list, written to notes/inventory.json (in a
repository with protocol.toml; out sets another place), with each run's
totals appended to inventory_history.jsonl beside it, so its growth shows.

  inventory --ram RUN --ram RUN2 ...

Functions are found from each executable's entry, the calls in functions
already found, code that starts after a return or with a stack prologue,
and pointers into code: the handlers a table dispatches to, which analysis
from the entry alone misses. Give ram run directories from the places the
game goes (a level, a menu, a cutscene): a snapshot holding an executable
adds the tables it fills at runtime, and code it holds that no executable
does is an overlay, matched to the disc file it was loaded from and the
part of it RAM still holds. A traced run (run --trace) adds its call
targets, calls through pointers included, which is what finds overlays
reached only through pointers. Each executable and overlay lists its gaps,
what no function covers, with how many words decode as code: classify
them, or record the game reaching them. Take an inventory again whenever
the corpus reaches somewhere new.

This psx does not compute the ledger yet. Keep the units, their stages and
the specs covering them in notes/LOOP.md, and run --trace's coverage.txt for
what the corpus executes.

## The loop

1. Inventory: inventory, with snapshots of the places the game goes;
   format tables for the disc's files.
2. Corpus: record playthroughs against the checklist (handoff for people),
   kept as input plus a named start state.
3. Per system unit, bottom-up (formats, then what everything shares: the
   order of a tick, timing, collision; then the player; then the rest):
   find the behaviour (below), record cases (guide topic cases), write the
   spec, publish stage; the reviewer approves; publish cross. The
   implementer builds it until its cases pass, and its questions come back
   as spec changes and new cases.
4. A divergence found replaying the corpus goes back as a question in game
   terms ("lands two units further on row 812 of the long-jump case"),
   never as code.

## Making cases that separate

A case is evidence for a rule only if a different rule would fail it.
Defaults:
- Start a case away from anything not specified yet. A run that crosses a
  step, a slope or a trigger no spec covers cannot pass until that spec
  exists; move the start state instead.
- Before calling a case plain ("open ground"), check it touches nothing
  else: sample what would show a push, a collision or a trigger, and look
  for nearby geometry in the level's data. A wall a short way off can turn
  one run of an otherwise plain set.
- Vary input on odd fields as well as even ones, and taps of one field. A
  game that ticks every other field reads the pad on only some fields;
  cases that change input only on even fields hide which.
- For each rule, record a case on each side of its boundary (just short of
  a threshold and just past it), and one that holds two inputs together.
- Record calibration cases while working a rule out, then heldout cases
  afterwards, made without looking at the port; keep some back
  ("kept_back": true) for the reviewer.
- Give each case coverage (guide topic cases): which spec claims which
  columns and rows, so the port compares what it has built and knows what
  the rest waits for.

## Finding things in RAM

- The player's object: search 32-bit values over a range for its position
  while it moves one way (ram_search width=4 with between and increased),
  then narrow with more snapshots. A narrow field (a 16-bit heading) gives
  thousands of candidates; a wide one moving steadily gives a few. The
  object's other fields are near it: ram_view the struct across snapshots,
  then name it in the field map (guide topic fields).
- The tick: run find_tick (guide topic fields).
- Input: the console's pad reaches the game through its own input word.
  Hold one button and diff against holding none for the word; a write
  watch on it gives the one routine that writes it, and xrefs on it the
  readers, which are where the game's behaviour starts.
- Arrays of objects: one element found, the others are a stride apart;
  xrefs on the first element's address often finds the table's base.

## Play-throughs

When a state is out of a script's reach, ask a person through handoff.
Describe it in game terms, by landmarks and actions ("in the first level,
stand at the top of the stairs facing the door, then save"), never in
rooms, coordinates or memory. Give it expect conditions (run's until form),
and when it returns check they hold and the screenshot shows what was asked;
ask again with what was wrong if not. Name the state (states save) so it
rebuilds from power-on.

## When the emulator changes

The emulator is the instrument evidence is measured with. After a psx
update: states rebuild to replay every start state's chain from power-on; case check
to record every case again (write=true replaces those that differ, after
you understand why); the port's tests then run against the new cases, and a
difference is an instrument change until triaged, not a port regression.
Play-throughs that no longer reach their goal are asked for again.

## The port's own tools

Every port needs tools that only make sense for its game; build them in the
project's tools/ without asking. A reader for the game's level and data
formats usually comes first: finding walls and steps near a case's start,
listing the animations an object has. psx provides what every PlayStation
game shares (the console, the emulator, the disc, recordings, the barrier)
and stops there.

## Reporting what was missing

Each role's log asks what was missing. For each tool or piece of guidance
you wished you had, ask: would it help someone port a different
PlayStation game, one you know nothing about? If yes it is a gap in psx:
report what was missing in general terms ("I could not tell why a run's
ticks were uneven"), not a feature ("add a tick counter"), so psx can
choose the general fix, which may be a paragraph here. If no, build it in
tools/ and list it as a project tool: a helper for the nearest wall in this
game's levels is the second kind.

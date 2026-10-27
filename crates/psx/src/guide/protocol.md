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
round-trips in the editor). A unit whose tests start failing drops back,
and so does one whose cases were not checked on the current emulator.
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

Functions are found from each executable's entry, calls, code after a
return or with a stack prologue, and pointers into code (the handlers a
table dispatches to). Give ram run directories from the places the game
goes (a level, a menu, a cutscene): a snapshot adds the tables filled at
runtime, and code no executable holds is an overlay, matched to the disc
file it came from. A traced run (run --trace) adds its call targets and
the code it executed, where the code at hand is what ran there, and its
code pages (code.bin, RAM as it was when code in it ran): code loaded and
replaced between frames, which no snapshot holds and no disc file
matches, is found as it ran. Code the BIOS copies into RAM at power-on (its shell) is
listed apart under bios_code. An image at an overlay's place whose code
words are the overlay's (only its data differs) is one of its variants,
counted once; one whose code differs notes same_range_as, with how many
words and code words differ. Each executable and overlay lists its gaps,
what no function covers, with how many words decode as code: classify
them, or record the game reaching them. Take an inventory again whenever
the corpus reaches somewhere new.

## Coverage

coverage merges what traced runs executed into the inventory: per
function, its blocks executed and not, how often it was called and which
runs reached it, written to coverage.json beside the inventory.

  coverage RUN RUN2 ...

A unit is exercised when a corpus run executes one of its blocks; the
unexecuted blocks of exercised functions, and the functions no run
reached, are the list to classify (dead, debug only, unreachable on this
disc) or to record. Each executed word is credited to the executable or
overlay whose code ran there, so code loaded over other code is told
apart. Two lists say the inventory is short: executed_outside_functions
(code the analysis missed) and in_no_image (an overlay replaced before the
run's end, often compressed on the disc). Take the inventory again with
the corpus runs as ram: their code pages hold what ran (a run recorded
by a psx before code.bin, record again). What is left once the inventory
has read every corpus run is code it cannot place: the ledger's
inventory_gaps says so, and another pass will not change it.

coverage keeps each run's recipe in coverage.json: the named state it
started from (by the sha256 its report notes), its arguments with the
trace window, and its executed.bin's sha256. The run directories can go;
the recipes are the corpus.

  coverage --check

runs every recipe again (into .psx/corpus-check/) and compares what each
executed: same, differs (this psx ran other code: coverage over the runs
again takes the corpus anew), state_changed (states rebuild first) or
no_recipe (a run from a state no chain names, which states save names, or
from an older psx: record it again).

## The ledger and next

Every session, of every role, starts with protocol next:

  protocol next --role investigator

It computes the ledger from the repositories' files, writes it to
notes/ledger.json beside the inventory (its totals to ledger_history.jsonl
when they change), writes the implementer's notes/NEXT.md (spec ids,
titles and crossed case names only), and gives the role's work in order,
now first. Nothing in it is kept by hand; each exit check is a file:
- exercised: coverage executes the function, or notes/classified.json
  classifies it ({"TOMB.EXE 80012345": "dead"}: dead, debug or
  unreachable; a classified unit needs nothing more and counts complete);
- the specs covering a function: notes/provenance.json, routines keyed by
  an address in it (with the executable's or file's name beside the
  address where two hold code there);
- specified: the spec crossed (publishing log); evidenced: every case it
  names or that claims it exists and was recorded or case checked on this
  emulator and held; implemented: the implementer's code cites its id;
  verified: each crossed case claiming it has a passing verdict since it
  last changed (psx-bevy's write_run writes NAME.result.json in the
  target's tmp folder; protocol.toml's [protocol] results moves it);
  integrated: a corpus case (role corpus) passes, or for a format a
  round_trip verdict (write_result);
- heldout: clean mode's last verdict on each case kept back for the spec
  (notes/oracle.jsonl; a changed case counts as not run); no stage needs it;
- formats: a format spec's files (patterns) and records ({name: fields})
  in its front matter; disc files no format spec names are listed.
The work it gives: rebuilding start states and checking cases again when
the emulator changed (until then they are not evidence); waiting while an
audit is due (every few implemented specs, [protocol] audit_every, and at
a loop's end, until audit records one: guide topic review); the findings
of a failed audit; revising a rejected bundle and crossing an approved one;
taking the inventory and coverage again; answering questions and choices
(specs/answers/Q-NNNN.md, a choice under its number in
specs/answers/CHOICES.md; answered once crossed); thin specs (a
verified spec whose kept-back cases fail); recording missing cases; staging; then a few units to specify, formats first, unless
[protocol] specs_ahead crossed specs wait for the implementer; and
classifying what no run reaches.

## The report

  protocol report --plain

The ledger for the person: complete by kind, and an estimated date with
its range (from the last four weeks of the ledger's history), or why there
is none (the inventory still growing, or too little change). plain adds
what moved since the person's last plain report and what waits on them (a
play-through, a session to start); without it, JSON with the stages.

## The loop

1. Inventory: inventory, with snapshots of the places the game goes;
   format tables for the disc's files.
2. Corpus: record playthroughs against the checklist (handoff for people),
   kept as input plus a named start state.
3. Per system unit, bottom-up (formats, then what everything shares: the
   order of a tick, timing, collision; then the player; then the rest):
   find the behaviour (guide topic fields), record cases (guide topic cases), write the
   spec, publish stage; the reviewer approves; publish cross. The
   implementer builds it until its cases pass, checking itself against
   the original in clean mode as it goes, and its questions come back as
   spec changes and new cases.
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
  ("kept_back": true): the reviewer checks with them, and clean mode's
  heldout runs the port on them without showing them (guide topic
  cleanroom). Give every name a case uses a glossary entry and publish
  the start states, so the implementer can record from them too.
- Give each case coverage (guide topic cases): which spec claims which
  columns and rows, so the port compares what it has built and knows what
  the rest waits for.

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
update: states rebuild, then case check (write=true replaces cases that
differ, once you understand why); a difference is an instrument change
until triaged, not a port regression. Play-throughs that no longer reach
their goal are asked for again.

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

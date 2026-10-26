A start state is where recordings begin: a case's row 0, a handoff's start,
a probe's starting point. A state file holds the console's whole memory (the
game's own code and data), so it stays out of git and can be lost or go
stale when psx changes. Keep each one with its chain instead: every run from
power-on that led to it, small enough to commit.

- .psx/states/NAME.bin: the state (run from_state takes it).
- notes/states/NAME.json: the chain: a description in game terms, the media
  and BIOS identity, and the steps, each one run's frames, input, pokes,
  until conditions (and their field map), the pad and cards at power-on,
  which state it ended in (its end or a checkpoint) and that state's sha256
  and psx build.

states make NAME runs one step and names the state it ends in. from is a
named state, a state file, or power-on when left out; input, replay, until,
map and poke are run's; pad and memcards only from power-on:

  states make title --frames 1500 --description "the title screen"
  states make level1 --from title --frames 900 \
      --input '[{"frame":10,"buttons":["START"],"until":14}]' \
      --until lara.room=16 --description "Lara stands at the level start"

states save SOURCE NAME names a state an earlier run or handoff made
(state.bin, a checkpoint state_fN.bin, a handoff mark's state.bin, or the
run directory). Its chain comes from the runs' reports, back to power-on or
to a state a chain already names (by sha256), so a run from a named state,
or a handoff from one, saves with the whole chain. A state with no report
and no chain cannot be saved: make it again with states make.

states list gives each named state, its description, its steps and whether
the file is present and matches the chain.

states rebuild [NAME...] replays chains from power-on on this psx (all of
them by default). Steps that chains share run once, and runs are kept in
.psx/rebuild/BUILD/ for the next rebuild. For each state it reports:

- first_differing_step: the first step whose state differs from the sha256
  the chain notes (null when every step reproduced);
- status: same, differs (the rebuilt state is not the file), or restored (the
  file was missing and is back);
- compare, when they differ: values old and new for sample names (field map
  names, as run's), and how much of main RAM differs. An older psx's state
  may be unreadable here: then compare by cases.

After a psx update: states rebuild, then replace=true puts the rebuilt states
in place (the old ones move to .psx/old/BUILD/states/) and notes the new
sha256 in the chains; then case check, whose differences are the triage
list: an instrument change (a state that reached the same place by another
field) or a real divergence (the game played differently). Each chain
step notes the emulator (the core's own source, not all of psx) it was
made on, updated when a rebuild matches: protocol next asks for a rebuild
only when the emulator changed.

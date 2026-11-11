psx reads a PlayStation game: it runs the disc on its own emulator core and
measures what the game does, reads its code and decodes its data. A port is
the game, or a part of it, rewritten as an ordinary, modern program and checked
against recordings of the original. This guide is the whole method; the
topics run, debug, code, assets, setup and arithmetic hold the details of
the commands.

Each command is an MCP tool named command_subcommand. Addresses, PCs and registers are hex strings (0x80012345); a
data address that could be anywhere has its region beside it (main_ram,
scratchpad, io, bios). Large results are files in a run_dir: read them with
read (JSON by RFC 6901 pointer, JSONL and CSV by rows and columns, hex, PNG
as an image), and page with next_cursor. A capture proves what was
recorded, not that the game reached gameplay or that a hypothesis holds.

## A port

psx port init DEST --media GAME.cue writes the workspace: a template in Rust
(game/, a psx_scenario Port with no engine; tools/import/, glossary/,
vendor/), which the port may use or replace (psx-port.toml names its
commands; topic setup), goal.toml, STATUS.md, catalogue/, formats/, the
disc and BIOS linked in, .mcp.json for psx, and git. Pass fields_per_tick from the original's main
loop; the region comes from the disc. Its AGENTS.md says what the port's
code is held to; this guide says how the work goes.

In a workspace, port_check is the test: each recording passes, or gives its
first difference (the original's value and the port's there) and the code
that wrote it; scenario_show port=true puts the port's values beside the
original's. port_status is information.
What to do next is the agent's to choose, saying what and why, and the
person's to redirect. A session takes one difference or piece and stops;
the next starts from STATUS.md in the workspace, which says what is ported
and checked, what is ported but not checked, what isn't ported and what's
next.
The person decides when it's done, by playing it. Without a workspace the
same commands investigate a game: start with context.

## The loop

1. The person says what they want; the agent writes it as the goal.
2. Record the original in scenarios showing what the goal describes:
   play-throughs by the person, and short scripted runs of each thing
   they show.
3. Name what to compare in the glossary, and where the original keeps it.
4. Find the code the recordings run: code_coverage_diff of recordings
   doing the thing against ones not doing it, watches on the values
   involved, disassembly and decompilation.
5. Implement, reading the original's code.
6. Compare the port with every recording, tick by tick (port_check).

Rules:
- Read, don't fit. Constants come from the code. A constant tuned until a
  curve matches passes the recordings it was tuned on and fails the next.
  When only a recording shows a rule, the citation says so.
- Cite the address, the instruction or the recording beside each constant
  and rule, and what was not verified.
- A long play is the map, not the work. When its first difference shows
  something not yet ported, record a few short recordings of that thing,
  starting near it (from=PLAY@TICK) and varying how it happens, and port
  it until they pass; then see the play get further. A rule built from one
  moment of a long play rarely holds at the next.
- Choose recordings by what they show. A branch the recordings take only
  one way (port_status branches=true) is a case none of them has shown:
  a short recording that takes it the other way checks the rule there.
  One no recording can reach goes in STATUS.md, with why.
- Hold recordings out. A play in goal.toml's held_out is judged by
  port_check and counted by port_status, never shown: psx refuses the
  agent's calls that name it. Ask for one (scenario_play held_out=true)
  on a route the work recordings don't take, and again when a piece
  works. The person's own plays are held out unless they give them to the
  agent; one the agent asked for without held_out is a work recording,
  and the person can move any recording either way in psx web.
- A recording that differs is not a distance to close: the rule for
  those quantities is not understood yet, or other code it runs is not
  ported. port_check names them, not where or by how much, and points
  at the code that writes them (topic code); understand that code.
- The world a recording needs: a scenario in a stand-in world (a flat
  floor) records only what fits it; the original's levels are code and
  assets to port like any other.
- The disc and psx are the only sources.

## The goal

goal.toml holds the person's description, the disc's assets to extract
(true, every file; false; or asset names, see Assets), review_lines and
held_out (plays held out from the agent, scenario/name). Every other recording under
scenarios/ is a work recording, and a scenario's toml names the assets
its recordings need (assets = [...]).

port_status lists the functions the work recordings ran that nothing in
game/ or tools/ cites by address; catalogue/not_ported.toml (entry or range
= why) takes out the ones the port won't follow, such as libraries and
drivers.

## Recordings

The glossary (glossary/src/lib.rs) names the quantities a scenario
compares, in game terms: what a player could notice (where things are,
what's drawn, what the game shows or plays), never the original's counters,
timers, flags or script variables, which the port may handle its own way:

  pub struct Quantities {
      /// The player's position across the level
      /// Unit: world units
      pub player_x: i32,
      /// The player stands on the ground; false through a jump
      pub player_on_ground: bool,
  }

Fields are bool, integers, fixed-point aliases, enums or floats, each with a
doc comment and a Unit: line; the crate has no dependencies, macros,
generics or unsafe code. Its binding, glossary/binding/ (a cdylib psx builds
and loads), knows the PlayStation:

  impl Glossary for Game {
      type Glue = glue::Glossary;
      const TITLE: &str = "A Game (Europe) SLES-01234";
      const IMAGE_SHA256: &str = "…";   // context's image_sha256
      /// The logic step: the main loop's accumulator -= 256.
      fn tick(&self, t: &mut Tick) { t.store(0x800A_1C30, 4).by(0x8001_52A4); }
      fn quantities(&self, m: &Machine) -> Quantities {
          let player = m.u32(0x800A_2F18);
          Quantities { player_x: m.i32(player + 0x24), player_on_ground: m.u32(player + 0x60) & 1 != 0 }
      }
  }
  psx_capture::export_glossary!(Game);

The tick is the store that ends one step of the game's logic; the
quantities are read right after it, anew each tick (pointers and list walks
are ordinary code; only main RAM and the scratchpad can be read). Bind the
logic step, not the render loop: many games draw every field and think
every second one, or draw as fast as they can. run with find_tick finds it
(topic run).

A scenario, scenarios/<id>.toml, is a moment both the original and the port
can reach:

  moment = "the player at the start of 1-1, the first tick they can move"
  chain = "notes/states/level_1_1.json"   # how the original gets there
  kind = "level"                          # the port's launcher
  idle = 60                               # the determinism check's idle fields
  [compare]                               # quantity = tolerance, raw units
  player_x = 0
  [parameters]                            # how the port gets there
  level = "1-1"

The chain is a named start state's (topic setup). End its last run at a
game moment, with until on the game's own state: disc reads vary, so a
count of fields after a press lands a field or two off.

scenario_record id name frames input|replay [description, from] starts from
the scenario's state, runs the first tick with the pad released (the
recording's initial values, the moment the port's launcher builds), then
records frames more fields with the pad as given, counted from the field
after that tick: scenarios/<id>/<name>.jsonl, one line per tick with the
field it ended in, the pad, its mode and sticks when they changed and
the quantities that changed. The sticks are the raw bytes the game read
(128 at rest), so they need pad dualshock in the chain and analog mode
(the game's switch, or ANALOG) before they move; the port reads them as
Pad::sticks and Pad::analog.
Recordings are latched, steady and fixed unless stock (topic run).
Beside it, <name>.coverage.json keeps what the
original ran as the inventory's code (no instructions, so it is committed):
take code_inventory first, and unpack compressed overlays first (topic
code). from=NAME starts at the end of another recording of the scenario,
to split a long route; from=NAME@TICK starts after its tick TICK, to
record near a moment of a play. Either way the first tick runs with the
pad released, and the port reaches the start by running those ticks
first. A held-out play is never a start. scenario_show reads a recording
per tick, with each numeric quantity's change since the tick before.

scenario_check is the determinism check: it reaches each scenario again
with idle fields added before the chain's last run and records again. A
different value fails (state the parameters do not hold, often a chain
ending on a count of fields or a tick on the render loop); a different
schedule is only a note. A recording made before the scenario's compare or
a quantity's type changed is made again.

scenario_play id name request asks the person to play from the
scenario's state after its first tick (or where from starts). It queues;
port_status lists it once played, recorded as scenario_record does.
Write the request in game terms: the spot, the way to face, what should
have happened. held_out=true makes the play held out: its answer says
only that it was recorded. Without a scenario, a play reaches a hard place for
investigation (topic run).

In the port, Game launches each scenario kind and observes the quantities
it keeps (AGENTS.md). port_check builds and runs the port by its
psx-port.toml, replays each recording through it and writes the verdicts.

## Status and reviews

port_status is information and decides nothing: the recordings passing, the
quantities they compare (compared), how far they reach (reach: starting
points and the longest), the functions they ran and how many the port cites or lists as not ported,
branches taken both ways (branches=true lists the open ones), assets,
problems in goal.toml or the catalogue, plays waiting or answered
(scenario_play status reads an answer), debug files on the disc (code_symbols
reads them), and what stops a recording being judged (unrecorded, unbuilt,
late_input, unpack).

port_status screens=true draws each work recording on both sides at up to
32 moments and scores how far the port's picture is from the original's (0
the same, 100 black against white), with the worst tick of each;
scenario_show screens=true over that tick shows it. It takes minutes, and
without it port_status repeats the last scores. It is information only: what
isn't drawn yet always counts, so read the trend.

Once review_lines lines of game/ and tools/ changed since the last review
(goal.toml, 800 by default), or that many with none yet, port_check says a
review is due and port_status gives review_due: why, and the brief for a
reviewer who did not write the code. Nothing waits on it. It asks whether one
would write a new game like this, and whether the recordings show all the
description asks; the reviewer also files a report for the person alone
(report). review.md: TOML front matter (commit = the commit reviewed) between
+++ lines, then one ## heading per finding, each answered with an
**Answer:** line: done, with port_check passing again, or kept and why.

Verdicts are psx's (.psx/verdicts/, never edited). A verdict counts only
for the commit it judged with no uncommitted changes, so commit, then
port_check (optionally one scenario).

## Assets

Each disc file but the code is unknown, format found, imported, checked or
unused (topic assets has each state, and archives). Asset names pick what
is extracted (topic assets): goal.toml's for the person's choice, a
scenario's for what its recordings need. Only the importers read the
original's formats; the game reads the port's own.

## Commands

- context: the build, project root, media, BIOS and capabilities (setup).
- bios_install: a local BIOS, or a pinned download for a region (setup).
- guide: this text, or a topic: run, debug, code, assets, setup,
  arithmetic.
- read: a returned file, by pointer, rows, hex or image.
- disc_info, disc_ls, disc_extract, disc_sector, disc_exe: the disc's
  tracks, files, sectors and executables (assets).
- run: the game for N fields from power-on or a state, with input,
  watches, samples, traces, GPU captures and states (run).
- debug_start, debug_go, debug_show, debug_set, debug_back, debug_save,
  debug_end: a run that stops at a PC, a write or the tick, to look,
  change and go on or back (debug).
- ram_diff, ram_search, ram_view: values across snapshots (code).
- code_disasm, code_xrefs, code_decompile, code_functions, code_symbols:
  reading code (code).
- code_inventory, code_coverage, code_coverage_diff: every function, and
  what runs and recordings executed (code).
- assets_scan, assets_extract, assets_textures, assets_diff,
  assets_memcard: standard formats, VRAM and memory cards (assets).
- states_save, states_list, states_rebuild: named start states and their
  chains (setup).
- scenario_record, scenario_check, scenario_show, scenario_play:
  recordings.
- port_init, port_update, port_add, port_status, port_check:
  the port (port_update, port_add: setup). port_vectors: arithmetic test vectors (arithmetic).
- message: talk with the person (run).

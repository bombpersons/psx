psx reads a PlayStation game: it runs the disc on its own emulator core and
measures what the game does, reads its code and decodes its data. A port is
the game, or a part of it, rewritten as an ordinary, modern program and checked
against recordings of the original. This guide is the whole method; the
topics run, code, assets and setup hold the details of the commands.

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
commands; topic setup), goal.toml, catalogue/, formats/, the disc and BIOS
linked in, .mcp.json for psx, and git. Pass fields_per_tick from the original's main
loop; the region comes from the disc. Its AGENTS.md says what the port's
code is held to; this guide says how the work goes.

In a workspace, start each session with port_next: one item, why it is
due, done_when (the fact that will show it done) and how. Do it until
done_when holds, commit, saying what changed (stomp hang), and call
port_next again. Confirm, Wait and Stuck wait for the person: say what
they need and stop. port_status shows where the goal stands; plain=true
is text for the person. Without a workspace the same commands
investigate a game: start with context.

## The loop

1. The person says what they want; the agent writes it as the goal and
   the person confirms it.
2. Record the original in scenarios showing what the goal describes:
   scripted runs, and play-throughs by the person.
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
- Hold recordings out. The person's plays (goal.toml's held_out) are
  judged only once the code is reviewed, pass or fail; never work on
  them or tune to them.
- A recording that differs is not a distance to close: the rule for
  those quantities is not understood yet, or other code it runs is not
  ported. port_check names them, not where or by how much, and points
  at the code that writes them (topic code); understand that code.
- The world a recording needs: a scenario in a stand-in world (a flat
  floor) records only what fits it; the original's levels are code and
  assets to port like any other.
- The disc and psx are the only sources.

## The goal and the catalogue

goal.toml holds the person's description, the disc's assets to extract
(true, every file; false; or asset names, see Assets), review_lines and
held_out (the person's plays, scenario/name). Every other recording under
scenarios/ is a work recording, and a scenario's toml names the assets
its recordings need (assets = [...]).

The code to port is what the work recordings run, less
catalogue/not_ported.toml (entry or range = why: libraries, drivers,
code the port has its own of). Cite each function you port by address in
game/ or tools/; a function cited nowhere is work. catalogue/ also holds
wont_reach.toml (branch address, script position or range
0xSTART..0xEND, end excluded = why no recording can reach it) and
questions.md (one "- " line per open question); catalogue/README.md has
the formats.

The person reviews the goal with psx port confirm and confirms it with
--yes, the wont_reach branches and their reasons included; the
description, an asset name taken out, a play dropped or a wont_reach
changed needs confirming again.

## Recordings

The glossary (glossary/src/lib.rs) names the quantities a scenario
compares, in game terms:

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
code). from starts at the end of another recording of the scenario, to
split a long route. scenario_show reads a recording per tick, with each
numeric quantity's change since the tick before.

scenario_check is the determinism check: it reaches each scenario again
with idle fields added before the chain's last run and records again. A
different value fails (state the parameters do not hold, often a chain
ending on a count of fields or a tick on the render loop); a different
schedule is only a note. A recording made before the scenario's compare or
a quantity's type changed is made again.

scenario_play id name request asks the person to play from the
scenario's state after its first tick (or the end of from). It queues;
port_next gives a Read item once played, recorded as scenario_record does.
Write the request in game terms: the spot, the way to face, what should
have happened. Without a scenario, a play reaches a hard place for
investigation (topic run).

In the port, Game launches each scenario kind and observes the quantities
it keeps (AGENTS.md). port_check builds and runs the port by its
psx-port.toml, replays each recording through it and writes the verdicts.

## Status and next

The goal's state, each holding with those before it:

- matching: every work recording passes at HEAD.
- finished: every branch of the code to port taken both ways or a
  confirmed wont_reach, no question open, no function cited nowhere.
- reviewed: the review holds and the assets are settled.
- complete: the goal confirmed, the person's plays pass and nothing left
  turned off.

port_status gives three shares: recordings passing, code cited or not
ported, branches resolved.

port_next's items, in this order:

- Read: the person answered a play request; debug files to read.
- Fix: C in decomp/ that passed its code check fails now.
- Catalogue: goal.toml or the catalogue has problems; fix them.
- Confirm (person): the goal is unconfirmed or changed since; until it
  is, only investigate and record.
- Review: findings without an answer, a review that does not read, or
  review_lines lines of the code (game/, tools/) changed since the last
  (goal.toml, 800 by default).
- Format, Index, Import, Check: a scenario's assets, before its work.
- Build: the port has no launcher for a scenario kind, or does not
  observe a compared quantity.
- Unpack: overlays the inventory knows only from code that ran in them.
- Record: a recording not ready, or one made before the pad latched.
- Implement: no port_check has judged HEAD.
- Fix: a recording fails (the one worked on, else the earliest to
  differ), with its quantities and the pointer; once the port draws,
  compare the screens (scenario_show screens=true).
- Restore: a system turned off (a scenario's [off]) that the port's code
  now cites.
- Answer: an open question.
- Locate: functions the work recordings run that are cited nowhere and
  not in not_ported.toml.
- Cover: a branch never taken one way, or script never read or a
  decision's way never taken: record it, ask the person to play it, or
  list it in wont_reach.toml.
- Confirm (person): wont_reach branches not confirmed as they stand.
- Format, Index, Import, Check (the goal's assets): see Assets.
- Review: the goal matching or finished since the last review, and no
  review holds for the code as it is. It asks whether one would write a
  new game like this, and whether the recordings show all the
  description asks. how is the brief for a reviewer who did not write
  it, who also files a report for the person alone (report). review.md:
  TOML front matter (commit, matching, finished) between +++ lines, then
  one ## heading per finding, each answered with an **Answer:** line:
  done, with port_check passing again, or kept and why.
- Play (person): the goal reviewed and no play of the person's held out:
  start scenario_play and name it under held_out. A play that fails
  gives Implement or Fix for the code it runs.
- Restore: systems still turned off, once the plays pass.
- Done: the goal complete.
- Wait (person): only unanswered play requests are left.
- Stuck (person): the same item three times with nothing changed.

Verdicts are psx's (.psx/verdicts/, never edited). A verdict counts only
for the commit it judged with no uncommitted changes, so commit, then
port_check (optionally one scenario).

## Assets

Each disc file but the code is unknown, format found, imported, checked or
unused (topic assets has each state, and archives). Asset names pick what
is extracted (topic assets): goal.toml's for the person's choice, a
scenario's for what its recordings need, which port_next gives before
their work: Format, Index, Import, Check. Only the importers read the
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
- port_init, port_update, port_add, port_status, port_next, port_check:
  the port (port_update, port_add: setup). psx port confirm is the
  person's. port_vectors: arithmetic test vectors (arithmetic).
- message: talk with the person (run).

psx reads a PlayStation game: it runs the disc on its own emulator core and
measures what the game does, reads its code and decodes its data. A port is
the game, or a part of it, rewritten as ordinary Rust and Bevy and checked
against recordings of the original. This guide is the whole method; the
topics run, code, assets and setup hold the details of the commands.

Each command is an MCP tool named command_subcommand (code_disasm is psx
code disasm). Addresses, PCs and registers are hex strings (0x80012345); a
data address that could be anywhere has its region beside it (main_ram,
scratchpad, io, bios). Large results are files in a run_dir: read them with
read (JSON by RFC 6901 pointer, JSONL and CSV by rows and columns, hex, PNG
as an image), and page with next_cursor. A capture proves what was
recorded, not that the game reached gameplay or that a hypothesis holds.

## A port

psx port init DEST --media GAME.cue writes the workspace: the Bevy
template (psx-bevy/, game/, tools/import/, vendor/), glossary/, goal.toml,
catalogue/, formats/, the disc and BIOS linked in, .mcp.json for psx, and
git. Pass fields_per_tick from the original's main loop; the region comes
from the disc. Its AGENTS.md and game/AGENTS.md say what the port's code is
held to; this guide says how the work goes.

In a workspace, start each session with port_next: one item, why it is
due, done_when (the fact that will show it done) and how. Do it until
done_when holds, commit, naming the feature's id (F-0003: stomp hang), and
call port_next again. An item for the person (Confirm, Play, Stuck) waits
for them: say what it needs and stop. port_status shows every feature by
stage; plain=true is text for the person. Without a workspace the same
commands investigate a game: start with context.

## The loop

1. The person says what they want; the agent proposes the features it
   implies and the person confirms the list.
2. Find each feature's code: code_coverage_diff of recordings doing the
   thing against ones not doing it, watches on the values involved,
   disassembly and decompilation.
3. Name what to compare in the glossary, and where the original keeps it.
4. Record the original in scenarios: scripted runs, and play-throughs by
   the person.
5. Implement, reading the original's code.
6. Compare the port with every recording, tick by tick (port_check).

Rules:
- Read, don't fit. Constants come from the code. A constant tuned until a
  curve matches passes the recordings it was tuned on and fails the next.
  When only a recording shows a rule, the citation says so.
- Cite the address, the instruction or the recording beside each constant
  and rule, and what was not verified.
- Hold recordings out. A feature's held_out recordings are judged only
  once it is reviewed; the person's play-throughs are the best ones. Their
  results stay hidden until then: don't tune to them.
- The first tick that differs is where to look; the end state only
  summarises.
- The world a recording needs: a feature tested alone runs in a stand-in
  world (a flat floor), and only recordings that fit it count for it. The
  original's levels are a feature of their own.
- The disc and psx are the only sources.

## The goal and the catalogue

goal.toml names the features the person wants (ids, or "all"), a
description, and assets = true when the goal includes the disc's assets.
catalogue/F-NNNN.md is one feature: TOML between +++ lines, then a
paragraph in game terms (catalogue/README.md has the format and how fine to
cut):

  id, title, area
  depends     features it cannot work without
  code        the original's functions, by entry address (the inventory's)
  questions   what is not known yet
  [recordings] work and held_out, as scenario/name
  [wont_reach] branch address = why no recording can reach it

The goal's features and their dependencies are its closure. The person
reviews it with psx port confirm and confirms with psx port confirm --yes,
which also confirms the wont_reach branches with their reasons. The list may
grow afterwards; taking a feature out, or a wont_reach added or changed,
needs confirming again.

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
field it ended in, the pad when it changed and the quantities that changed.
Digital buttons only. Beside it, <name>.coverage.json keeps what the
original ran as the inventory's code (no instructions, so it is committed):
take code_inventory first, and unpack compressed overlays first (topic
code). from starts at the end of another recording of the scenario, to
split a long route. scenario_show reads a recording per tick, with each
numeric quantity's change since the tick before (a walk's speed, a jump's
gravity).

scenario_check is the determinism check: it reaches each scenario again
with idle fields added before the chain's last run and records again. A
different value fails (state the parameters do not hold, often a chain
ending on a count of fields or a tick on the render loop); a different
schedule is only a note. A recording made before the scenario's compare or
a quantity's type changed is made again.

scenario_play id name request opens a window for the person, at the
scenario's state after its first tick (or at the end of from), with the
request beside the game. On F12 psx records their input as scenario_record
does and says whether it ended where the play did; over MCP it returns a
ticket at once: poll with status and wait until it is no longer waiting.
Write the request in game terms: the spot, the way to face, what should
have happened. Without a scenario, a play reaches a hard place for
investigation (topic run).

In the port, each feature's plugin registers a launcher per scenario kind
and observes the quantities it keeps (AGENTS.md, The code); game/tests/
scenarios.rs replays each recording tick by tick with its pad. port_check
runs it and writes the verdicts; cargo test -p <game> --test scenarios gives
the same result while working.

## Status and next

A feature's stages, each holding with those before it:

- catalogued: it has an entry.
- located: its entry names its code.
- recorded: work recordings, their scenarios deterministic, their coverage
  counted against the inventory.
- matching: every work recording passes at HEAD.
- reviewed: every branch of its code taken both ways by its recordings or
  a confirmed wont_reach, no question open, and a review that holds.
- done: its held-out recordings pass.

port_next's items, in this order:

- Catalogue: goal.toml or the catalogue has problems; fix them.
- Confirm (person): the list is unconfirmed or lost a feature; until it is,
  only investigate and catalogue.
- Build: the port has no launcher for a scenario kind, or does not observe
  a compared quantity.
- Unpack: overlays the inventory knows only from code that ran in them.
- Locate: the entry names no code.
- Record: its recordings are not ready.
- Implement: no port_check has judged it at HEAD.
- Fix: a recording fails; the earliest divergence first.
- Answer: an open question.
- Cover: a branch of its code its recordings never took one way: record
  what reaches it, ask the person to play it, or list it under wont_reach.
- Confirm (person): wont_reach branches not confirmed as they stand.
- Review: its work is finished without a review that holds. how is the
  brief for someone who did not write it (a sub-agent or a fresh session).
  Findings go in reviews/F-NNNN.md: TOML front matter (commit, files)
  between +++ lines, then one ## heading per finding, each answered with an
  **Answer:** line. It holds while its files are unchanged since its
  commit.
- Play (person): a reviewed feature without held-out recordings: start
  scenario_play and name the recording under held_out.
- Format, Index, Import, Check (assets = true): see Assets.
- Stuck (person): the same item three times with nothing changed.
- Done: every feature of the goal is done.

Verdicts are psx's (.psx/verdicts/, never edited). A verdict counts only
for the commit it judged with no uncommitted changes, so commit, then
port_check (optionally one feature).

## Assets

Each disc file but the code is:
- unknown;
- format found: a formats/<id>.md doc names it (TOML front matter: files
  as globs, loader as the code that parses it; then the layout;
  formats/README.md), or assets_scan finds it whole as TIM, TMD, VAB or VAG,
  or it is a stream of XA and STR sectors only;
- imported: the last port_check ran the importers (tools/import) on the disc,
  and an output's Provenance::of_disc names it;
- checked: a passing importer test marked it with import::checking after
  comparing the import with what the game built (RAM after loading, VRAM
  uploads, a GPU capture);
- or unused: formats/unused.toml, with the runs that never load it.

An archive's format doc says archive = true: the importer lists its entries
(import::index), which are then the assets, named /ARCHIVE:NAME (with @0xOFF
where names repeat) or /ARCHIVE@0xOFF. port_status counts them always, with
how many no traced run (trace_cd) loaded. With assets = true, port_next
gives, after the features, Format (a file or entry with no format), Index
(an archive the importer does not list), Import and Check. Only the
importers read the original's formats; the game reads the port's own.

## Commands

- context: the build, project root, media, BIOS and capabilities (setup).
- bios_install: a local BIOS, or a pinned download for a region (setup).
- guide: this text, or a topic: run, code, assets, setup.
- read: a returned file, by pointer, rows, hex or image.
- disc_info, disc_ls, disc_extract, disc_sector, disc_exe: the disc's
  tracks, files, sectors and executables (assets).
- run: the game for N fields from power-on or a state, with input,
  watches, samples, traces, GPU captures and states (run).
- ram_diff, ram_search, ram_view: values across snapshots (code).
- code_disasm, code_xrefs, code_decompile, code_functions,
  code_annotate, code_symbols: reading code (code).
- code_inventory, code_coverage, code_coverage_diff: every function, and
  what runs and recordings executed (code).
- assets_scan, assets_extract, assets_textures, assets_diff,
  assets_memcard: standard formats, VRAM and memory cards (assets).
- states_save, states_list, states_rebuild: named start states and their
  chains (setup).
- scenario_record, scenario_check, scenario_show, scenario_play:
  recordings.
- port_init, port_update, port_status, port_next, port_check: the port
  (port_update: setup). psx port confirm is the person's.

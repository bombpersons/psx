A port is one workspace and one agent: it reads the original with psx and
writes the port in the Bevy template. psx keeps the port's state on disk and
says what is due next; the person chooses the goal.

Start: psx port init DEST --media GAME.cue writes the workspace: the
template (psx-bevy/, game/, tools/import, vendor/), goal.toml, catalogue/,
media/ and bios/ linked in, .mcp.json for psx, and git. The region comes
from the disc; pass fields_per_tick from the original's main loop.

goal.toml names the features the person wants (ids, or "all"), a
description, and assets = true when the goal includes the disc's assets. catalogue/F-NNNN.md is one feature of the game each: id,
title, area, depends (features it cannot work without), code (the
original's functions, found with code_coverage_diff), recordings (work and
held_out, as scenario/name), wont_reach (branches no recording can reach,
and why; the person confirms them) and questions, as TOML between +++
lines, then a paragraph in game terms (catalogue/README.md has the format
and how fine to cut). psx checks each against the workspace. The goal's features and everything they depend
on are its closure. The agent proposes the catalogue; the person checks it
with psx port confirm and confirms with psx port confirm --yes. The list may
grow afterwards; taking a feature out needs confirming again.

Every session: port_next gives one item, with why it is due, done_when (the
fact that will show it done) and how. Do it until done_when holds, commit
(naming the feature's id in the message: the review finds its files that
way), and call port_next again. port_status shows each feature of the goal
by stage, its recordings, branches, questions and review; plain=true is
text for the person.

Stages, each holding with the ones before it: catalogued (an entry),
located (its code named), recorded (work recordings, their scenarios
deterministic, their coverage counted), matching (every work recording
passes at HEAD), reviewed (every branch of its code taken both ways by its
recordings or a confirmed wont_reach, no question open, a review that
holds), done (its held-out recordings pass). Held-out results are shown
only once the feature is reviewed: don't tune the port to them.

Items, in this order:
- Catalogue: goal.toml or the catalogue has problems (listed); fix them.
- Confirm (person): the goal's list is not confirmed, or lost a feature;
  until it is, only investigate and catalogue. Later also for wont_reach
  branches the person has not confirmed as they stand.
- Build: the port cannot replay a feature's recordings (no launcher for a
  scenario kind, a quantity not observed).
- Unpack: overlays the inventory knows only from code that ran in them.
- Locate: a feature's entry names no code: coverage diff of runs doing it
  against runs not doing it.
- Record: its recordings are not ready (scenario_record, then
  scenario_check, the determinism check; docs/scenarios.md). The glossary
  binding's tick is the game's logic step, not its render loop, and the
  chain's last run ends at a game moment (until), not a count of fields.
- Implement: no port check has judged it at HEAD.
- Fix: a recording fails; the earliest divergence first.
- Answer: an open question in its entry.
- Cover: a branch of its code its recordings never took one way: record
  what reaches it, ask the person to play it, or list it under wont_reach
  with why.
- Review: its own work is finished without a review that holds. how is
  the brief for a reviewer who has not seen the work; findings go in
  reviews/F-NNNN.md (TOML front matter between +++ lines: commit, files;
  then one ## heading per finding), each answered with an **Answer:**
  line. A review holds while its files are unchanged since its commit.
- Play (person): a reviewed feature without held-out recordings: start
  scenario_play for the person and name the recording under held_out.
- Format, Index, Import, Check (with assets = true): a disc file or archive
  entry with no format yet, an archive whose entries the importer does not
  list (import::index), one not imported, one not checked against what the
  game built (formats/README.md).
- Stuck (person): the same item three times with nothing changed. Stop and
  ask.
- Done: every feature of the goal is done.

Verdicts are psx's: it runs the checks and writes .psx/verdicts/F-NNNN/,
never the agent. A verdict counts only for the commit it judged, with no
uncommitted changes, so commit before checking. port_check (optionally one
feature) runs the game's tests/scenarios.rs, where the port registers
scenario launchers and observe the glossary's quantities
(psx_bevy::scenario, AGENTS.md), and writes the verdicts: each recording the
entry names replayed tick by tick with its pad, every quantity within the
scenario's tolerance. While working, cargo test -p <game> --test scenarios
gives the same result without a verdict.

How to write the port (what the code is held to, the original as
evidence, the review, and what a goal may ask for beyond features) is in
the workspace's AGENTS.md and game/AGENTS.md.

Evidence: the disc and psx are the only sources. Record the original as
scenario recordings from named start states (guide topic states), and cite
the address, writer or recording beside each constant and rule. Each
recording keeps what the original ran as the inventory's code
(<name>.coverage.json), so take code_inventory before recording, and unpack
compressed overlays first (guide topic code, Decompressed code).
scenario_show reads a recording back per tick, with each numeric
quantity's change since the tick before.

Assets: each disc file but the code is unknown, format found (a
formats/<id>.md doc names it, with the code that parses it, or assets_scan
finds it whole as TIM, TMD, VAB, VAG, XA or STR), imported (the last
port_check ran the importers on the disc, and an output's
Provenance::of_disc names it), checked (a passing importer test marked it
with import::checking after comparing the import with what the game built:
RAM after loading, VRAM uploads, a GPU capture) or unused
(formats/unused.toml, with the runs that never load it). An archive's
format doc says archive = true: its entries, listed by the importer's
import::index, are then the assets, named /ARCHIVE:NAME or
/ARCHIVE@0xOFFSET. port_status shows them always, with how many no traced
run (trace_cd) loaded; port_next asks for them when the goal has
assets = true.

Template updates: port_update replaces untouched psx-bevy/ and vendor/
files, writes <file>.psx-new beside edited ones (merge it, keeping the
change listed in docs/template.md, then delete the .psx-new) and copies
changed seeded files under .psx/template-update/ to merge; a seeded file
the template dropped is listed with removed_upstream.

A port is one workspace and one agent: it reads the original with psx and
writes the port in the Bevy template. psx keeps the port's state on disk and
says what is due next; the person chooses the goal.

Start: psx port init DEST --media GAME.cue writes the workspace: the
template (psx-bevy/, game/, tools/import, vendor/), goal.toml, catalogue/,
media/ and bios/ linked in, .mcp.json for psx, and git. The region comes
from the disc; pass fields_per_tick from the original's main loop.

goal.toml names the features the person wants (ids, or "all") and a
description. catalogue/F-NNNN.md is one feature of the game each: id,
title, area, depends (features it cannot work without), mechanics,
scenarios, and a paragraph in game terms (catalogue/README.md has the
format and how fine to cut). The goal's features and everything they depend
on are its closure. The agent proposes the catalogue; the person checks it
with psx port confirm and confirms with psx port confirm --yes. The list may
grow afterwards; taking a feature out needs confirming again.

Every session: port_next gives one item, with why it is due, done_when (the
fact that will show it done) and how. Do it until done_when holds, commit,
and call port_next again. port_status shows each feature of the goal by
stage (catalogued, captured, implemented, verified, reviewed, extracted),
its verdicts and the problems; plain=true is text for the person.
- Catalogue: goal.toml or the catalogue has problems (listed); fix them.
- Confirm (person): the goal's list is not confirmed, or lost a feature.
  Until it is, only investigate and catalogue.
- Fix: a verdict at HEAD fails; its failures name the mechanic, step and
  field, or the scenario, tick and quantity.
- Implement, Extract: the feature's next stage.
- Unavailable (person): the next items need commands psx does not have yet.
- Stuck (person): the same item three times with nothing changed. Stop and
  ask.
- Done: every feature of the goal is extracted.

Verdicts are psx's: it runs the checks and writes .psx/verdicts/F-NNNN/,
never the agent. A verdict counts only for the commit it judged, with no
uncommitted changes, so commit before checking.

How to write the port (the shape of the code, numbers, evidence, the
self-review before calling a feature done, and the choices a goal can make:
an engine/game split, mod crates, the whole front end, an editor) is in the
workspace's AGENTS.md and game/AGENTS.md.

Evidence: the disc and psx are the only sources. Name what you find in a
field map (guide topic fields), record the original as cases (guide topic
cases) from named start states (guide topic states), and cite the address,
writer or recording beside each constant and rule.

Template updates: template_update replaces untouched psx-bevy/ and vendor/
files, writes <file>.psx-new beside edited ones (merge it, keeping the
change listed in docs/template.md, then delete the .psx-new) and copies
changed seeded files under .psx/template-update/ to merge.

Use psx to gather evidence from a PlayStation game, then implement the recovered
rules and assets as ordinary game code. Choose the tools needed for the current
question; a full port is not a prerequisite for a useful investigation.

A port's workspace (port_init) holds goal.toml, the features the person
wants, and catalogue/, the game's features with their dependencies. The
agent proposes the catalogue; the person confirms the goal's list with
psx port confirm --yes (psx port confirm shows the list, its closure and
any problems first). In the workspace, start every session with port_next:
the one item due now, why, and the fact that will show it done.
port_status shows every feature of the goal by stage.

Otherwise start with context({}) to identify the running build, project
root, configured media/BIOS and capabilities. Relative paths resolve
against that root, which may differ from the client harness's working
directory.

Typical routes:
- Setup: context -> disc -> ls / exe -> a bounded run with checkpoints.
- Gameplay: from_state + input variations -> ram_diff / ram_search -> sample /
  watch -> disasm / Ghidra -> poke to test a hypothesis. Name what a port
  compares in the glossary, with the game's tick (docs/scenarios.md).
- Assets: filtered assets_scan -> assets_extract; use an external parser for
  custom formats. GPU captures and textures provide visual reference.
- Rebuild: template_create -> decoded assets and idiomatic Bevy rules -> compare
  recorded trajectories and rendered output against the original evidence.
- Whole port: port_init -> catalogue and goal -> port_next, item by item
  (guide topic port).

Tool descriptions are brief; guide topics hold the details. Use the run, input,
fields, states, code, assets, bevy and port topics for the relevant
route, and troubleshooting for unexpected results. Inspect returned files with artifact_read:
JSON by RFC 6901 pointer, JSONL/CSV/text by rows (frame_from/frame_to with
frame_field, hex pc, contains, columns), hex for raw bytes and PNG as an image.
Continue with page.next_cursor, reusing the filters; totals stay null until a
stream is fully scanned. A successful capture proves
what was recorded, not that the game reached gameplay or that a hypothesis is true.

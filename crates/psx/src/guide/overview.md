Use psx to gather evidence from a PlayStation game, then implement the recovered
rules and assets as ordinary game code. Choose the tools needed for the current
question; a full port is not a prerequisite for a useful investigation.

Start with context({}) to identify the running build, project root, configured
media/BIOS and backend capabilities. Relative paths resolve against that root,
which may differ from the client harness's working directory.

Typical routes:
- Setup: context -> disc -> ls / exe -> a bounded run with checkpoints.
- Gameplay: from_state + input variations -> ram_diff / ram_search -> sample /
  watch -> disasm / Ghidra -> poke to test a hypothesis.
- Assets: filtered assets_scan -> assets_extract; use an external parser for
  custom formats. GPU captures and textures provide visual reference.
- Rebuild: template_create -> decoded assets and idiomatic Bevy rules -> compare
  recorded trajectories and rendered output against the original evidence.

Use guide's input, code, assets and bevy topics for the relevant route, and
troubleshooting for unexpected results. Inspect returned files with artifact_read;
use page/cursor metadata to keep evidence bounded. A successful capture proves
what was recorded, not that the game reached gameplay or that a hypothesis is true.

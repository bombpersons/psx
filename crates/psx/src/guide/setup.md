## The project

Relative paths resolve against the project root: --root or PSX_ROOT, else
the MCP client's single workspace root, else the server's directory.
context reports it, the build, the media and BIOS in use (resolved, and
media_from says where from), capabilities and warnings; record its build
when results look odd, since a connected server may be older than the
source. project_root on any MCP call selects another existing project for
that call only. context with save:true keeps defaults in .psx/project.json,
which other clients of the root also see. A port workspace has its media/
and bios/ linked in.

## BIOS and media

Use a BIOS of the disc's region (context's disc_region_hint): the BIOS makes
the console, so a PAL disc on an NTSC BIOS runs on an NTSC console. When none
is installed, bios_install region download:true fetches a pinned,
hash-checked one (psx never downloads on its own), or from imports a local
dump. Don't record on another region's BIOS. States need the exact BIOS and
media they were saved with.

## Named states

A start state is where scenarios, plays and probes begin. A state file holds
the console's whole memory (the game's code), so it stays out of git; its
chain, every run from power-on that led to it, is small and committed.

- .psx/states/NAME.bin: the state.
- notes/states/NAME.json: the chain: a description in game terms, the media
  and BIOS, and each run's frames, input, pokes, until and pad, with the
  state each ended in and its sha256.

run save_as NAME description TEXT runs one step and names the state it ends
in; from_state continues from it:

  run frames 1500 save_as title description "the title screen"
  run from_state title frames 900 input [{"frame":10,"buttons":["START"],"until":14}]
      until 800A1F2C:u16=16 save_as level1 description "Lara at the level start"

states_save SOURCE NAME names a state an earlier run or play made (state.bin,
a state_fN.bin, a mark's state.bin or the run directory), its chain taken
from the runs' reports back to power-on or a named state. replace=true keeps
the replaced chain under notes/states/replaced/. states_list gives each
state and whether its file matches its chain. states_rebuild [NAME...]
replays chains from power-on (shared steps once) and reports per state the
first step that differs and status same, differs or restored; after a psx
update, rebuild, then replace=true, then scenario_check.

## The template

port_update replaces untouched psx-bevy/ and vendor/ files with this build's,
writes <file>.psx-new beside ones the port changed (merge it, keeping the
change listed in docs/template.md, then delete it) and puts changed seeded
files under .psx/template-update/ to merge; removed_upstream lists seeded
files the template dropped. Keep each project's cargo target directory to
itself.

## When results look wrong

- A missing file is resolved against the server's root, not the client's
  shell: use project_root or absolute paths.
- Input that does nothing: check input.jsonl, the pad state and frame
  origins, and whether the game reads the pad then (topic run, Driving
  menus).
- A blank or still screen: no_gpu_draws also fires on a still title or a
  load. cd_retry_loop is an emulator bug: report the LBA.
- Screen sizes change during boot; read the video mode before comparing
  images.
- Without a vision-capable model, leave return_screen off and say that the
  picture was not looked at.
- A Ghidra query that timed out keeps its partial analysis: retry with a
  longer timeout. stale_analysis after a Ghidra update analyses again.

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

A start state is where scenarios and plays begin. A state file holds
the console's whole memory (the game's code), so it stays out of git; its
chain, every run from power-on that led to it, is small and committed.

- .psx/states/NAME.bin: the state.
- notes/states/NAME.json: the chain: a description in game terms, the media
  and BIOS, and each run's frames, input, until, pad and adjustments
  (fix with its build, kept in notes/states/fixes/; a step that names none
  ran stock), with the state each ended in and its sha256. A rebuild runs
  each step as it ran; a step on newer fixes is a run saved again.

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

## The port's commands

The port is any program that keeps the port protocol, written in any
language. port init writes a ready-made template in Rust (port_add's
add-ons are Rust too), its commands filled in; a port written otherwise
replaces the template's game/ and tools/import/ and names its own. The
workspace root's `psx-port.toml` names the commands, run in the root:

```toml
[port]
build = ["make", "-C", "game"]                       # optional
run = ["game/build/game", "--psx"]                   # speaks the protocol
import = ["game/build/import"]                       # optional
import_test = ["make", "-C", "game", "test-import"]  # optional
```

- `build` runs before each check; a failure is the check's failure.
- `run` starts the port in protocol mode. A program named by a relative
  path is in the workspace.
- `import` is run as `import DISC OUT` (write the imported files and their
  `*.provenance.json` into OUT) and `import index DISC ARCHIVE...` (print
  each archive's entries as JSON).
- `import_test` runs the importers' tests with `PSX_IMPORT_REPORT` set: a
  passing test appends the disc paths it checked, one a line.
- Without the file, the commands are the Rust template's (cargo's).

## The port protocol

JSON lines: psx writes one request a line on stdin; the port answers each
with one line on stdout and flushes. stdout carries only the protocol (a
line that is not a JSON object is skipped); logs go to stderr.

```
-> {"hello":1}                                     <- {"kinds":["level"]}
-> {"launch":{"kind":"level","parameters":{...}}}  <- {"ok":true} | {"error":"..."}
-> {"tick":{"buttons":["CROSS","UP"],"analog":false,"sticks":[128,128,128,128]}}
                                                   <- {}
-> {"observe":{}}                                  <- {"values":{"croc_x":1234,"croc_gait":"walking"}}
-> {"screen":{"path":"/tmp/f/12.rgba"}}            <- {"width":320,"height":240} | {"none":true}
```

- `hello` carries the protocol version (1); answer with the scenario kinds
  the port launches.
- `launch` starts a fresh game from the scenario's kind and parameters:
  nothing of the last game is kept.
- `tick` runs one tick with that pad: the buttons held by name (SELECT,
  L3, R3, START, UP, RIGHT, DOWN, LEFT, L2, R2, L1, R1, TRIANGLE, CIRCLE,
  CROSS, SQUARE; any case), the analog flag and the sticks lx, ly, rx, ry
  as raw bytes (128 centred).
- `observe` gives the glossary's quantities after the last tick, in the
  recordings' encoding: integers as the raw word (a fixed-point quantity's
  underlying integer, a float's bit pattern), as decimal strings outside
  ±2^53; bools; an enum's variant by name; lists and records as arrays and
  objects. psx checks each value against its glossary type (a value out of
  range is a port that forgot to wrap). Leave out a quantity the port does
  not have yet; a name the glossary lacks is a failure.
- `screen` writes the frame after the last tick as `width * height` RGBA
  bytes, rows top to bottom, at the original's display size, or answers
  `none`.
- Any request may be answered `{"error":"..."}` (it could not be done) or
  `{"panic":"..."}` (the port failed: the recording fails at that tick).
  A port that exits or takes over two minutes on a reply is started again
  for the next recording.

Only the requests drive the game: no clock, no unseeded randomness, no
files that change between runs. psx runs each recording twice and reports
where the runs part. Without `--psx` the same program can be the game,
with its own window and pad.

## The template

port_update replaces untouched vendor/ files with this build's, writes
<file>.psx-new beside ones the port changed (merge it, keeping the port's
change, then delete it) and puts changed seeded files under
.psx/template-update/ to merge; removed_upstream lists seeded files the
template dropped. A workspace from the older Bevy template keeps its
psx-bevy/ as its own code (released counts the files no longer tracked).
Keep each project's cargo target directory to itself.

## Add-ons

port_add NAME writes a ready-made crate under addons/NAME/, a workspace
member game/ depends on, with the add-ons it needs. None is required: a
port of one feature's rules passes its scenarios on the template alone.

- shell: the window (windowed or borderless fullscreen, any size and
  aspect), the fixed-step loop at the port's tick rate, and the pad from
  keyboard and gamepads with psx play's bindings.
- renderer: wgpu drawing in a PS1 look (the PS1's resolution, affine
  textures, snapped vertices, dither, 15-bit colour) and a modern look,
  switched at run time: glTF models, textures, a camera, lights, fog and
  sprites; capture() draws a frame for --replay. Its example shows a model:
  cargo run -p renderer --example view -- MODEL.gltf.
- disc-import: tools/import run on the player's disc at first start, with
  a screen that asks for it.
- menus: F1 controls and F2 display menus.
- full: the four, wired into game/src/main.rs (an untouched one is
  replaced; an edited one gets main.rs.psx-new), with game/src/demo.rs
  standing in for the game's drawing. cargo run imports the disc and plays
  the demo.

Once added they are the port's code, to change or move into game/.
port_update writes a newer version of an add-on's file beside it as
<file>.psx-new.

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

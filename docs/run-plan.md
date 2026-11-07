# Plan: one machine loop, with run and a debugger on it

`run` is one 548-line function (`run_native::run`) over 73 arguments.
Each of its twenty or so observers is a local variable with its own setup,
its own `if` in the loop and its own block of the report, so adding one
touches all three. It also refuses `latch` and `fix`: only
`scenario::record` and the handoff (play, web) honour a recording's
adjustments, each with its own loop. So there are four loops driving the
machine (run, record with its lead-in, the handoff, diagnose), and the
observers live in only one of them.

This plan makes one loop that the others use, then puts a debugger on it:
a session that stops where the agent asks, lets it look, and continues.
Some of run's options move to the debugger, where they work better.

## What run is used for

From the call logs (`.psx/calls.jsonl`) of the four workspaces and psx's
own: 318 runs (265 from Gran Turismo 2's agent; 224 over MCP, 94 from
the CLI).

| Share | Arguments |
|---|---|
| almost every run | frames (99%), from_state (81%) |
| most runs | summary (68%), input (64%) |
| often | sample (43%), out (43%), timeout (35%), sample_per tick (26%), adjust (22%; steady 68 times, fix 6, latch 4), replay (22%), watch (21%), decomp (17%) |
| sometimes | watch_changes_only (11%), sample_every (11%), media (9%), max_events (6%) |
| rarely | trace, trace_cd, brief, save_state (4% each), pad_reads, until, watch_after (3–4% each), save_state_at, save_as (3% each), trace_frames, trace_from_frame, trace_range (2% each), diagnose, break, tail, pad, watch_reads (1% each), audio, trace_spu, snapshot_every (under 1%) |
| never | poke, find_tick, gpu_frame, gpu_next_draw, gpu_search_frames, gpu_draw_filter, trace_bios, trace_start, profile, memcard1, memcard2, symbols |

What the logs show:

- **run is mostly "from a state, with this input, sample or watch these".**
  The tracing, GPU and sound options are rare.
- **Agents wanted what run refuses.** Seven runs asked for latch or fix
  and were refused.
- **Agents wanted to start watching late.** Eleven runs passed
  `watch_after` without `diagnose`. Over MCP clap's `requires` is not
  checked, so the option was silently ignored, and the agent was trying to
  watch from field 118 on. Run 118 fields, then watch, is what a debugger
  does.
- **Two shortening modes is one too many.** summary is used 17 times as
  often as brief.

## The design

### The session

One type, `Session`: a machine, how its pad is driven, its adjustments,
the observers attached, and where it is (run field, absolute field, tick,
instructions). Its one loop is
`advance(&mut self, limit) -> Stop`: run until the limit (fields, ticks,
instructions or the deadline) or until an observer says stop, then say
why and where.

- **Start.** Power-on (BIOS, disc or EXE, with cards and pad), a state, or
  a recording (`scenario/name`). A recording gives its scenario's chain
  state, its pad by tick and its adjustments, with fix using the build the
  recording kept (probes plan step 3).
- **Driver.** What sets the pad: events by run field (input, replay), a
  recording's ticks, or a person (the handoff). Every input goes through
  the adjustments' `Running`, so latch works wherever there is a tick.
  latch, steady and fix are on by default wherever they can apply
  (step 3).
- **Observers.** One trait:
  - `step`, `field` and `tick` hooks, each answering continue or stop;
  - whether it needs every instruction;
  - `finish`, which writes its files and its section of the report.

  Watches, read watches, samples, `until`, the tracer, profile, pad_reads,
  find_tick, audio, screens, GPU capture, the CD/BIOS/SPU logs, checked
  C's report, the recorder and the debugger's breakpoints are all
  observers. The loop steps instruction by instruction when one of them
  asks, and runs whole fields otherwise. That replaces today's
  hand-written list of five.
- **Validation in one place.** Arguments become a session's
  configuration in a single conversion that CLI and MCP share, so a
  combination clap refuses is refused over MCP too (`watch_after` above).

`run` is then start, attach, `advance(frames)`, finish. `scenario record`
is a session from the scenario's state with the recorder observer.
The handoff advances a field at a time with the person as driver. Their
reports and recordings stay as they are now.

### The debugger

A debugging session is a `Session` that stops between calls. Calls:

- **`debug_start`:** run's start, driver, adjustments and decomp, plus
  any observers to keep for the whole session (watch, sample,
  watch_reads). Returns the session's id and where it is.
- **`debug_go`:** continue until one of its stops holds:
  - `pc=ADDR` or a name from names/;
  - `write=ADDR[:WIDTH]` or `read=…`;
  - a condition as `until` takes it;
  - `tick` (the binding's tick ends);
  - `draw` (a GPU draw, with run's draw filters);
  - `fields=N`, `ticks=N` or `instructions=N`.

  It also says what to keep on the way: `history=N` (the last N
  instructions), `trace` for a span, or `gpu` to capture the field's GPU
  commands. It returns why it stopped, where, the PC and its function, and
  what the session's observers saw since the last stop (watch hits, sample
  rows), with large output in files as now.
- **`debug_show`:**
  - registers;
  - memory by address or name in sample's formats;
  - the call stack, named;
  - the instruction history;
  - quantities and probes now;
  - the screen;
  - the last GPU capture (gpu_frame.json and its VRAM, which
    assets_textures reads as it reads a run's);
  - the hardware view diagnose gives today (CD history, unhandled I/O,
    hot PCs).
- **`debug_set`:** write memory or a register, or hold a pad state from
  here.
- **`debug_back`:** go back to an earlier stop.
- **`debug_save`:** a state file here, to continue from with
  `run --from-state`.
- **`debug_end`:** end the session.

A session lives on disk, not in the server: `.psx/debug/<id>/` holds its
start, each call and a checkpoint at each stop. That checkpoint is the
machine state plus the driver's and adjustments' own state, so `Running`
and the drivers become serialisable. Any call (MCP or CLI) restores the
latest checkpoint, acts and checkpoints again. So a session survives a
restarted server or an old binary's worker, `debug_back` is restoring an
earlier checkpoint, and the CLI can debug without a daemon. A checkpoint
is a few MB, written once per call.

### Moved from run to the debugger

| From run | In the debugger |
|---|---|
| diagnose, break, tail, max_steps, watch_after | debug_go pc= / history= / instructions=, debug_show hardware; watch from a stop |
| gpu_frame, gpu_next_draw, gpu_search_frames, gpu_draw_filter | debug_go draw (filters) with gpu, debug_show gpu |
| trace_start | debug_go pc=, then trace |
| save_state_at | debug_go fields=N, debug_save |
| poke | debug_set |
| brief | summary stays the one short form |

The rest stays in run: the observers it is used for, the trace window
that coverage recipes reproduce (trace_from_frame, trace_frames,
trace_range), until and save_as (chains), and the rarely used logs, which
are an observer each at no cost to the loop.

Chains use two of the dropped options. Three of the eighteen chains in
the workspaces have a step with save_state_at, from a person's mark. A
step that keeps a state at field N becomes a step of N fields, and
`states rebuild` rewrites old steps that way. No chain uses poke.

## Decisions

- **One loop, not shared helpers.** The four loops differ in who drives
  the pad and who watches, not in how the machine runs. Making those two
  things parameters removes the differences instead of copying them.
- **The debugger's sessions are files.** An in-memory session dies with
  its server, and GT2's servers have outlived their binary before. Files
  also give rewinding and a CLI for free.
- **Stops are events, not single steps.** Every call is a model turn, so
  the debugger is built for "run to where it matters, look, go on";
  `history` and `trace` cover the instructions in between in one call.
  `instructions=1` exists but is not the way in.
- **A debugging session names no state.** Chains are runs by fields;
  after debug_set or a stop mid-field, a state has no run that makes it
  again. debug_save writes a file; naming states stays with run's
  save_as.
- **Nothing judged changes.** Recordings, coverage and verdicts come out
  byte for byte as before; the tests and a scenario_check on Croc and GT2
  hold this.

## Steps

1. **The session, with run on it.** Session, Start, Driver (events),
   the observer trait, every run observer moved onto it, and validation
   shared by CLI and MCP. run_native::run becomes start, attach, advance,
   finish.

   *Done when* run's tests pass and the reports of a set of
   runs (one per observer) equal today's, apart from run_dir and wall_ms.

   *Done (2026-11-07).* `session.rs` (Session, Observer, Driver, one
   `advance`) and `run_native/observe.rs` (the events driver and fourteen
   observers); run is start, attach, advance, finish. Reports and every
   run file are byte-identical to the old binary's on 25 Croc runs (one or
   more per observer, plus errors) and six GT2 runs from its call log.
   `RunCmdArgs::check` refuses over MCP what clap refuses on the command
   line; three of those GT2 runs passed watch_after without diagnose and
   are now refused instead of ignored.
2. **Adjustments and recordings.** Running in the session; a recording
   as a start and its ticks as a driver; latch and fix refused only
   without one. This is the probes plan's step 3.

   *Done when* sampling a latched, fixed recording's quantities in run
   gives the recorded values on every tick.

   *Done (2026-11-07).* `run --recording SCENARIO/NAME`: the session
   holds the adjustments' `Running` (the driver sets the pad through it;
   it sees each step and tick after the observers), a recording's lead-in
   (`Session::lead_in`, `scenario record`'s: released until the first tick
   ends, fields counted from the next) and the `Ticks` driver, which ends
   the run at the recording's last tick (`recording_ended`).
   `scenario::Captured` loads a recording for run and for check's replay
   alike. Every captured recording in GT2 (8, latched, steady and fixed)
   and Croc (42, latched and steady) gives its recorded values at every tick; a test
   does the same on a latched test game and matches the replay's
   adjustment report and RAM. adjust takes steady alone; latch and fix
   come with a recording. frames is now optional (60, or the recording's
   length).
3. **The port's adjustments by default.** The recordings a port is
   matched against run latched, steady and fixed, so the game is
   investigated that way too. `run`, `states save` and (later) the
   debugger run with latch, steady and fix wherever each can apply:
   - **steady** always;
   - **latch** when the glossary binding builds and gives the tick. Where
     no tick ends for a while (the boot, a load, a menu whose loop the
     binding does not see), input passes through to the controller, as a
     play from power-on does now (one rule in `Running`, which the
     handoff uses too), and the report lists the fields it passed
     through;
   - **fix** when `fixes/` holds C, built when the run starts; C that does
     not build fails the run.

   The report names what was on and why any of the three was not (no
   binding, no fixes/). `adjust` names a set instead (`["stock"]` or `[]`
   for none); a run asking for latch or fix where it cannot apply is
   refused.

   Chain steps keep how they ran. A step saved from now on names its
   adjustments (`stock` when none) and, with fix, points to its build
   kept in `notes/states/fixes/` by its sha256 (run writes the build it
   ran as `fix.json`; `fix_build` runs a kept one), so a rebuild runs what
   the step ran, whatever fixes/ holds by then. A step that names none
   was saved before this and is stock. Coverage recipes keep theirs the
   same way. A play's reproduce arguments name its adjustments, so a
   state from a latched play is a chain step too.

   *Done when* every existing chain rebuilds to its sha256s, a state
   saved from a run under the defaults rebuilds to the same sha256 after
   fixes/ changes, and a run from power-on reports the fields latch
   passed through.

   *Done (2026-11-07).* run takes latch, steady and fix by default where
   each applies (`adjust_unavailable` says why one did not; `stock` or
   `[]` for none; named ones are refused where they cannot apply), and
   writes the fix build as `fix.json`; `fix_build` runs a kept one. The
   fallback is `Running::falling_back` (passing through after
   `STALL_FIELDS` fields with no tick, listed in
   `adjusted.passed_through`), and the handoff's play uses it too. A
   step saved now names its adjustments and keeps its build in
   `notes/states/fixes/`; one that names none runs stock, as do old
   coverage recipes, and a play's reproduce names its adjustments. All
   18 chains (Croc 6, GT2 8, TRC 4) rebuild to their sha256s. On a copy
   of GT2's workspace a power-on run under the defaults latched, steadied
   and fixed, passing through fields 11 to 1800 (no tick in the boot and
   menus); two steps saved from it rebuilt to the same sha256s after
   fixes/ was changed to build a different image. The handoff's
   `verify` still replays a play stock; step 5 moves it onto the
   session.
4. **record on the session.** The recorder observer, with lead_in and
   reach_end as advances of the same session.

   *Done when* scenario_check passes on Croc's and GT2's
   recordings, and re-recording one gives the same file.

   *Done (2026-11-07).* `scenario record` is a session: the adjustments'
   `Running`, the `Ticks` driver (or `ByField`, events by field, without
   a tick), a coverage observer and the recorder observer, which reads
   the quantities at each tick end. `Session::lead_in` feeds the
   observers too and says whether a tick ended (`LeadIn`). GT2's 8
   scenarios check deterministic. Croc's check refuses its 42 recordings
   as recorded before psx measured input every tick, as the old binary
   does, so a re-record stands in: GT2's brake (latched, steady, fixed),
   a stock GT2 recording and Croc's jump_and_stomp (latched, steady)
   record the same rows, header and coverage as before, and the same
   rows as their recorded files.
5. **The handoff on the session.** The person as a driver, and rewind
   from the session's checkpoints.

   *Done when* a play recorded by a test gives the same
   recording as before, and web's play still rewinds.

   *Done (2026-11-07).* The handoff's play is a session with the
   `Person` driver (the pad the window or page holds, applied at the
   field boundary when it changed); each frame is one advance. Rewind,
   quick load and start over restore through `Session::restore`, which
   starts the adjustments again (`Running::restart`), from
   `session::Checkpoints`. A scenario play's start is the session's
   lead-in, with the pad released as `scenario record` releases it, and
   verify replays with the play's adjustments, not stock. A field
   counts as ended at the next boundary, so latch's state shown in the
   window turns one field later; the machine runs the same. The scenario
   play test leaves the same recording, coverage, continuation and end
   state before and after, on GT2's b1_brake (latched, steady, fixed)
   and Croc's croc_animations (latched, steady). The disc test (rewind,
   quick save and load, start over, verify) passes, and a new one checks
   a steady play verifies. The web page calls the same rewind; it was
   not tried in a browser.
6. **The debugger.** Sessions on disk, the debug_* calls over MCP and
   CLI, and stops and shows as above.

   *Done when* a test stops at a PC, at a write and at the
   tick, goes back one stop, and shows the same registers as the first
   time.

   *Done (2026-11-07).* debug.rs: `psx debug start|go|show|set|back|save|end`
   and the debug_* tools, with guide topic debug. debug_start is run's
   start (`run_native::begin`, which run now uses too: media, state or
   recording with its lead-in, decomp, the port's adjustments) with the
   input by field (`ByField`, from replay) or the recording's ticks as
   driver. A session is `.psx/debug/<id>/`: session.json (its start,
   its stops, the one it is at) and per stop the machine state and a
   point (`session::Place`, the adjustments' `Running` and the driver,
   all serialisable now); loading a stop puts the decomp and the
   adjustments back on the machine (`Running::reinstall`), which a state
   leaves out. Observers' `step` and `tick` return `Flow`; a stop at a
   field boundary leaves the driver's input due for the next advance.
   debug_go stops at pc, write, read (CPU only), until, tick, fields,
   ticks, instructions, with history=N, limit and timeout; debug_show
   gives registers, values (sample's formats, quantities, probes),
   bytes, history, screen and stops; debug_set pokes, sets registers or
   the pad; debug_back moves to an earlier stop (stops are never
   dropped); debug_save copies a stop's state. The test (from a
   recording: tick, the tick's PC again, a write, back one, the same
   write and registers) passes on Croc's jump_and_stomp and GT2's
   brake. A GT2 session 20 fields into brake has run --recording's
   registers, and ten fields in four calls leave the state one call
   leaves (the instruction count differs by a few: the core's idle-loop
   skipping is not in a state). GT2's b1_brake and b1_launch check
   deterministic. Left for later: the observers kept for a whole
   session (watch, sample, watch_reads), draw and gpu, trace, the call
   stack and debug_show hardware (step 7 brings diagnose's); inline
   input and memory cards at the start, to keep the tool list in its
   budget.
7. **The moves.** diagnose, the GPU capture options, trace_start,
   save_state_at, poke and brief come out of run; their uses go to the
   debugger (diagnose's report becomes debug_show hardware);
   `states rebuild` rewrites save_state_at steps. Guide topics run and
   assets say where things went.

   *Done when* every chain in the workspaces rebuilds to the
   same state sha256, and the guide's run topic is shorter than now.

   *Done (2026-11-07).* run has no diagnose, break, tail, max_steps,
   watch_after, gpu_*, trace_start, save_state_at, poke or brief.
   debug_go draw (any, primitives, textured) stops at the end of the
   first whole field that draws and captures it; gpu captures the first
   whole field from the stop; trace traces the go as run's trace does.
   Each keeps its files in the stop's directory, `stops/<n>/`, with a
   report.json, so assets_textures takes it as run_dir. debug_show gpu
   gives the latest capture on the way back through the stops, and
   hardware gives what diagnose reported (CD commands, SPU, unhandled
   I/O, DMA, interrupts, and the hot PCs and I/O of the next two
   fields, `native::hardware` and `native::hot`). debug_set's poke is
   `ADDR=VALUE[:WIDTH|:FMT]`. The GPU capture and trace are observers
   that run_native and the debugger share (`observe::Gpu`,
   `run_native::tracer`). `states rebuild` rewrites a step that kept a
   state at field N as a step of N fields with the input before N
   (`states::upgrade`) and writes the chain back when the state is the
   same. The run topic is 12025 bytes, down from 12136, and says what
   is in the debugger now; assets and debug say how to capture a field.
   The run-based GPU capture test is now a debugger test (no draw ends
   at the limit with not_found; a draw after a poke stops and is
   captured; show gpu and assets_textures read it). Croc smoke test:
   draw textured, then trace and show hardware. All 18 chains (GT2 8,
   Croc 6, Tomb Raider Chronicles 4) rebuild to the same sha256s; GT2's
   b_license_tests, f0010_cd_trace and license_menu now start with a
   1500-field step (the press at 500 and release at 512 kept). The Mega
   Man 8 test's poke is a debugger poke now, but its state is an old
   format (8) that the core no longer loads, so it did not run.
8. **A trial.** GT2's agent chases a divergence with the debugger.

   *Done when* its feedback file says what it used and what it
   missed.

## Not in this plan

- **Scripts in the debugger or the bindings** (Rhai or similar): held
  until this lands.
- **Breakpoints on GTE or DMA events**: until a trial asks for them.
- **A debugger in psx web**: the sessions are files, so web could show
  one later.

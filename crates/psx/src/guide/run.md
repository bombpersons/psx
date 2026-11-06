run executes a disc, EXE, BIOS boot or state for a bounded number of fields
(vblanks) on psx's native core. from_state takes a named state, a state file
or a run directory, and needs the BIOS and media it was saved with; without
it the run boots from power-on. run never downloads.

## Frames and input

frames, input, poke, gpu_frame and trace windows count from the run start
(run_frame); frame in logs is absolute (since power-on, or the state's own
origin). Row N of a sample is memory after N fields, before field N's input
and pokes.

input is an inline JSON array of whole held pad states; replay is a file
with the same array. Choose one.

  [{"frame":0,"buttons":["CROSS"],"until":5},{"frame":10,"buttons":[]}]

Each event replaces the whole held state and stays until the next one or its
until (exclusive: [0,5) is five fields); buttons:[] releases all. Events are
strictly ordered within 0..=frames and do not overlap. Before the first
event a state keeps its saved pad. Buttons: SELECT L3 R3 START UP RIGHT DOWN
LEFT L2 R2 L1 R1 TRIANGLE CIRCLE CROSS SQUARE (any case), and ANALOG,
the DualShock's mode button: a press toggles analog mode unless the game
locked it. sticks [lx,ly,rx,ry], 0..255 with 128 centred, need pad
dualshock at power-on and analog mode (the game's or ANALOG's).
input.jsonl says when each event was applied, not that the game read it.

Driving menus. A game reads the pad once a tick, and a menu acts on a
press's edge (held this tick, not the tick before). Hold each press 4 fields
and release 4 before the next; a game that ignores that is not taking input
yet (a fade, a load): wait on a state, not more fields. A menu's cursor is a
variable like any other (topic code): until on it waits on the item.

## Adjusted recordings

scenario_record and scenario_play adjust the original to the port's model
unless stock is given, and the recording names what they did:

- latch: the game's pad changes only where a tick ends, to that tick's
  input. Input waits for the tick's end; psx writes it into the game's pad
  buffer at once and puts the buffer back when the interrupt handler polls
  into it mid-tick. Every read in a tick sees that tick's input, as the
  port's tick does. adjusted keeps the buffer it found and the lag frames:
  ticks in which no game code read the pad. late_input lists the ticks
  in which a whole field passed with no poll into the buffer and no game
  code read it before the tick ended: the game ran that field on the
  last poll's buttons, so the original's own driver delays the input. Input must reach the game every tick, so a port never
  copies the delay: find the function that holds it back and replace it
  in fixes/ (topic code).
- fix: the functions of fixes/ run in place of the game's own, so the
  game takes its input every tick. late_input empty with fix and latch on
  is the fix working; fix_calls counts the replacement's calls.
- steady: no tick overruns its fields. The CPU's own work costs a quarter
  of its clocks and the GPU draws without taking time; timers, the CD, the
  SPU and video keep the real clock. fields_per_tick shows how many ticks
  took each number of fields.

Either can change what a game does. scenario_adjust_check records a
scenario again from its state, stock and adjusted, and gives each
quantity's first tick where they differ (adjust names one alone, to
tell which caused it): a press taking effect a tick sooner is latch's
cost; a value the stock game never reaches is a rule that depends on
timing, so record that scenario stock and say why in the feature's
notes. run with pad_reads shows where a game keeps its pad (the
buffer its handler fills, the copies it makes), the code that reads it,
and the fields in which nothing read it.

## Memory formats

sample, until and poke read memory as ADDR:FMT. FMT is u8 i8 x8 u16 i16 x16
u32 i32 x32, fxI.F (fx16.16, fx4.12), bits of a word (u16[14] is bit 14 as 0
or 1, u8[4..7] bits 4 to 7 as a number; a poke writes those bits alone), a
byte string strN (printable ASCII as itself, other bytes as \xNN, trailing
zeros left off; until takes = and != only) or a width 1/2/4.

## Observations

- sample: samples.csv, one row per field. Each entry is a glossary quantity
  by name (player_x), ADDR:FMT[:NAME] or {"addr","format","name"}.
  sample_every thins rows. sample_per tick takes a row where the glossary
  binding's tick ends, with a tick column; glossary names and ticks build
  the workspace's binding and need its media. Values are raw, as recordings
  hold them.
- until ADDR:FMT=VALUE (also != < <= > >=; several must all hold) stops at
  the first field where it holds: until_met, and the report's until gives
  run_frame and the values. frames caps the wait; until.met false means it
  never came. A route is a chain of runs: until the menu is up, then
  from_state with the press at frame 0.
- find_tick counts every CPU store to main RAM by PC and address and groups
  those stored in exactly the same fields (tick_finder.json, most storing
  PCs first). Run it over a few hundred fields of busy play. The first group
  not stored "1 in every field" is usually the logic's, and its
  fields_by_stores is the tick's rhythm ({"0":300,"1":300}: every other
  field); a double-buffered game also shows drawing groups every fourth
  field. Bind a store made once per pass of the loop, on the path every pass
  takes (a counter is best), in main RAM, and check it is in the group:
  reading the code alone misleads, a branch can skip a store.
- watch ADDR[:WIDTH]: every CPU and DMA store into the range, with the
  writer's PC and old and new values (writes.jsonl); watch_changes_only
  drops same-value stores. watch_reads LO-HI: CPU loads with PC (reads.jsonl).
- poke ADDR=VALUE[:WIDTH|:FMT][@FRAME|@A..B|@A..] writes memory at the start
  of a field to test a hypothesis; keep an unmodified run to compare.
- trace: trace.txt (windowed by trace_from_frame / trace_range), and for
  coverage and the inventory code.bin (each RAM page's code as it ran),
  executed.bin, branches.bin (each conditional branch's ways taken) and
  calls.json (callers and call moments). max_events caps the event logs and
  trace.txt, not those files; code_pages_dropped or call_frames_dropped above
  0 says they are incomplete.
- trace_cd: CD transfers, decoded with a disc into loads.json (each
  transfer's file and offset, where in RAM it went, and whether the final
  RAM still holds it). trace_bios: BIOS calls.
- trace_spu: voice key on and off, first END and sound RAM uploads
  (spu.jsonl); sample_sha256 matches assets_extract's adpcm_sha256 (topic
  assets). audio: audio.wav of what the console plays.
- profile: CPU cycles by function, with calls and the hottest instruction;
  time in the VSync wait shows how much of each frame is idle.
- gpu_frame N captures one frame's GPU commands; gpu_next_draw searches for
  one that draws (topic assets). snapshot_every saves screens.
- save_state writes state.bin at the end, save_state_at state_fNNNNNN.bin at
  chosen fields; save_as NAME names it (topic setup).
- memcard1/memcard2 take a .mcd or new (written cards are saved in the run
  directory); pad dualshock enables sticks.

## Results

report.json in run_dir is the whole report; brief gives a summary with the
files, summary only status, frames, paths and counts. status partial with
termination timeout means the wall-clock budget ran out (frames.reached).
complete with frames_reached means the budget was met, not that the game
reached gameplay. Warnings: no_gpu_draws (nothing drawn lately: a still
title or load screen does that too), cd_retry_loop (the game re-reads one
LBA forever: an emulator bug, report the LBA). Boot and menus take as long
as the game takes: find a moment with snapshot_every and until rather than a
fixed field.

## Asking the person

When scripted input cannot reach a place in reasonable time (platforming,
combat, long routes), scenario_play without a scenario asks the person to
play from media or from_state, with the request beside the game. The
request joins a queue and the call returns at once: go on with other work.
The person answers when they can, in psx web (in the browser or a window)
or with psx play, or declines with a note saying why or what to ask
instead. port next gives a Read item when an answer arrives; status reads
it (wait blocks up to 600 s for one). expect gives the end state asked for
(until's syntax): the result says whether it held, and ask_again. Ask for
marks to find where the game keeps something: "mark just before and after
you pick up the key". The person plays with keyboard or gamepad, can
rewind, quick save and load, restart, mark and hand back done or say they
cannot do it.

Results: status (done, declined, closed, abandoned); state.bin to continue
from; input.json, every pad change with rewound input removed, and
reproduce, the run that replays it (verify=true checks it); ram.bin,
screen.png and the person's note; marks/<n>/ per mark (state, RAM, screen,
label), each with a RAM diff against the mark before and on_route;
route_snapshots for ram_search. Analog games want pad dualshock.

## Messages

The person can write from psx web at any time. What they wrote arrives
with the result of a later psx call, under messages. Reply at once, in a
line or two, then go on with the work: they are waiting on the page.
message sends them text (reply_to answers in a thread), and
without text lists the latest threads. From a shell, psx message --wait
blocks until they write (at most an hour; after --text, until they
answer) and prints what they wrote: run in the background, it ends when
they do, which is a way to hear from them while nothing else runs.

diagnose is an emulator diagnostic for psx's own development, not for
reading a game.

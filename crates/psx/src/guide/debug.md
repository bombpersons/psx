# The debugger

A debugging session is a run that stops where you ask, so you can look,
change something and go on, or go back. debug_start starts one where a
run would start: media, from_state or a recording (SCENARIO/NAME, with
its adjustments, lead-in and pad by tick), replay (a file of pad
states by field, as run's), and by
default the port's adjustments (latch, steady and fix where each
applies; adjust and fix_build as run takes them), decomp as run's. It
returns the session's id, stopped before the first field. The other
calls take id, or act on the latest session started.

## Stops

debug_go runs until one of its stops holds, and the place it stopped is
a new stop:

- pc: the CPU is about to run the instruction at an address or a name
  from names/; with when, only where its conditions also hold (until's,
  registers too: `a3=0x800811D0`), running on past the hits that miss.
- write, read: right after a CPU store or load touching `ADDR[:WIDTH]`
  (4 bytes, or a names path's size, by default) or `LO-HI`, in hex or
  names (topic run, Memory formats). DMA transfers are
  not seen.
- until: a field boundary where conditions hold, as run's until.
- tick: the binding's tick ends (the glossary's binding for this disc).
  That is where a recording takes the tick's row: memory holds what the
  recording has for tick N, and tick N+1's work comes after the stop.
- draw: the end of the first whole field that draws (any, primitives or
  textured), captured as gpu captures it.
- fields, ticks, instructions: after that many.

limit (fields, default 3600) and timeout (seconds, default 120) end a
go that found none; its stop says why ("limit", "timeout",
"recording_ended" when a recording's ticks run out). history=N keeps
the last N instructions on the way. These keep what happens on the
way in the stop's directory, stops/N/, as run's do:

- trace: trace.txt, calls.json and coverage.
- gpu: the GPU commands of the first whole field from here
  (gpu_frame.json, its VRAM and PNGs).
- sample (with sample_every, sample_per): samples.csv, row 0 the stop
  the go starts from (or the next field, from a stop inside one), then
  a row per field or tick to the stop it makes; tick counts from the
  session's start, as the stops' ticks do. sample_pc ADDR instead takes
  a row each time ADDR is about to run, without stopping, its tick the
  one it runs in: with registers as columns (`ra`, `a0`, `a1->8`) it
  lists a function's calls, their callers and arguments.
- watch (watch_changes_only): writes.jsonl, every CPU and DMA store;
  watched gives the address each watch landed on.
- watch_reads: reads.jsonl, every CPU load; it slows the go as read
  does.

The stop lists the first stores, and the writers and readers by PC with
their counts; the files hold up to
100000 each. A stop gives the field (frame
absolute, run_frame from the start), the ticks, the PC, its function
and instruction, and for write and read what was written or read and by
which instruction.

## Looking and changing

debug_show shows the stop: registers by default (load_in_flight, where
the instruction before the stop was a load, is the value its register
takes after the stop's instruction, which still reads the old one); value
takes what sample does (`ADDR:FMT[:NAME]`, names paths, registers,
quantities by name);
bytes gives memory as hex; history the instructions go kept; screen a
PNG; stops every stop and the one the session is at; gpu the latest GPU
capture (topic assets); hardware the CD controller's recent commands, the
SPU, unhandled I/O reads, DMA and interrupts, and the PCs and I/O hottest
over the next two fields, for psx's own development; stack the calls the
stop is inside, innermost first, each with its function, sp, frame size
and where it returns to, read from the functions' prologues (`addiu
sp,sp,-N`, `sw ra`), so code that keeps its frame otherwise ends it
early (ended says why).

debug_set writes memory (poke `ADDR=VALUE[:WIDTH|:FMT]`), a register
(pc, hi, lo or a register by name), or holds a pad state from here (not
in a recording's session, whose ticks set the pad). It makes a new stop.

## Going back and keeping a state

Each stop is a checkpoint, kept until the session ends: debug_back goes
to the stop this one came from, or to=N, and going on from there makes
new stops beside the old ones. debug_save writes the stop's machine
state to a file for run from_state (run chooses its adjustments again).
Naming states stays with run's save_as. debug_end removes the session's
files in .psx/debug/.

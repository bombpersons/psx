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
  from names/.
- write, read: right after a CPU store or load touching `ADDR[:WIDTH]`
  (4 bytes by default) or `LO-HI`, in hex or names. DMA transfers are
  not seen.
- until: a field boundary where conditions hold, as run's until.
- tick: the binding's tick ends (the glossary's binding for this disc).
- draw: the end of the first whole field that draws (any, primitives or
  textured), captured as gpu captures it.
- fields, ticks, instructions: after that many.

limit (fields, default 3600) and timeout (seconds, default 120) end a
go that found none; its stop says why ("limit", "timeout",
"recording_ended" when a recording's ticks run out). history=N keeps
the last N instructions on the way, trace traces them as run's trace
does (trace.txt, calls.json, coverage), and gpu captures the GPU commands
of the first whole field from here (gpu_frame.json, its VRAM and PNGs);
both go in the stop's directory, stops/N/. A stop gives the field (frame
absolute, run_frame from the start), the ticks, the PC, its function
and instruction, and for write and read what was written or read and by
which instruction.

## Looking and changing

debug_show shows the stop: registers by default; value takes sample's
formats (`ADDR:FMT[:NAME]`) or glossary quantities and probes by name;
bytes gives memory as hex; history the instructions go kept; screen a
PNG; stops every stop and the one the session is at; gpu the latest GPU
capture (topic assets); hardware the CD controller's recent commands, the
SPU, unhandled I/O reads, DMA and interrupts, and the PCs and I/O hottest
over the next two fields, for psx's own development.

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

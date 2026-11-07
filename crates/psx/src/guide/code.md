Start from an observation: a sampled value, a writer's PC, a read, a CD
load. Ghidra's names and pseudocode, and every heuristic here, are
hypotheses to check against the instructions and what runs do (MIPS delay
slots, GTE operations).

## Finding things in RAM

ram_search keeps the addresses whose values satisfy pred between
consecutive snapshots (changed, unchanged, increased, decreased, delta=N;
one for all pairs or one per pair), exact values per snapshot ("441,*,376")
or a between range; save writes the matches and candidates narrows a later
search to them. ram_view reads a struct across snapshots (only the rows
that differ, unless same=true); ram_diff clusters nearby changes between
two.

- The player: search 32-bit values for its position while it moves one way
  (width=4, between, increased), then narrow. Its other fields are near it:
  ram_view the struct across snapshots.
- Input: hold one button and diff against holding none for the game's input
  word; a write watch gives the routine that writes it, code_xrefs on it the
  readers, where behaviour starts. code_xrefs bits=MASK keeps the loads
  tested with one button's bit.
- Arrays of objects: one element found, the others are a stride apart;
  code_xrefs on its address often finds the table's base.
- An object at another address in each level: find it in two, then search
  for a word holding its address in both (with or without its 80 prefix);
  the glossary's binding reads it through that pointer.
- The tick: run with find_tick (topic run).

## Reading code

code_disasm takes source (a host EXE or disc.cue::/PATH), ram (a run
directory or ram.bin), or both; function selects a heuristic function
extent (read its boundary evidence and stop reason), text:true gives
lines, listing.next_addr continues.

code_xrefs finds references to an address or a LO-HI range: jal, j and
branch targets, lui pairs, gp-relative accesses, and aligned data words
holding the address (pointer and jump tables; no_data skips them). Give gp
for RAM. A busy global has hundreds: kind keeps loads or stores, bits=MASK
the loads tested with those bits.

code_decompile takes source and addr, importing and analysing on first use;
reuse its analysis handle. For overlays give ram with ranges, entries and
gp. GTE operations read as calls by register and command
(gte_set_VXY0(v), gte_RTPS(sf,lm), gte_get_SXY2()), without their
arithmetic. A note says why the pseudocode reads registers nothing sets
(unaff_, in_): a start inside a function, or code that does so on purpose.
Names files hold one fact per line, read as C, `//` for evidence and a
trailing `?` for a guess; fields by offset, parts nobody knows left out:

    struct Object 0x988 {
      0x038 int floor_height   // written by world_collide
      0x370 int x
    }
    global 0x8007528C Object *croc
    function 0x80031F64 void world_collide(Object *obj, int unk, int area)
      local_150 Object *+0x38 obj_38
    function 0x80100000 in RACE.OVL race_update?

The project's names files (names/ and .psx/names/, *.names) apply to every
decompile; its names field counts what applied and lists what did not, by
file and line, and unnamed lists what the decompile still leaves unnamed,
most often seen first: callees and globals by Ghidra's names (FUN_, DAT_)
and offsets in no field of a named struct (Poly+0x24). code_names QUERY
gives where a function, global, type, Type.field or function.local is
defined and what it resolves to (a hex address: the names covering it);
with no query and a source, the files' problems and what the functions
the captured recordings call most leave unnamed, weighted by those calls. A function's name applies where the code at its address is
the inventory's function there, by its bytes: `in MODULE` (an overlay's or
executable's file) names one of several loaded at one address, and a name
for code not in the analysis is left out without a word. A project open
in the Ghidra GUI is an error until closed. A query cut short (a deadline,
a killed psx) is recovered by the next one; a lock left by a dead process
(psx.lock under .psx/ghidra/) is removed by hand only after checking its
pid is gone. code_symbols names PsyQ library code by signature, not the
game's own functions, and reports the build: gp, GCC's division checks,
scheduling, hand-written library code. Given a disc it searches every
file, the decompressed files the inventory was taken with (or unpacked=)
and the sectors no file owns for debug files (SN SYM, psylink MAP, ELF),
source paths and GCC strings; a debug file most of whose functions start
an executable's functions is imported to .psx/names/ with the PsyQ names,
both rebuilt on each run, and the agent's names win over them.

The names files are also every view's names. An address argument (run's
trace ranges and watches, samples, disasm, xrefs, decompile, ram_view)
takes a function's or global's name as well as hex: world_collide,
croc+0x370, RACE.OVL:race_update when overlays share a name. Traces,
watch hits, calls, disasm, xrefs, coverage and ram_view rows print them,
a function's where its bytes are the inventory's (body_match) and a
global's as name+0xOFF within its type. symbols adds other symbol files.

When reading a function leaves doubt about what it does, write it as C
and check it: checked C is a way to be sure of a rule, not a step a
feature needs. The agent's C for a function goes in decomp/<name>.c, including
"names.h": every struct of the names files at its offsets (each offset
and size asserted), the globals and every function's prototype, so the C
reads the game's data and calls the original's functions by name.
code_check function builds decomp/ with a MIPS GCC (context's mips_gcc)
and replays the captured recordings that reach the function (by the
coverage kept beside each; scenario and recording choose others, state
runs the game from a state instead). Calls to it spread over those
recordings (cases, 8 by default; 3 from a state) each run as the original
and as the C, from the same moment, and their effects are compared: the
return value, sp and the registers a call keeps, the bytes left stored
(named by the globals, or the pointer arguments' fields, that hold
them), the GTE's registers, hardware writes and the calls made with
their arguments. Time is not compared. Each call is also run varied
(varied, 2 by default): its integer arguments and the integers its
pointer arguments point to set at random within their types in names/,
to reach what no recording does; an enum's are set to its values only,
so an argument the game passes from a few (a slot, a mode) is an enum.
varied_dropped counts those the original did not return from, calls the
game could not make. A call's arguments are compared as many as its
callee's signature in names/ takes (all four without one), and the 16
bytes above sp, where a function may store its argument registers, are
its own.
branch_ways lists the original's branch ways no case took. A failed
case is saved as a state (.psx/decomp/cases/); code_check from it
reruns that call. A check holds until its function's C changes, or the
C of the decomp/ functions it calls, decomp/'s headers or the names that
C uses; with no function, code_check checks each function whose check no
longer holds. Compiler errors
come back by file and line.

decomp= on run, scenario_check and port_check runs the game with the C in
place of the functions it defines for the whole run (decomp=[] those
whose last check passed on this C and names, ["all"] every one, or a
list; on the CLI --decomp, --decomp=all, --decomp=a,b). An overlay's
function is replaced only while its entry holds what its check saw.
The result names the functions replaced with their calls, those left out
and why, and share_in_c, the instructions run in the C. scenario_check
and port_check replay each recording with it: diverges_with_c, with
the quantities that differ (differs_in), is a recording that replays as
recorded without the C and not with it: a function it replaces does not
do what the original's does there.

port_check gives a failing feature a pointer, worked out on one of its
recordings replayed on the original, at the first difference: for each
quantity that differs, the bytes the binding reads for it (reads), the
functions that stored into them just before (writers; written_before
where none did lately), and the code run there for the first time in
the recording (first_run, called_first when the function itself is
new). Each function comes with the features whose code lists it and its
C (checked, unchecked, none), and with_c says whether the recording
replays with the checked C. evidence reads it: another feature's code
not done (the features depend on each other), this feature's code with
no checked C (the rule is not understood yet), or checked C that
replays it (the rule is known there: the port does not follow it, or
what is around it differs). port next's Fix carries it. It is a place
to look, not the moment: debug_start on the recording finds that (a pc
stop at a writer, a watch on reads).

fixes/ holds C that changes what the game does on purpose, most often so
the game takes its input every tick where its driver delays it
(late_input, topic run; port next asks for it): a function there replaces the game's function of that name (in the executable, not
an overlay) in every recording, play and check, as the fix adjustment
(topic run). It builds like decomp/, with "names.h", and is not checked
against the original, since it differs from it; scenario_adjust_check
with adjust fix measures what it changes. Each recording keeps the build
it was recorded with and replays with it; scenario_check gives
fix_changed for a recording whose fixes/ has changed since, to record
again.

Functions reached only through a table (state handlers, object updates, a
jump table's cases) are often missing from an analysis of the EXE. Seed one
from RAM:
1. Find the table: a trace's "-> target (from pc)" shows the jump into one
   handler; code_xrefs on that handler gives the data word holding it.
2. ram_view fmt=x32 from the base: entries are code addresses; the first
   word that is not one ends the table.
3. code_functions with ram, range (the code holding them) and entry once
   per table entry, gp from the run's registers; decompile them there.

## The inventory and coverage

code_inventory lists every function of every executable on the disc and of
the overlays in RAM snapshots (traced runs), with blocks and instruction
counts. Take it before recording: recordings credit their coverage to it.

code_coverage reads traced runs and recordings (.jsonl): per function, the
blocks run and not, and branch_ways_missed. GCC's division checks (the
branches to break 6 and break 7) are left out. A missed way gives what the
branch tests, read back through the code before it ([0x8007529C] == 2,
([arg0 + 0x178] & 0x2) == 0 in 0x80043234), the outcome no run saw, and
the blocks only that way reaches; a branch behind it that never ran is
counted under branches_behind_missed_ways, not listed. With function=
(an address or a name) it gives that function's C from Ghidra instead,
each line with how often the runs ran it and each condition marked never
true or never false. port next groups a feature's open branches by test
and outcome, and a wont_reach key may be a test as it gives them: it
answers for every branch of the feature's code testing it.
code_coverage_diff runs=[...]
against=[...]: only lists the functions only the first set ran, further the
blocks and branch ways (taken, fell through) only they took in functions
both reached. To find a feature's code, diff runs or recordings doing it
(walking, a jump) against runs from the same state not doing it; more
against runs (other actions) leave out more shared code. Swap the sets for
what the feature skips.

## Scripted rules

When the rules are a program the game's own interpreter runs (bytecode,
tables, a tree of nodes), coverage of the machine code shows only the
interpreter: its if is one branch for every if of every script. Then the
glossary's binding names the program and, when needed, points of the
interpreter, and psx keeps the script's coverage with each recording
(psx-capture's crate docs show the methods):

- programs(p, m): p.program(name, address, bytes) for each program
  running, read where a recording starts and when each tick ends. The
  bytes the game reads of it count as run; stretches never read are gaps.
  Name each program by a name that means the same program in every run.
- hooks(h): h.at(pc) runs once the instruction at pc has run, h.branch(pc)
  once the conditional branch at pc has gone, after its delay slot.
- hook(at, m, s): from the registers and memory as they are, s.site(name,
  position) for the position running and s.way(name, position, way, of)
  for a decision's way (a switch of five cases is one decision of five
  ways). A position is an offset into the program, or whatever names it
  the same way in every run.

- map(name, code, map): once you can decode the script, the program's
  structure from its bytes: map.statement(start, end) for each statement,
  map.decision(at, ways) for each that decides, map.routine(name, start,
  end) for names. Asked once per program in a recording, and kept beside
  it (positions and names only).

Reads need only where the program is; hooks give each decision's ways,
and cover programs that are not bytes in one place. With a map, coverage
counts statements run (a byte read or a site named) instead of bytes
read, so tables and skipped operands are no gaps, and a decision that ran
with no hook naming its way is a gap until one does. Positions and the
map are psx's only knowledge of the script: reading it is yours. A
recording made before the map changed is made again.

A catalogue entry's code lists stretches of a program beside functions,
name+0xSTART..0xEND (end excluded), and wont_reach takes a position
(name+0xAT) or a stretch inside them. code_coverage and code_coverage_diff
on recordings report programs: stretches read, sites and ways (with a
map, statements run and routines entered), and in a diff the ones only the
first set ran, which locate a feature's part of the script.

## Compressed code

The inventory reads an overlay no disc file holds only around the code that
ran (bounded_by found_code), and coverage counts only that (code_not_held);
port_next gives Unpack. Read it whole from the decompressed bytes:
1. watch one of its addresses: the writer's pc is the decompressor;
   code_disasm it and its callers for the format and the disc data read.
2. Decompress that data into a directory with an unpacked.json:
     {"files": [{"file": "menu.bin", "source": "/DATA.VOL",
                 "offset": 81920, "length": 23104}]}
3. code_inventory with unpacked DIR and the run's ram. Its unpacked list
   says per file whether RAM held its code (in_ram); only bytes RAM held
   are read, so a wrong decompressor adds nothing.

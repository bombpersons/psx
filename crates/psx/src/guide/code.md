Start from an observation: a sampled value, a writer's PC, a read, a CD
load. Ghidra's names and pseudocode, and every heuristic here, are
hypotheses to check against the instructions and what runs do (MIPS delay
slots, GTE operations).

## Finding things in RAM

ram_search keeps the addresses whose values satisfy pred between
consecutive snapshots (changed, unchanged, increased, decreased, delta=N;
one for all pairs or one per pair), exact values per snapshot ("441,*,376")
or a between range; save writes the matches and candidates narrows a later
search to them. ram_view reads a struct across snapshots; ram_diff
clusters nearby changes between two.

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
gp. A project open in the Ghidra GUI is an error until closed. A query cut
short (a deadline, a killed psx) is recovered by the next one; a lock left
by a dead process (psx.lock under .psx/ghidra/) is removed by hand only
after checking its pid is gone. code_annotate applies names and comments
and exports them as symbols for run, disasm and xrefs. code_symbols names
PsyQ library code by signature, not the game's own functions.

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
blocks run and not, and branch_ways_missed. code_coverage_diff runs=[...]
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

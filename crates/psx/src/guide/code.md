Start from an observation: a sampled value, writer PC, read access or CD load.
run supports sample, watch, watch_reads, trace and trace_cd.
loads maps recorded CD DMA transfers to disc files and checks final RAM residency;
it does not follow every later CPU copy or decompression into its final destination.

disasm requires source (a host EXE or disc.cue::/PATH), ram (run directory or
ram.bin), or both. text:true avoids duplicate instruction JSON. function selects
a heuristic function extent; read its boundary evidence, stop reason and
truncation warnings. function_budget limits discovery; count limits the listing.
xrefs finds static callers/references, with limited indirect-reference coverage.
listing.next_addr continues a disasm listing. xrefs accepts a hex address or a
LO-HI range (a whole struct) and also reports aligned data words equal to the
address (pointer/jump tables; no_data skips them); give gp for RAM-only scans.
An address built in main RAM's KUSEG mirror (lui 0x8 + addiu for 0x0008xxxx)
counts as the same address.
A busy global (an input word, a flags word) has hundreds: kind keeps loads or
stores, and bits=MASK keeps the loads whose value is tested with those bits
(andi, or and with a constant), each with the bits it tests. Loads that
compare the whole word (any button at all) are not bit tests.

ram_search keeps addresses whose values satisfy pred between consecutive
snapshots (one predicate for all pairs or one per pair), exact values per
snapshot ("441,*,376") or a between range. save writes matches and candidates
restricts a later search to them, narrowing over rounds. ram_view reads a struct
across snapshots; ram_diff clusters nearby changes between two snapshots.

ghidra_decompile takes source plus addr and imports/analyzes automatically.
Reuse its analysis handle for further functions, references and pseudocode.
For runtime overlays, supply ram with explicit ranges and entries, and gp when
known. Same-analysis queries queue within deadlines; a project open in the
Ghidra GUI is an error until it is closed. A query interrupted (its psx
killed, or a deadline) is dealt with by the next query on that analysis,
which stops what is left of it; its result says so under recovered. Leave
lock files alone: removing one by hand loses that record.

Functions reached only through a table (per-state handlers, object update
routines, a jump table's cases) are often missing from an analysis of the
EXE: nothing calls them directly. Seed a RAM analysis with the table:
1. Find a table. A trace's "-> target (from pc)" lines show the jump into
   one handler; xrefs on that handler's address lists the aligned data word
   holding it, which is inside the table. Or decompile the dispatcher: a
   call through table[state] names the base.
2. Read it from a run's RAM with ram_view fmt=x32 from the base: entries are
   code addresses (80xxxxxx inside the code range), and the first word that
   is not one ends the table. The state or kind numbers index it.
3. ghidra_analyze with ram (that run's directory), range (the code range
   holding the entries: the EXE's text, or the overlay's) and entry once per
   table entry; gp comes from the run's registers. The new analysis has the
   handlers as functions, named by entry address, and what they call;
   decompile them there.

symbols names matching PsyQ library code, not every game function. Low coverage
alone does not prove a bad match; ambiguous signatures are skipped. Ghidra names
and pseudocode are hypotheses to check against instructions and runtime behavior,
especially MIPS delay slots and COP2/GTE operations. Use ghidra_annotate and
ghidra_export_symbols for named findings; exported body-bound names can be checked
against resident bytes in native traces. An address alone cannot identify overlays.

Decompressed code. inventory reads an overlay no file holds only around the
code found in it (bounded_by found_code). When such code was decompressed
from the disc, give inventory the decompressed bytes to read it whole:
1. Find what wrote it: run with watch on one of its addresses gives the
   writer's pc; disasm it (and what calls it) for the format and the disc
   data it reads.
2. Write a decompressor and decompress that data into a directory with an
   unpacked.json:
     {"files": [{"file": "menu.bin", "source": "/DATA.VOL",
                 "offset": 81920, "length": 23104}]}
   (file holds what length bytes of source at offset decompress to).
3. inventory --unpacked DIR --ram RUN ... An unpacked file names the code
   RAM holds as its bytes, like a disc file, and is read over that extent.
   The inventory's unpacked list says per file whether RAM held its code
   (in_ram, and the overlays it names). One that did not was decompressed
   wrongly, holds no code, or no run loaded it: only bytes RAM held are
   read, so a wrong decompressor adds nothing.
The ledger's partly_read_overlays groups such overlays by stretch of RAM
(those that overlap or touch), and protocol next gives the investigator each
stretch not yet settled (unpack_overlays), before coverage. A stretch is
settled when the inventory reads its code whole, or when notes/overlays.json
explains why no disc file holds it (built at runtime, say):
  {"regions": [{"start": "80010000", "end": "8002EE98",
                "why": "written by 80012340 from a table it builds"}]}
An entry settles each stretch inside start..end. Name the writer and what
it read: the why is a claim, checked like any other.

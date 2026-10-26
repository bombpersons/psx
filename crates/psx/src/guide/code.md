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

symbols names matching PsyQ library code, not every game function. Low coverage
alone does not prove a bad match; ambiguous signatures are skipped. Ghidra names
and pseudocode are hypotheses to check against instructions and runtime behavior,
especially MIPS delay slots and COP2/GTE operations. Use ghidra_annotate and
ghidra_export_symbols for named findings; exported body-bound names can be checked
against resident bytes in native traces. An address alone cannot identify overlays.

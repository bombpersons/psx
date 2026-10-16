Start from an observation: a sampled value, writer PC, read access or CD load.
run supports sample, watch, watch_reads, trace and trace_cd on the native backend.
loads maps recorded CD DMA transfers to disc files and checks final RAM residency;
it does not follow every later CPU copy or decompression into its final destination.

disasm requires source (a host EXE or disc.cue::/PATH), ram (run directory or
ram.bin), or both. text:true avoids duplicate instruction JSON. function selects
a heuristic function extent; read its boundary evidence, stop reason and
truncation warnings. function_budget limits discovery; count limits the listing.
xrefs finds static callers/references, with limited indirect-reference coverage.

ghidra_decompile takes source plus addr and imports/analyzes automatically.
Reuse its analysis handle for further functions, references and pseudocode.
For runtime overlays, supply ram with explicit ranges and entries, and gp when
known. Same-analysis queries queue within deadlines; foreign/GUI locks remain
errors. Do not remove another analysis owner's lock to make a query succeed.

symbols names matching PsyQ library code, not every game function. Low coverage
alone does not prove a bad match; ambiguous signatures are skipped. Ghidra names
and pseudocode are hypotheses to check against instructions and runtime behavior,
especially MIPS delay slots and COP2/GTE operations. Use ghidra_annotate and
ghidra_export_symbols for named findings; exported body-bound names can be checked
against resident bytes in native traces. An address alone cannot identify overlays.

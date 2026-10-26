In a clean-room port the investigator reads the original's code and the
implementer never does: only specs (behaviour in game terms) and cases
(observations) cross to it. barrier check looks at what is about to cross,
files or a message, for what would carry the original's code with it:

- address: a PlayStation address (80xxxxxx or A0xxxxxx in main RAM,
  1F80xxxx scratchpad and hardware), with or without 0x;
- hex_constant: any 0x number. Say the value as observed, in decimal or
  in game terms (flag "active", not 0x4000);
- code: a C-like statement (an assignment or call ending in ;, if or
  while with a condition and a brace, ->, +=, <<, ++, && outside tables),
  MIPS registers and instructions, decompiler output (FUN_..., undefined4)
  and code blocks in a programming language;
- tool: the reverse-engineering tools (Ghidra, disassembly, decompiling,
  the debugger and its breakpoints) and the executable's serial
  (SLUS_005.67);
- symbol: the original's names from psx's own analysis: Ghidra function
  names other than its defaults (the investigator's renames included) in
  .psx/ghidra, the psx symbols output (PsyQ library names) in
  .psx/symbols, and symbols=FILE (.symbols.json or .sym). Plain lowercase
  words (open, main, strlen) are not matched;
- name: name=WORD, a name of the original's to refuse (the executable's
  file name, TOMB2.EXE);
- pointer_column: in a CSV, a column whose values are addresses (a
  pointer sampled by mistake);
- copied: a run of eight or more words shared with the investigator's
  notes (notes=DIR), unless the same words are already published
  (published=DIR). Checked in prose (.md, .txt, messages), not in JSON or
  CSV, whose data the notes' recordings share by nature.

  barrier check specs --notes notes --published ../port/specs \
      --name TOMB2.EXE

The result lists each finding by file, line, kind and the text found, and
exits 1 unless it is clean; allow=KIND lets one kind through (counted in
allowed). Rewrite a finding in game terms and check again.

The checks catch addresses and code-shaped text, not a routine paraphrased
in plain words: a reviewer who knows the code still reads what crosses.

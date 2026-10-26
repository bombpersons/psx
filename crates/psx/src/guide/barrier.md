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

Publishing: nothing crosses without the reviewer's approval of exactly
those bytes.

  publish stage --to ../port --name TOMB2.EXE [PATHS] [--state NAME]
  (the reviewer writes reviews/HASH.json)
  publish cross HASH --to ../port --reviews ../review/reviews
  publish check --to ../port --reviews ../review/reviews

- stage takes what differs in specs/ (specs, the glossary, cases with
  their observations CSVs, and specs/states/NAME.json, the description of
  each state=NAME from its chain) from what has already crossed. With
  PATHS (files or directories under specs) only those; without, all of
  specs, and files gone from it are removed on the other side. A case
  with "kept_back": true stays behind with its observations. The changed
  files get the checks above, with the notes defaulting to notes/ and the
  port's specs/ and questions/ as published text; any finding refuses
  the stage (allow=KIND lets a kind through, and the bundle lists each
  one for the reviewer). A clean stage writes .psx/bundles/HASH/:
  files/ and bundle.json. HASH covers each file's path and sha256 and
  the sha256 of the version it replaces.
- The reviewer reads the bundle and writes reviews/HASH.json in its own
  repository: {"bundle": "HASH", "verdict": "approve" or "reject",
  "reasons": [...]}. Reasons stay on the investigator's side.
- cross copies the bundle into the port's specs/ only when that review
  approves it and the bundle's files still hash as staged (status
  awaiting_review, rejected with the reasons, or stale when another
  bundle replaced the same files first: stage again). It appends to the
  port's notes/published.jsonl the bundle, the review's sha256, the files
  and their sha256 and the sha256 of the entry before, and lists
  replaced_unapproved: files that had arrived another way and that it
  overwrote.
- check, and every cross, tests the invariant: the port's specs/ is
  exactly what the logged bundles add up to, with each one's review still
  there, still approving and unchanged, and the log's chain intact. The
  result lists unapproved files (changed after crossing, or carried by no
  bundle), missing ones, reviews and log breaks; it exits 1 unless the
  invariant holds.

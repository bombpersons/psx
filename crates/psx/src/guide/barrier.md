A clean-room port has three repositories side by side, one per agent:

  port init PORTS --media "media/Game.cue" [--game NAME] [--fields-per-tick N]

writes PORTS/investigate (the investigator: AGENTS.md for the role, specs/
with its rules, glossary and index, notes/provenance.json, notes/LOOP.md,
notes/recordings/ and a git-ignored notes/explore/, samples.toml, the disc
and BIOS linked into media/ and bios/, protocol.toml naming the other two
repositories and the disc's executables, and .mcp.json for psx),
PORTS/clean (the implementer: psx's Bevy workspace with its own AGENTS.md
and architecture rules, guidance written for working from specs and cases,
questions/, notes/, and data/: every file on the disc but SYSTEM.CNF and
the executables) and PORTS/review (the reviewer: AGENTS.md, reviews/,
audits/, .mcp.json). Each gets .claude/settings.json: the implementer's
denies psx, the web, the other repositories, psx's own folder, the disc's
folder, the host's session folders for the other agents, and edits to
.claude/, notes/ and specs/, and runs barrier hook on every message (its
.claude/barrier.json names the investigator and the executables); the
investigator's and reviewer's deny the web and writing into the others'
repositories. git init runs in each. The game's name defaults to the disc
image's file name. With protocol.toml, publish needs no to or reviews and
refuses the executables' names. template create DEST --role ROLE writes one
of the three alone, with the others assumed as siblings (investigate,
clean, review) unless given. port adopt adds these files to repositories
that already exist, overwriting nothing (guide topic cleanroom, Migrating).

The person starts each agent's session in its own folder; an
investigator never starts the others', since a session it starts would
share its notes.

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

The message check: what the person types to the implementer passes the
same checks before the agent sees it. barrier hook is the implementer's
UserPromptSubmit hook (CLI only; the host runs it, not the agent), so its
.claude/settings.json needs one line:

  "hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command",
      "command": "/path/to/psx barrier hook"}]}]}

Settings are in the implementer's .claude/barrier.json, paths relative to
its repository:

  {"investigator": "../investigate", "names": ["TOMB2.EXE"],
   "published": ["specs", "questions"], "log": "notes/guidance.jsonl",
   "confirm_prefix": "[cross]"}

investigator gives the symbol names from its .psx analysis and its notes/
for copied text; notes, symbols (files) and allow (kinds) are also taken,
and the first hook's dirty_paths and published_paths still work. Each
message is logged as guidance (nothing found), stopped (exit 2: the person
is told why and pointed to the investigator, and the log keeps only the
kinds found, never the text) or crossing (the message starts with
confirm_prefix: it goes through, and the agent is told to write it up as
a question before acting on it). Each log entry carries the sha256 of the
line before it, so a rewritten history shows. If the check cannot run (no
settings, a bad event) the message is stopped. Deny the implementer
.claude/ and the log in its access rules.

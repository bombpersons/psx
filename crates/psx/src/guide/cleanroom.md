A clean-room port keeps the people (or agents) who read the original's code
apart from the people who write the port's code. It is a common practice for
reducing the risk of copying an original's code; what it protects and how
strict it must be is a question for a lawyer before release, not for psx.

## The roles

Three roles, each in its own repository, held for the whole project. Whoever
has read the original's code is an investigator for good.
- The investigator reads the disc, the emulator, disassembly, decompiler
  output and RAM, and writes specs, case files and answers to questions.
- The reviewer reads everything, the original included, and approves or
  rejects each bundle the investigator publishes. Its only output to the
  implementer is that approval. It never writes specs.
- The implementer reads the specs, the cases, the game's data files and its
  own code, and writes the engine, the game, the importers and the case
  runner. It never sees the executable, disassembly, decompiles, RAM dumps,
  emulator states, psx or the investigator's notes.

The roles are enforced by access, not promises: psx port init writes each
repository with its access rules (the implementer's deny psx, the web, the
other repositories and the disc's folder), and publish carries specs across
only as the reviewer approved them (guide topic barrier).

## What may cross, and what may not

Across, only through an approved bundle:
- behaviour in game terms ("while hanging, left moves along the edge 16
  units a tick until the edge ends or a wall blocks");
- observed values as data: speeds, timings, table contents measured at
  runtime, with how each was observed;
- recordings as case files, start-state descriptions, screenshots;
- file formats as field tables.

Never across:
- disassembly, decompiler output, pseudocode, or prose that follows the
  original's control flow, statement order or function boundaries;
- addresses: which routine an observation came from goes in the
  investigator's notes/provenance.json, keyed by spec id;
- structure mirroring the original's: specs are organised by behaviour as a
  player would describe it, not one per function;
- table contents from the executable (below);
- the original's identifiers, except names coined for the glossary.

## The disc

The implementer needs the data files (levels, sounds, video, fonts) to
write and test importers, never the executable or its overlays. port init
gives it every disc file but SYSTEM.CNF and the PS-X EXEs, in data/, which
it never commits. A data file can still hold code (some games keep level
code in level files): the implementer reads such a file only as far as a
format spec describes it. Screenshots of the original reach it as fixtures.
The shipped game imports from the player's own disc on first start.

## Tables in the executable

Behaviour data kept in the program rather than in data files (weapon
statistics, sound tables, state tables) is not copied into specs or source.
The spec gives its layout and where it is found in terms the implementer
can use on the player's disc (the boot executable, a fixed offset, a
signature); an importer reads it on first start, as it reads levels, and
the implementer writes that importer from the layout alone.

## Small algorithms

Some behaviours are nearly all expression: a random number generator, a
fixed-point square root, a hash. Their specs give observable outputs, a test
vector long enough to pin the behaviour down (thousands of values), not the
formula; the implementer finds a method that matches. When a table is plain
mathematics, say so as mathematics: a sine table that is round(4096 sin x)
for x in steps of a circle, checked against the original's table entry by
entry, is a fact about sines, not the original's expression.

## Questions and choices

The implementer cannot look at RAM, so a divergence it cannot explain
becomes questions/Q-NNNN.md in its repository (what was expected, what the
port did, which case). Where a spec leaves something open and it chooses,
the choice goes in questions/CHOICES.md. The investigator reads both
directly and answers by changing a spec or adding cases, then publishing;
never by explaining the original's code. The log is part of the record.

## People and agent hosts

A person who plays handoffs, reads both sides or steers both agents relays
only specs, cases and questions between them. The implementer's message
hook (barrier hook) checks what a person sends it and logs every message.

An implementer's session is never started from, forked from or continued
from an investigator's: hosts share context between related sessions. Each
agent works in its own folder, so memory and transcripts kept by path are
not shared, and the implementer's access rules deny the other agents'
session folders. The person starts each role's session in its own folder.

## Records

Each repository keeps its access rules and instructions under version
control; commits carry a Role: trailer (and Spec: for the implementer); the
question log, the reviews, the publishing log (notes/published.jsonl) and
the guidance log (notes/guidance.jsonl) are kept. Together they show who
saw what, and when.

## Migrating a port that predates the protocol

Most of an existing port's work is investigation: recordings, measured
values, notes. That stays valid on the investigator's side. What is
tainted is port code written by someone who read the original's code, and
that is the cheaper part to redo once specs and cases exist.
1. Triage every module: keep (not derived from the original: a template,
   ticks, input, the console's look, menus, mod loading); audit (written
   from facts, such as format parsers and an editor over the port's own
   formats; the reviewer checks it against the format specs); rewrite (game
   behaviour, and engine code whose notes cite an original routine).
2. The investigator converts notes into specs: each splits into a spec and
   notes that stay behind the barrier. Where a spec is thin, existing
   recordings usually answer it.
3. Fixtures and their test definitions become case files (case record, or
   by hand in its form); the existing tests become the implementer's bar.
4. Create the clean repository new (port init), not as a branch: the old
   history holds the tainted code. Publish the specs; copy in the kept and
   audited code.
5. Reimplement one system at a time, starting with what most cases cover.
   The old port is the reference until the clean one passes the same cases.

Auditing legacy behaviour code in place is not an option: an author
reviewing their own code does not make it clean.

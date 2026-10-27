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
  emulator states, psx (but for clean mode, below) or the investigator's
  notes.

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
directly (it cannot write there) and answers by changing a spec or adding
cases, never by explaining the original's code. It says what changed in
specs/answers/Q-NNNN.md, or under the choice's number in
specs/answers/CHOICES.md, and publishes them together: the answer crosses
and is reviewed like a spec, the ledger counts the question answered once
it has crossed, and the implementer's NEXT.md lists it. The log is part of
the record.

## People and agent hosts

A person who plays handoffs, reads both sides or steers both agents relays
only specs, cases and questions between them. The implementer's message
hook (barrier hook) checks what a person sends it and logs every message.

An implementer's session is never started from, forked from or continued
from an investigator's: hosts share context between related sessions. Each
agent works in its own folder, so memory and transcripts kept by path are
not shared, and the implementer's access rules deny the other agents'
session folders. The person starts each role's session in its own folder.

## Clean mode

psx-clean, built beside psx, is the one part of psx the implementer
reaches: an MCP server (port init names it clean in the implementer's
.mcp.json, allowed by its access rules while psx stays denied) that runs
the original from what the investigator published and answers only in
published names. It has no command that shows code, memory, addresses,
traces or states; it calls psx on the investigator's repository, keeps its
working files there, and passes on neither psx's output nor its messages.
- next: the ledger brought up to date and notes/NEXT.md returned.
- record: an exploratory case (role exploratory, start_state naming its
  start) from a published start (specs/states/), any input for a
  bounded number of fields ([clean] max_frames in protocol.toml, default
  18000), the subject and samples by published names (the glossary's terms
  and the crossed cases' subjects and columns), written to the
  implementer's notes/oracle/. Unpublished fields of the subject, and
  columns whose values look like memory addresses, are left out. The
  port's runner replays it unchanged; it is evidence for the implementer
  only and never backs a spec.
- compare: the port's run of a case (write_run's CSV) against the
  original's recording: the first row and column that differ, by how much.
- heldout: the port's case runner on the cases kept back ("kept_back":
  true) for crossed specs, run with PSX_CASES and PSX_RESULTS set (the
  template's cases_from_folder test) under bubblewrap: the port's
  repository and cargo's home as overlays whose writes are discarded, the
  other repositories hidden, no network. Only pass, fail, waiting or
  no_result per case comes back, with its spec and a label (the case
  file's hash, cut short), never the runner's output: whether the spec
  was enough, on cases the implementer never saw. Without bubblewrap it
  runs unsandboxed and says so, and the audit marks it.
- screens: the original's pictures by row, side by side with the port's.
- template_update: psx template update on the implementer's own
  repository, which touches only the template's code (psx-bevy/, vendor/)
  and carries nothing of the original; the result lists what changed and
  what to merge.
So the investigator publishes, for clean mode, the names it may ask by in
the glossary (units and meanings, never the original's identifiers; the
field map's names are those names) and start states with descriptions
(publish stage state=NAME). Every request goes into the implementer's
notes/oracle.jsonl, each entry with the sha256 of the one before; the
implementer cannot edit it, and the audit checks its chain and lists the
requests.

## Records

Each repository keeps its access rules and instructions under version
control; commits carry a Role: trailer (and Spec: for the implementer); the
question log, the reviews, the publishing log (notes/published.jsonl) and
the guidance log (notes/guidance.jsonl) and the clean-mode log
(notes/oracle.jsonl) are kept. Together they show who
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
   audited code. The implementer is a session that has never seen the old
   port or the original's code, started fresh in its own folder.
   Repositories set up by hand before psx had the protocol's files take
   them with port adopt:

     port adopt --investigator INV [--implementer IMP] [--reviewer REV]
                [--legacy OLD]... [--media DISC] [--dry-run]

   It writes only what is missing (the role files, protocol.toml, .mcp.json
   with psx or clean mode, the reviewer's repository if there is none, data/
   if there is none), adds to JSON settings what they lack (deny rules,
   clean mode, the message check's settings) and overwrites nothing; where
   a repository already has its own message check or AGENTS.md, it keeps
   them and says so. Each --legacy port is denied to the implementer
   (with its sessions' folders), listed in protocol.toml's [legacy], and its
   notes/ and docs/ join the text the message check refuses. It ends with
   what only a person settles: the field map, the start states to
   publish, the case runner's cases_from_folder, the legacy triage.
   Running it again changes nothing, unless the repositories moved (guide
   topic barrier, Moving a port). Its legacy ports default to
   protocol.toml's.
5. Reimplement one system at a time, starting with what most cases cover.
   The old port is the reference until the clean one passes the same cases.

Auditing legacy behaviour code in place is not an option: an author
reviewing their own code does not make it clean.

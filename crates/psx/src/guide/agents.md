port run, run by the person in the investigator's repository, starts each
role's agent when it has work, so nobody relays "the bundle is ready" by
hand. It computes the ledger, picks the next role in turn (investigator,
reviewer, implementer) whose first item of work does not wait on another
(wait_for_..., nothing, awaiting_review, hold), and runs that role's
harness once, headless, in its repository and its sandbox (below), with a
prompt naming the item. A role with no [agents] entry is the person's to
run: port run leaves it, and says so when only it has work.
When the agent exits it computes the ledger again. It stops when no role
has work (protocol report then says what waits on the person), when a run
changes nothing (the role's work, its commit and its working tree are as
before: a stuck agent is not started again), when a run fails (its
harness exited with an error or was killed, it ran out of time, or its
stream has no successful end: usage ran out, say), or after max_runs runs.
The role's next session, in this port run or a later one, is told what a
failed one left uncommitted, ahead of its item. One
port run at a time; --dry-run says what it would run; --follow, from
another terminal, shows the current run as it goes.

Each run is a fresh session doing one item: what a role knows between
runs is in its files (notes, LOOP.md, NEXT.md, the specs and git), as the
roles' instructions already ask, so no conversation grows long and no
compaction is needed. The prompt tells the agent so.

The harness is a command line per role in protocol.toml (port init and
port adopt write an example, commented out):

  [agents]
  max_runs = 10             # per port run (default 10)
  timeout_minutes = 120     # a run is stopped after this long (default none)
  sandbox = true            # each session in bubblewrap (default)

  [agents.implementer]
  command = ["claude", "-p", "{prompt}", "--model", "{model}",
             "--permission-mode", "bypassPermissions",
             "--output-format", "stream-json", "--verbose"]
  model = "claude-sonnet-5-5"
  prompt = "..."            # optional: replaces psx's ({work} is the item and its guide topics)
  share = ["~/.config/x"]   # optional: from the person's home, writable
  share_ro = ["~/data"]     # and read-only (the implementer's sandbox)

{prompt} is the prompt, {model} the model, {role} and {repo} the role and
its repository. Any harness with a headless mode serves (claude -p, codex
exec, gemini -p, ...); it must edit and run commands without asking, and
load the repository's MCP server (port init enables it in each
.claude/settings.json; a harness that does not read .mcp.json needs psx's
or clean mode's server in its own configuration).

## The sandbox

Each session runs in bubblewrap (Linux), so its limits hold whatever the
harness is and whatever its own access rules allow:
- the implementer sees the system's folders, its own repository, the Rust
  toolchain (cargo's downloads writable), the programs on its PATH from
  the home folder, the harness's own program, git's identity, a home of
  its own and what share and share_ro list. The other repositories, the
  disc, psx's folder and the person's home are not there, and psx is
  masked wherever the sandbox has it. Its notes/, specs/, .claude/,
  .mcp.json and git's config and hooks are read-only;
- the investigator and the reviewer see everything and write their own
  repository and the person's home (their harness's login and
  transcripts), never the port's other repositories or psx (its programs,
  and its source when built from one).
  Their .claude/, .mcp.json, protocol.toml (whose commands port run runs
  outside) and git's config and hooks are read-only.
All three get a fresh /tmp, their own process space and the network,
which the harness needs for its model's service (the web stays denied by
the access rules alone).

psx and clean mode need what the sandbox keeps out: publish cross writes
to the implementer, and clean mode runs the original from the
investigator's repository. So their servers run outside, started as the
role's .mcp.json says for each connection, and psx mcp or psx-clean mcp
inside the sandbox passes its messages through (a socket at
/tmp/psx-mcp/), whatever harness started it. The servers' errors are in
.psx/port-run/NAME.mcp.log. psx's own commands run inside the investigator's
sandbox too, but those that write to the others work only through its
server. The implementer's message hook (barrier hook) does not run inside,
where psx is not; port run's prompts are psx's own.

The implementer's home is ~/.local/share/psx/agents/PORT/ (PORT from the
implementer's folder): its harness's settings, login and transcripts, kept
from the person's. Log its harness in there once:

  psx port run --shell implementer

opens a shell in the implementer's sandbox, as its sessions have it (its
server too); run the harness there and log in, or look around. An API key
in the environment passes through. sandbox = false runs the sessions
with only the harness's own access rules.

What each run did is in the investigator's .psx/port-run/: NAME.txt as the
person reads it (the agent's text, its tool calls, how it ended, and the
commits it made), NAME.log the harness's raw output, and runs.jsonl a
record per run (role, item, command, exit, why it failed, the tokens it
used as its harness counts them, seconds, commits, uncommitted
files, whether it made progress, the item after). Claude Code's
stream-json, Codex's exec --json and Gemini's stream-json are read into
lines; other output is shown as it is. The harness keeps the session's own
transcript, to read or continue by hand.

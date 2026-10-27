port run, run by the person in the investigator's repository, starts each
role's agent when it has work, so nobody relays "the bundle is ready" by
hand. It computes the ledger, picks the next role in turn (investigator,
reviewer, implementer) whose first item of work does not wait on another
(wait_for_..., nothing, awaiting_review, hold), and runs that role's
harness once, headless, in its repository, with a prompt naming the item.
When the agent exits it computes the ledger again. It stops when no role
has work (protocol report then says what waits on the person), when a run
changes nothing (the role's work, its commit and its working tree are as
before: a stuck agent is not started again), or after max_runs runs. One
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

  [agents.implementer]
  command = ["claude", "-p", "{prompt}", "--model", "{model}",
             "--permission-mode", "bypassPermissions",
             "--output-format", "stream-json", "--verbose"]
  model = "claude-sonnet-5-5"
  prompt = "..."            # optional: replaces psx's ({work} is the item)

{prompt} is the prompt, {model} the model, {role} and {repo} the role and
its repository. Any harness with a headless mode serves (claude -p, codex
exec, gemini -p, ...); it must edit and run commands without asking, and
load the repository's MCP server (port init enables it in each
.claude/settings.json; a harness that does not read .mcp.json needs psx's
or clean mode's server in its own configuration). Until psx sandboxes the
sessions, the access rules in .claude/settings.json are their only limits.

What each run did is in the investigator's .psx/port-run/: NAME.txt as the
person reads it (the agent's text, its tool calls, how it ended, and the
commits it made), NAME.log the harness's raw output, and runs.jsonl a
record per run (role, item, command, exit, seconds, commits, uncommitted
files, whether it made progress, the item after). Claude Code's
stream-json, Codex's exec --json and Gemini's stream-json are read into
lines; other output is shown as it is. The harness keeps the session's own
transcript, to read or continue by hand.

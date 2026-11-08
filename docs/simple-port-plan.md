# Plan: a smaller psx for ports

psx now tells an agent what to do next and decides when it is done: `port
next`'s twenty-odd kinds of item, the catalogue's gates, five goal states,
three shares. That machinery was built so an agent could port a game alone,
for someone who doesn't know how.

The Tomb Raider 2 run (`tr2-rs`, October) had none of it. In a week of
prompts that were mostly one-line pointers in a direction, it reached 12
handoffs, 221 tests against about 650 captured files, and about 57,000 lines
of Rust. Its accuracy came from comparing against the original. Its direction
came from a person who could see what was missing. Its bookkeeping was a
status table in the README and a TODO file.

The published work on LLM ports says much the same: give the agent a done
that a machine can decide, then loop. The long tail stalls whatever the
orchestration, and what helps there is better signals and fresh context.

So psx keeps the signal and gives up steering.

## Who does what

- **psx says what is right.** It records the original, replays recordings
  against the port, and gives the first difference and who wrote the value.
- **The person gives direction.** The agent proposes what's next and why;
  the person nods or points elsewhere. The person can also leave it to run.
- **The person says when it's done,** by playing it. No state or share
  decides that.

## What stays

- Everything for reading the game: `run`, `code_*`, `ram_*`, `disc_*`,
  `assets_*`, `states_*`, the `[off]` and `fixes/` adjustments, input patches.
- Recording and replay: `scenario_record`, `scenario_check`, `scenario_show`,
  the glossary.
- **`port check`: the one thing the agent has to satisfy.** Pass or fail per
  recording, the first difference, the pointer. It still judges a clean
  commit and runs the importers when there are any.
- **`port status`: information only.** Recordings passing, functions the
  recordings ran and how many the port cites, branches run, plays waiting or
  answered. It says nothing about what to do and has no states.
- The play queue: `scenario_play`, `psx play`, psx web.
- `psx web`'s recordings, side-by-side screens and stats.
- `goal.toml`, cut to the description, the assets and `held_out`. A held-out
  play stays a cheap check the agent hasn't seen.
- `not_ported.toml`, optional. It only takes lines out of the "not cited"
  list in `port status`.

## What goes

- `port next` and its items, including Locate, Cover and the stuck counter
  (`port/next.rs`, 1,980 lines).
- `port confirm` and `confirmed`; the person confirms by saying so.
- Review as a gate (`port/review.rs`, `review_lines`). A review is something
  the person asks a fresh agent for.
- The states (working, matching, finished, reviewed, complete) and the shares
  as targets.
- `wont_reach.toml` and `questions.md`. A branch nobody reaches is a number
  in `port status`, and an open question is a line in the status file.
- Sticky Fix (`fixing` in the verdict), and the psx web panels that show any
  of the above.
- Most of the guide's overview (275 lines) and the loop in `AGENTS.md`.

## The status file

`STATUS.md` in the workspace, kept by the agent, as `tr2-rs`'s README table
was: what is ported and checked, what is ported but unchecked, what isn't
ported, and what's next and why. Nothing in psx reads it. It's where gaps become
visible and where the person points.

`AGENTS.md`'s method comes down to a few lines: what the goal is, that
`port check` is the test, that the status file is kept current, and that the
next step is the agent's to choose, saying why.

## The gap, stated plainly

Nothing tells you what's missing beyond what someone noticed. A port can pass
every recording and still lack whole parts of the game no recording shows.
`port status`'s uncited functions and branch count cover part of that. The
person playing covers the rest, and that takes someone who plays it. An agent
left entirely alone will stop earlier than one pushed through `port next`, and
this plan accepts that. The trial below is to find out how much earlier.

## On hold

- `docs/understanding-plan.md` steps 9 (held-out plays and acceptance) and 10
  (the Gran Turismo 2 trial).
- Turning off what isn't being ported, one list entry per system. It would
  sharpen the target and is still the next idea to try once this settles.
- Differential input fuzzing, as an answer to the gap above. It's untested on
  a whole game, so it's a separate bet.

## Candidates from the research

Each is added only if the trial shows the problem it solves:

- A fresh agent session per failing recording, which beat long compacted
  sessions in the published runs.
- Scored screens, since models claim screens match until a pixel difference
  says otherwise.
- Negative controls: `port check` runs a corrupted recording and must fail.

## Steps

### 1. A trial build, deleting nothing

On a new branch off `plain-port`, built in its own worktree so the running
croc workspace's binary isn't touched:

- `port_next` is no longer offered: not in the MCP tools or the server's
  instructions, and `context` doesn't lead with it.
- `port init` writes the short `AGENTS.md` and a `STATUS.md` with its
  headings.
- The guide's overview loses the loop.
- `port status` lists plays waiting and answered, which only `port next`
  announces today.
- `port check` passes on a goal with no catalogue and no confirmation. (From
  reading the code it already should; it needs `goal.toml` and the
  recordings.)

### 2. The trial (the person, with Codex)

A fresh, small goal with the trial build. The agent is told to pick its own
next step and say why. The person keeps a note of each prompt they send.

It worked if:

- the agent's choices are mostly the ones the person would have made, or the
  prompts stay one-line pointers;
- recordings get to passing at least as fast per hour as the current croc run
  does under `port next`;
- less time goes on code that no compared quantity depends on;
- when the agent says something is done, playing it agrees.

If it wanders, or stops early, add back the smallest thing that answers
that, and see whether one line in `port status` (the recording that fails
earliest, say) is enough before anything like an item returns.

### 3. Remove what went

If the trial holds: delete the list above, cut the verdict and goal fields,
teach `port update` to move a workspace over, and bring psx web, the guide,
the C and Croc examples and their tests along. Old `goal.toml`s need to keep
loading until they're moved.

### 4. Then the candidates, one at a time

Only those the trial asked for.

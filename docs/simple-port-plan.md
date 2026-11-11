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
- **A nudge to review.** Once `review_lines` (800 by default) of `game/` and
  `tools/` have changed since the last `review.md`, or that much code exists
  with none yet, `port check` and `port status` say so and hand over the
  brief for a reviewer who hasn't seen the work, as the Review item does now.
  Nothing waits on it: findings stay in `review.md` for the agent to answer
  or the person to read, and committing a new review quiets it.
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
- Review as a gate: reviews due when the goal comes to match or finishes,
  and unanswered findings or a stale review holding anything back.
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
  says otherwise. (Added to the trial as `port_status screens=true`,
  information only, after 10 passing recordings said nothing of framing a
  reviewer saw at once.)
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
- `port check` and `port status` carry the review nudge, moved out of the
  Review item with its brief.
- `port check` passes on a goal with no catalogue and no confirmation. (From
  reading the code it already should; it needs `goal.toml` and the
  recordings.)

Done on branch `simple-port`. `psx port next` and `psx port confirm` still
work from the command line but are hidden. `port check` on the C example
passes with no catalogue and no confirmation. Two things differ from the
list: `port_check` names a review as due and leaves the long brief to
`port_status`, so a run that ignores it doesn't pay for the brief on every
check; and held-out results show only in `port status --plain` (the person's
view), counted, since the gate that showed them is out of reach now. psx web
loses its Next card and goal state; its other cards wait for step 3.

### 2. The trial (the person, with Codex)

A fresh, small goal with the trial build. The agent is told to pick its own
next step and say why. The person keeps a note of each prompt they send.

It worked if:

- the agent's choices are mostly the ones the person would have made, or the
  prompts stay one-line pointers;
- recordings get to passing at least as fast per hour as the current croc run
  does under `port next`;
- less time goes on code that no compared quantity depends on;
- the nudge gets reviews done, and the findings get acted on, without
  anything blocking;
- when the agent says something is done, playing it agrees.

If it wanders, or stops early, add back the smallest thing that answers
that, and see whether one line in `port status` (the recording that fails
earliest, say) is enough before anything like an item returns.

### 3. Remove what went

If the trial holds: delete the list above, cut the verdict and goal fields,
teach `port update` to move a workspace over, and bring psx web, the guide,
the C and Croc examples and their tests along. Old `goal.toml`s need to keep
loading until they're moved.

Done on branch `step3-remove`. `port next` and `port confirm` are gone from
the command line too, with the goal's states, the shares, the loop guard,
sticky Fix, `wont_reach.toml` and `questions.md`; `next.rs` became
`status.rs`, keeping the facts. Old `goal.toml`s with `confirmed` and old
reviews with `matching`/`finished` still load, unread, with a problem that
says to run `port update`, which takes `confirmed` out and moves questions
and won't-reach branches to the end of `STATUS.md`. Held-out plays are
counted for the agent and judged in `port status --plain` for the person,
no longer waiting on a "reviewed" state. The Croc example was moved with
`port update`. Added from the trial: AGENTS.md and the guide say a long
play is the map, ported from short recordings of what it shows.

### 4. Then the candidates, one at a time

Only those the trial asked for.

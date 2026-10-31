# psx feedback

Notes from porting Croc (PAL) with psx 0.8.0, working only from the
workspace, the guide and the tools.

## context suggests port_init inside a workspace

- Tried: `context` at the start, in the workspace `psx port init` made.
- Happened: its `next_calls` offers `port_init` (dest "port") first, then a
  60-field `run`.
- Expected: in a workspace (goal.toml present) it should point at
  `port_next`, as the MCP server's instructions and the guide say.

## project_root on a call fails with "No such file or directory"

- Tried: `code_disasm` and `code_inventory` with `project_root` set to the
  workspace (the same directory as the default root).
- Happened: `worker_failed`: "project worker in <root>: No such file or
  directory (os error 2)". The same calls without `project_root` work.
- Expected: naming the default root explicitly to behave like omitting it.

## Guide example hints at this game's answer

- The guide's and psx-capture's examples name Croc's tick
  (`0x80075240` by `0x80013CA8`) and Croc's object pointer (`0x8007528C`,
  `+0x370`). Running find_tick reached the same store independently, but a
  worked example on the same game as the test case gives the answer away;
  an unrelated example game would keep the test honest.

## IMAGE_SHA256: which hash?

- Tried: psx-capture says IMAGE_SHA256 is "the image's identity, as `psx
  context` reports the media", but `context` reports no hash. I took
  `disc_info hash=true`'s track/bin sha256 (53ed84…).
- Happened: `run` with glossary samples refused: the media is b30a07f5…
  (the error names it, so it was easy to fix).
- Expected: `context` (or `disc_info`) to print the identity the binding
  wants, under that name.

## The binding must be a workspace member

- Tried: glossary/binding as its own crate with an empty `[workspace]`
  table (the root Cargo.toml says the binding "is added once written",
  not how).
- Happened: "package ID specification `glossary-binding` did not match any
  packages": psx builds it with `-p` from the port's workspace.
- Expected: the root Cargo.toml comment or the guide to say: add
  `glossary/binding` to the workspace members, package name
  `glossary-binding`, `crate-type = ["cdylib"]`, build.rs calling
  `write_glossary_glue`. A template stub of the binding would avoid the
  guesswork.

## Croc's rules are bytecode: coverage and `code` miss them

- Found: Croc's controller is a script run by a bytecode VM (object loop
  0x8002C790, opcode table 0x8006B424); the C functions are opcodes
  (MOVE 0x80043234, TURN 0x8004425C, JUMP 0x80028688, ...). The jump
  buffer, the stomp's height test and its sub-states are script branches.
- Happened: a catalogue entry's `code` and the Cover item only know the
  inventory's machine-code functions and their branches, so the script's
  own branches (most of the stomp) are invisible to coverage, and the
  feature can reach "reviewed" without its script paths having been taken.
- Expected: some way to name script code (an interpreter's program and its
  branch points) as a feature's code, or at least guidance in the guide's
  code topic on games whose rules live in a VM.

## The vendored crates name this game's research

- vendor/psx-gte/src/math.rs says its sine table equals the one in
  "Croc's PAL CROC.EXE (at 0x8006D5D4 ...) (research/case-study-croc.md)".
  Useful, but again it hands the test game's answers to the agent.

## Cover: port_status lists 5 of 144 branches

- Tried: port_next said "144 branches of its code are unresolved"; port_status
  lists only five under `unresolved`, with no cursor or file for the rest.
- Did: code_coverage over the feature's recordings, then picked the
  functions out of its JSON by hand.
- Expected: port_status (or a `port_cover F-0001` view) to give the whole
  list, with each branch's function and missed way, or a file of it.
- Also: most of MOVE's branches are for things a flat stand-in floor cannot
  have (the analog stick, slides, ice, walls, platforms). Writing a
  wont_reach line per branch pc is a lot of busywork for "the stand-in
  world has none of these"; a way to give one reason for a block range or
  a condition would fit how the code is shaped.

## Cover counts only a feature's own recordings

- Branches of MOVE that only run in the air (strafing) need a jump, which
  is F-0002's, while MOVE is F-0001's. I named running_jump_steer under
  both features. That works, but it makes F-0001's verdict depend on the
  jump being right, which F-0001's `depends` does not say. Fine here (the
  features are checked together); worth a sentence in the guide.

## psx_fixed's Num has no Reflect

- game/AGENTS.md asks components to derive Reflect so the developer MCP
  can read them; a component holding `Fixed` speeds cannot (vendor/ is
  not to be edited), so Croc's speeds are `#[reflect(ignore)]`.
- Expected: psx-fixed to implement Reflect (behind a feature) or psx-bevy
  to offer a reflected wrapper.

## Review brief vs "holds while its files are unchanged"

- The review's findings were worth fixing, but fixing them changes the
  reviewed files, so the review no longer holds and has to be redone (or
  re-read) at the new commit. The guide could say what is expected after
  answers that change code: a second pass by the reviewer on the diff?

## scenario_play cannot open its window over MCP here

- Tried: scenario_play id=start_1_1 name=play_running with a request.
- Happened: io_error "could not start the handoff window: No such file or
  directory (os error 2)", with no path named.
- Expected: the window, or an error naming what is missing (the window
  binary? a display?), so the agent can tell the person what to fix.

## Held-out plays left the stand-in world, and port_check said "passed"

- Tried: named the person's three held-out plays (play_running, play_jumping,
  play_stomping) under recordings.held_out, committed (57463aa), ran
  port_check, then port_next.
- Happened: port_check returned `"passed": true` with `"held_out": 1` for each
  feature, though `cargo test --test scenarios` shows all three held-out plays
  failing: each diverges the tick Croc's height leaves -225 on a slope
  (play_running tick 71 at x 13017 z 16068; play_jumping tick 104 at
  x 11498 z 14377; play_stomping tick 84 at x 13094 z 12314). The requests
  asked to stay on the flat grass, but the flat area is small and nothing in
  scenario_play warns the player when they leave it. port_next then gave
  Record (agent) because scenario_check fails: play_running and play_stomping
  are not deterministic (a logic tick ends one field later on the second
  route, tick 317 / tick 115, once Croc is on the hill).
- Expected: port_check's verdict to say the held-out plays failed (even
  without showing where), and a way to tell the player the bounds a recording
  must stay inside (a scenario `bounds` the play window enforces or flags),
  so a held-out play the port cannot reach is caught while the person is
  still playing. And guidance for a held-out play that scenario_check finds
  nondeterministic: the agent can't record it again; only the person can.

## scenario check still loses a field on the replayed play_jumping

- Tried: `../../psx/target/release/psx scenario check start_1_1` (the build
  with input following the recorded ticks) after the person replayed the
  three held-out plays on flat ground.
- Happened: 16 recordings deterministic; play_jumping differs: schedule
  "tick 690: ended in field 1380, on the second route in field 1379", then
  croc_turn_rate at tick 690 and croc_angle from tick 691 (a turn press lands
  on a different tick). port_next stays on Record (agent) for F-0001 because
  start_1_1 is "not deterministic", and the agent can't record a person's
  play again.
- Expected: the second route to follow the recorded ticks through a lag
  field, or port_next to route a nondeterministic held-out play back to the
  person (Play) rather than to the agent (Record).

## Held-out plays exercise moves outside the goal

- The replayed play_running (ticks 767-823, 912-957) and play_jumping
  (360-364) hold L1/R1 while Croc stands still: the standing sidestep, root
  motion catalogued as F-0004 and left out of the goal. The port fails
  there by 1-5 units of position while every speed matches.
- Expected: a way to say in a play request which moves are out of scope, or
  for the play window to flag them, since "sidestep with L1/R1 while moving"
  alone didn't stop the person from sidestepping once Croc had stopped.

## Reading port_next's brief counts toward Stuck

- Tried: ran `psx port next` a few times in a row to read the Review brief
  (its `how` is long and I printed it in parts).
- Happened: the fifth call returned `Stuck` (who: person, "port next gave
  this item 5 times with nothing changed"), wrapping the Review item.
- Expected: re-reading the same item to be free (or a `--quiet`/`--brief`
  way to print just the brief), with Stuck counting attempts at the item
  rather than reads.

## A "No findings" review counts as an unanswered finding

- Tried: a review round whose reviewers found nothing wrote one
  `## No findings` heading, as the brief allows ("one ## heading per
  finding"), with nothing under it to answer.
- Happened: port_next: Review, "findings without an answer: No findings".
- Expected: a review with no findings to hold as it is (or the brief to say
  how to write an empty review). I answered "Nothing to change." under it.

## A review that lists its own file never holds

- Tried: reviewers following the brief listed the files they read in the
  review's front matter; two included `reviews/<id>.md` itself (they had
  read the previous round there).
- Happened: once the review was written and answered, port_next said "its
  files changed since the review: reviews/F-0002.md", which writing the
  review always causes.
- Expected: the review's own file left out of the "unchanged" check (or the
  brief to say not to list it). I removed it from the two front matters.

## The whole-code review (psx ea572f1), first run

- Worked well: the brief handed to a fresh sub-agent produced 12 concrete
  structural findings (one character body, generic tick phases, levels
  with placements...). Each change that answered one led psx to ask for
  a follow-up review of the diff only. That took four rounds in all
  (12, 2, 2, then 0 findings), each smaller than the last, and every
  recording passed at each step.
- Got in the way: the follow-up brief doesn't say what an empty round
  looks like. I told the reviewer "update only the commit, add no
  heading". It held, but the brief could say so itself.
- Got in the way: after the review-only commit (review.md alone), port_next
  gave a `Build` item: "the port cannot replay its recordings: no psx port
  check at HEAD", with instructions to register launchers. The launchers
  were already there; it only needed `port_check` re-run at the new HEAD.
  The session's first item had the same cause, worded as `Implement`. A
  "Check: re-run psx port check at HEAD" item would say what is actually
  due. Better still, a commit that touches only review.md or notes/ could
  keep the verdict, as the verdicts already set "notes and the review"
  aside.
- Template: the first review's finding 12 was about template files. The
  disc-import state machine and screens (`game/src/disc.rs`, about 400
  generic lines) ship in `game/`, not `psx-bevy/`. And `tools/import`
  ships with `SERIALS = []`, which accepts any PlayStation disc. I set the
  serial here; moving disc.rs into psx-bevy (a `DiscImportPlugin` taking
  the importer's functions) belongs upstream.
- Minor: AGENTS.md says `cargo test -p <game>`; the package is
  `croc-legend-of-the-gobbos`, not `game` (the lib's name). Naming it, or
  `cargo test --test scenarios` from `game/`, would save a guess.

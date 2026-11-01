# Plan: porting by recordings

The order of work for [port-design.md](port-design.md) and
[feature-inventory.md](feature-inventory.md). All on branch
`port-redesign`, one commit per step, pushed; `cargo test --workspace`
passes after each. No release until phase E is done.

Each step says what it changes and the fact that shows it done.

## Phase A: salvage, then remove mechanics

Removing first shrinks everything later steps touch. Two pieces of the
mechanic code are worth keeping, so they move out first.

1. **Branch outcomes into `coverage`.** Mechanic capture counts each way of
   each branch; `coverage` counts basic blocks. Move the counting into
   `coverage`, so a traced run reports, per function, the branch outcomes
   taken and not. *Done when* `psx coverage` on a traced run lists untaken
   branch outcomes, with the existing capture test's cases as its tests.
2. **Remove mechanics**: `psx mechanic`, `crates/psx/src/mechanic/`, and
   in the template the `replay/` crate; `port next` and `port status` stop
   referring to mechanics (they are rewritten in phase C). The glossary
   binding's loading moves to `crates/psx/src/binding.rs`. This comes before
   the split because `scenario record` ran the binding through mechanic
   capture. *Done when* `psx --help` has no `mechanic` and the tests pass.
3. **`psx-scenario`.** What is left of `psx-mechanics` (the scenario and
   recording formats and the glossary glue generator) becomes a new crate;
   mechanics, interfaces, traces, replay and their Rust reading go. `psx
   scenario`, `port check`, the template and the glossary binding use it.
   *Done when* `scenario record` and `port check` work on Croc unchanged,
   and no source mentions mechanics outside git history and Croc's own
   folders.
4. **Slim `psx-capture`** to the glossary binding: its tick, quantities and
   memory reads. Brackets, explains, params, calls and the audits go. *Done
   when* Croc's glossary binding builds against it and records as before.
5. **Remove cases and the field map**: `psx case`, `samples.toml`, `run
   --map` (until step 9 brings glossary names to `run --sample`). Also
   `template parser` with `templates/parsers/`, and `gpu-replay` (kept as a
   test utility if psx's own tests use it). *Done when* those commands are
   gone and the guide no longer mentions them.
6. **Docs**: remove `mechanics.md`, `rust-mechanics-plan.md`,
   `dispatch-plan.md`, `call-ownership-plan.md`, `agent-workflow.md`.

## Phase B: the twelve commands

7. **Regroup** into `context`, `bios`, `guide`, `read`, `disc`, `assets`,
   `code`, `ram`, `run`, `states`, `scenario`, `port` (feature inventory,
   "The commands"): `disc info/ls/extract/sector/exe`; `assets` with `xa`
   and `str` as formats of `extract`, `textures`, `diff`, `memcard`; `code`
   with Ghidra behind `decompile`, `functions` and `annotate`; `ram` with
   addresses classified wherever printed; `run --save-as`, `--diagnose`,
   `--trace-cd` writing the decoded loads; `states list/save/rebuild`;
   `port init --existing` and `port update`; `context` reporting BIOSes and
   Ghidra. *Done when* `psx --help` lists the twelve and `mcp` serves only
   their subcommands.
8. **Hex everywhere**: every PC and address psx prints, `writes.jsonl`
   included, with its region (main RAM, scratchpad, I/O, BIOS) beside it
   where that is not obvious, now that `addr` is gone. *Done when* no
   output field holds an address in decimal (fields that need a number for
   sorting keep it beside the hex).

## Phase C: the new pieces

9. **`run --sample` by glossary name**, and once per game tick (the
   glossary binding's tick) as well as per field.
10. **`code coverage diff`**: what one set of runs reaches that another
    does not, by function and branch outcome.
11. **Coverage of recordings**: `scenario record` traces the run and keeps
    its coverage beside the recording, so a feature's coverage is the union
    over its recordings without running them again.
12. **`scenario show`**: a recording's quantities per tick, with deltas.
13. **`scenario play`**: handoff folded in. It starts at the scenario's
    state, shows the request, and writes a recording with the same frame
    origin as `scenario record` (fixing today's one-field offset);
    `--from <recording>` continues from the end of one; `--detach` and
    `--status` for MCP clients. `handoff` and `handoff-status` go.
14. **The catalogue entry** (port-design.md, section 3): `code`,
    `recordings.work`/`held_out`, `wont_reach` and `questions`, checked by
    `port confirm`; scenario tolerances per quantity.
15. **`port status`**: stages (with Reviewed), verdicts, branch coverage
    per feature, open questions, reviews, and the asset files by state.
    A review (`reviews/<id>.md`) holds while its files are unchanged since
    its commit and every finding has an answer.
16. **`port next`**: one item in the order of section 5; Play and Confirm
    items for the person; the Review item's brief for a reviewer who has
    not seen the work: read the code as a stranger who must maintain or
    reuse it; the catalogue entry, the files the feature's commits touched
    and what that code uses, not the disassembly; judge against AGENTS.md's
    "The code", not against the original (the recordings do that); write
    each finding as what is wrong and why, not the fix.
17. **The asset inventory**: each disc file unknown, format found,
    imported, checked or unused, from `formats/` and the importers' tests,
    feeding status and next.
18. **Template fix**: a binary built with `dev-mcp` starts on its own.

## Phase D: the guide

19. **Rewrite the guide**: an overview, the porting loop and its rules,
    assets, recordings and play, status and next, and the command
    reference, short enough to read in one go. Topics that described
    mechanics, cases and fields go.
20. **Examples**: decided: both stay until step 21. They predate the port
    workspace (the old `engine/` layout, CSV trajectories, no goal,
    catalogue or recordings). Step 21's workspace replaces
    `examples/croc-movement` as the one worked example of the loop, and
    `examples/megaman8-movement` goes then; the case studies in
    `research/` stay.

## Phase E: proving it

21. **Croc's movement, end to end, on a fresh workspace** made by `port
    init`: the goal "Croc's running, jumping and stomp", found with
    coverage diff, implemented as an idiomatic plugin (a component, typed
    state, a world the host provides; the controller review's points),
    checked against the recordings, with at least one held-out recording
    the person plays, driven by `port next` until it has nothing left.
    It then becomes `examples/croc-movement` (no disc data: recordings
    hold quantities, coverage no instructions, states stay out of git),
    `examples/megaman8-movement` is removed, and the README's examples
    point at it.
22. **One asset, end to end**: a Croc file the template does not already
    decode (1-1's level geometry), from `loads` to an importer checked
    against the RAM the game builds.
23. **Coverage of scripted rules**
    ([script-coverage-design.md](script-coverage-design.md)): programs
    the binding names, whose bytes read count as coverage, and hooks in
    the binding that name script positions and decision ways however the
    interpreter works; `code` and `wont_reach` take `name+offset`
    stretches; Locate's coverage diff and Cover work in script space.
    With a decoder, the binding also gives each program's map
    (statements, decisions, routines), so coverage counts statements run
    and decisions no hook named. Built (psx-capture boundary version 6)
    and tried on Croc's F-0002 and F-0003: the diff gives the stomp state
    by name, and Cover asks for the bounce-pad jump, the second stomp from
    high, a long fall's landing and a random switch's default. Next: Croc
    settles those (recordings, or wont_reach the person confirms), then
    the example is updated.
24. **Croc in a level: assets, slopes and platforms.** The port so far
    runs on a flat floor with no model. Next it imports Croc's model, his
    animations and 1-1 from the person's disc, and moves on the level's
    slopes, ledges, walls and platforms as the original does. psx's part
    comes first, **choosing which assets**. A goal now takes all of the
    disc (`assets = true`) or none, but a feature like "Croc on slopes"
    needs 1-1's geometry, not every file, and how much of a game a port
    extracts (every level, every car) is the person's choice:
    - **Names.** An asset name is a disc path or archive entry, as
      format docs write them (`/MAPS/*.WLD`, `/CROCFILE.1:CROC.*`; `*`
      and `?` within a name), or `format:<id>`, every file
      `formats/<id>.md` covers.
    - **The goal** takes `assets = true` (every file), `false` (none),
      or a list of names: what the person wants extracted.
      `port confirm` keeps the list with the features; taking a name
      out needs the person again, as taking a feature out does.
    - **A feature's entry** takes `assets`, the names its recordings
      need to replay (1-1 for slopes). A feature in the goal brings them
      in, whatever the goal's own list says.
    - **What is needed** is the goal's names and its features', each
      file once. A name resolves against the inventory. A path into an
      archive whose entries the importer does not list yet gives that
      archive's Index item first. A `format:` id with no doc gives a
      Format item: find which files those are (a run with `trace_cd`
      that loads one) and write the doc. A name that still matches no
      file is a problem `port next` reports.
    - **Order.** The assets a goal feature needs (Format, Index, Import,
      Check) come before item 1, since its recordings cannot replay
      without them. The goal's own names come where assets come now
      (item 6). A feature is Reviewed only with its assets checked, and
      the goal is done only with every needed file checked or unused.
    - **Status.** `port status` counts the needed files, and other files
      apart. `port status --assets <name>` lists the files a name
      matches, each with its format and state.

    Built: goal.toml's `assets` takes names, a catalogue entry `assets`,
    and the confirmation keeps the names.

    Also before Croc: **a review each time a feature finishes**, not only
    once the goal's features all have, so a whole port is not one huge
    review at the end. The review lists the features finished when it was
    written; a finished feature it does not list, with the code changed
    since, asks for a follow-up of the change (port-design.md, The
    review). Built; on Croc no feature finished (each matched, then the
    next feature's fixes came first), so no review came in 43 commits:
    the review now comes when a feature first matches its recordings.
    Then every feature had matched and been reviewed, and the slides and a
    collision refactor (+931/-534 lines, 18 commits) went unreviewed until
    the last would finish: the review comes again when each feature
    finishes (review.md's `finished`).

    Then Croc, by the porting agent in Croc's workspace through `port
    next`, with the person confirming and playing:
    1. Settle the Cover items step 23 found (recordings, or wont_reach
       the person confirms).
    2. **Croc's animations** (a new feature, with F-0004's sidestep): his
       model and animations imported and checked. The animation playing
       and its frame become glossary quantities, so the recordings check
       the port plays them tick for tick; the rules that hang on them
       (animation events in the script, the stomp's timings, the
       sidestep's root motion) come from the imported data, not
       constants.
    3. **1-1's geometry** (step 22's asset): imported, checked against
       the RAM the game builds, with collision in the port's own level
       format.
    4. **Slopes, ledges and walls**: the ground F-0001's wont_reach set
       aside (slides, walls, walking off ledges), recorded in 1-1. Those
       entries come out of wont_reach, and the person confirms again.
    5. **Platforms**: standing on moving objects, recorded where 1-1 has
       them.

    Last, outside psx, the example: making stand-in assets is a game's
    own work once its features are done, so psx and the template know
    nothing of them. `examples/croc-movement` ships a stand-in Croc
    (its own mesh and clips, timed to the imported animations' gameplay
    data) and a small arena of the kinds of ground the recordings cover,
    loaded in place of the imported ones. The recordings that need 1-1
    run only with the person's disc; the README says so.

After phase E: decide on the release, and whether the method goes to
other platforms (mdx).

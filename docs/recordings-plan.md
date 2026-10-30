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
   included. *Done when* no output field holds an address in decimal
   (fields that need a number for sorting keep it beside the hex).

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
15. **`port status`**: stages, verdicts, branch coverage per feature, open
    questions, and the asset files by state.
16. **`port next`**: one item in the order of section 5; Play and Confirm
    items for the person.
17. **The asset inventory**: each disc file unknown, format found,
    imported, checked or unused, from `formats/` and the importers' tests,
    feeding status and next.
18. **Template fix**: a binary built with `dev-mcp` starts on its own.

## Phase D: the guide

19. **Rewrite the guide**: an overview, the porting loop and its rules,
    assets, recordings and play, status and next, and the command
    reference, short enough to read in one go. Topics that described
    mechanics, cases and fields go.
20. **Examples**: look at `examples/croc-movement` and
    `examples/megaman8-movement`; keep or replace them with one worked
    example of the loop.

## Phase E: proving it

21. **Croc's movement, end to end, on a fresh workspace** made by `port
    init`: the goal "Croc's running, jumping and stomp", found with
    coverage diff, implemented as an idiomatic plugin (a component, typed
    state, a world the host provides; the controller review's points),
    checked against the recordings, with at least one held-out recording
    the person plays, driven by `port next` until it has nothing left.
22. **One asset, end to end**: a Croc file the template does not already
    decode (1-1's level geometry), from `loads` to an importer checked
    against the RAM the game builds.

After phase E: decide on the release, and whether the method goes to
other platforms (mdx).

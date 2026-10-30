# psx feature inventory

What psx keeps, merges and removes for porting by recordings
([port-design.md](port-design.md)). Every subcommand is an MCP tool whose
description sits in each agent's context, so fewer, grouped commands are
easier to learn and cheaper to run.

## The commands

About 33 top-level commands become 12, and `mcp` to serve them:

| Command | Subcommands | Takes in |
|---|---|---|
| `context` | | `context`, `bios list`, `ghidra doctor` |
| `bios` | `install` | `bios install` |
| `guide` | topics | `guide`, rewritten for the new process |
| `read` | | `artifact-read` (for MCP clients without a shell) |
| `disc` | `info`, `ls`, `extract`, `sector`, `exe` | `disc`, `ls`, `extract`, `sector`, `exe` |
| `assets` | `scan`, `extract`, `textures`, `diff`, `memcard` | `assets scan/extract`, `xa`, `str`, `textures`, `vram-diff`, `memcard`; `extract` picks the decoder by format (TIM, TMD, VAB, XA, STR) |
| `code` | `disasm`, `xrefs`, `decompile`, `functions`, `annotate`, `symbols`, `inventory`, `coverage` | `disasm`, `xrefs`, `symbols`, `ghidra *`, `inventory`, `coverage`; Ghidra becomes the engine behind `decompile` and `functions` (`analyze` is automatic, `export-symbols` part of `annotate`) |
| `ram` | `diff`, `search`, `view` | `ram *`, `addr` (every address psx prints comes classified) |
| `run` | | `run`, `loads` (`--trace-cd` writes the decoded loads), `native` (`--diagnose`, for psx development), `states make` (`--save-as NAME`) |
| `states` | `list`, `save`, `rebuild` | `states` without `make` |
| `scenario` | `record`, `play`, `show`, `check` | `scenario`, `handoff`, `handoff-status` (`play --detach` and `play --status`) |
| `port` | `init`, `update`, `confirm`, `status`, `next`, `check` | `port`, `template create/update/adopt` (`update`, `init --existing`) |
| `mcp` | | |

Removed: `mechanic` (check, capture), `case` (record, check) and the field
map (`samples.toml`, `run --map`), `template parser` (the Python
external-parser starter), and `gpu-replay` (a test utility of psx's own).

## Changes within commands

- **Hex everywhere.** Every PC and address in psx's output in hex
  (`writes.jsonl` gives PCs in decimal today).
- **`run --sample` takes glossary names** and can sample once per game
  tick, not only per field; it replaces the field map.
- **`code coverage diff`**: the routines and branches one run reaches and
  another does not, which is how a feature's code is found; and per-feature
  branch coverage for `port status`.
- **`scenario play`**: today's handoff, writing a recording with the same
  frame origin as `scenario record` (today the handoff's input lands one
  field late), and `--from <recording>` to continue from the end of one.
- **`scenario show`**: a recording's quantities per tick, with deltas.
- **`port status` and `port next`** rewritten to the stages and order in
  port-design.md, section 5.

## Crates

| Crate | Decision |
|---|---|
| `psx-core`, `psx-gpu`, `psx-gte` | Keep: the emulator |
| `psx-oracle` | Keep, for psx development: calibrating the core against Mednafen |
| `psx-media`, `psx-assets`, `psx-symbols` | Keep |
| `psx-fixed` | Keep: exact fixed-point arithmetic in ports |
| `psx-capture` | Keep, slimmed to what the glossary binding needs (its tick, its quantities, reading memory); brackets, explains and the read and write audits go |
| `psx-mechanics` | Split: the scenario and recording formats and the glossary glue generator move to a new `psx-scenario`; mechanics, traces, replay, interfaces and Rust parsing go |

## The template (psx-bevy)

- **Keep:** the PS1 look, ticks, input and the controls menu, display
  settings, the launcher, the scenario launcher, `testing` (headless app,
  trajectory comparison), the developer MCP, the `--scene` capture viewer.
- **Fix:** a binary built with `dev-mcp` starts on its own (today it cannot
  find Bevy's shared library outside `cargo run`).
- **Remove:** step replay (`psx_bevy::replay`) and the workspace's
  `mechanics/`, `interfaces/` and `replay/` folders.

## The repository

- **Docs removed:** `mechanics.md`, `rust-mechanics-plan.md`,
  `dispatch-plan.md`, `call-ownership-plan.md`, `agent-workflow.md`.
  `scenarios.md` and `ghidra-integration.md` stay, updated.
- **Examples:** `examples/croc-movement` and `examples/megaman8-movement`
  are looked at before deciding; the likely outcome is one worked example,
  Croc's running and jumping against its recordings.

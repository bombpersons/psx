# Plan: workspace-layout template

Status: implemented (see "As built" at the end for where it differs from the proposal).

## Why

The engine-first porting prompt (`../external_agent_tests/AGENTS.md`) asks for an
`engine/` + `game/` + `tools/` workspace from day one. `template create` produces
the opposite: one crate (`psx-game`, lib `game`) mixing the reusable PS1 modules
with the demo, into an empty folder only, with no record of its version. Every
run then does the same surgery. The Tomb Raider run has about 45 uncommitted
`game/src/* -> engine/src/*` moves, and it merged a template update into
`game/src/controls/` just before moving those files away.

Goals:

1. `template create` writes the workspace the prompt describes, already split.
2. The template-owned code sits in one crate that the project never edits, so
   updates replace it instead of being merged file by file.
3. The demo shows the patterns we want copied: a registered controller, tuning
   values in an asset, and a sandbox that builds without the game.
4. The generic half of the prompt moves into psx (`guide port`), so a per-game
   prompt is about ten lines.

## Generated layout

```text
<dest>/
  Cargo.toml            workspace: members, workspace.dependencies, profiles
  Cargo.lock
  rustfmt.toml  .gitignore
  .psx-template.toml    version manifest (see below)
  AGENTS.md             only if absent: short map of the crates, points at the nested ones
  psx-bevy/             TEMPLATE-OWNED. Never edit; `psx template update` replaces it
    Cargo.toml          crate `psx-bevy`, lib `psx_bevy`, feature `dev-mcp`
    AGENTS.md           "do not edit; extend from engine/ or game/"
    src/  psx/ shell.rs controller(.rs|/) controls/ testing(.rs|/) viewer.rs
          model_viewer.rs dev_mcp.rs launcher.rs (new)
    tests/  controls + dev-MCP checks against a built-in test scene
  engine/               crate `engine`: this game's engine family, no game content
    AGENTS.md           placement rules (from the prompt)
    src/lib.rs          EnginePlugin, re-exports psx_bevy::prelude
    src/schedule.rs     EngineSet: named sets in update order, placeholders to rename
    src/objects.rs      ObjectTypeRegistry: type id -> spawn hook + system set
    src/triggers.rs     ActionRegistry: action id -> handler; UnknownActions resource
    examples/sandbox.rs + engine/assets/   hand-authored room, character, one type, one trigger
  game/                 crate `<name>` (lib `game`, bin `<name>`)
    AGENTS.md           evidence-to-code guide (today's template AGENTS.md, trimmed)
    src/lib.rs          REGION, FIELDS_PER_TICK, TITLE, GamePlugin
    src/main.rs         about 10 lines: psx_bevy::launcher::run(GameConfig { .. })
    src/demo/           demo player registered as an object type, tuning in assets/demo/player.ron
    tests/player.rs     demo trajectory test; tests/dev_mcp.rs only the demo-specific part
    assets/
  tools/import/         crate `import`: skeleton CLI + provenance helper (source path, size, sha256)
  docs/
    engine-model.md     stub holding the prompt's five questions
    evidence.md         stub: what is verified, against what
```

Choices behind it:

- **Template code becomes a crate, not files the project owns.** `engine/`
  depends on `psx-bevy` and re-exports what game code needs. The prompt's line
  "the template's modules move into `engine/`" becomes "`engine/` builds on
  `psx-bevy`". Both are game-agnostic, so the sandbox rule still holds.
- **`main.rs` logic moves into `psx_bevy::launcher`.** The CLI flags (`--mcp`,
  `--headless`, `--software`, `--region`, `--scene`, `--model`, `--controls`,
  `--bindings`, `--painter`, `--no-dither`, `--no-quantize`), the window, the
  renderer and headless setup are all generic. Today they live in the
  project's `main.rs`, so template fixes to them never reach existing projects.
  `GameConfig { title, region, fields_per_tick, plugins: impl FnOnce(&mut App) }`.
  The sandbox uses the same launcher.
- **Assets stay per crate** (`game/assets`, `engine/assets`), matching Bevy's
  `CARGO_MANIFEST_DIR` default. The prompt's top-level `assets/` changes to
  `game/assets/`. This avoids a custom asset root that breaks when shipped.
- **The engine skeleton stays deliberately tiny.** Registries and named sets
  only, with no component schema. Each file's doc comment says to rename or
  replace it to match `docs/engine-model.md`. Anything bigger would be the
  "invented abstraction" the prompt warns against.
- **Feature alignment carries over.** `dev-mcp` lives on `psx-bevy`. `engine`
  and `game` forward it (`dev-mcp = ["engine/dev-mcp"]` and so on). The
  always-on serde/futures/base64 alignment moves into `workspace.dependencies`,
  as in the TR root `Cargo.toml`.

## `.psx-template.toml`

```toml
template = "psx-bevy"
psx = "0.x.y+<PSX_BUILD_ID>"
created = "2026-10-17"
region = "pal"
fields_per_tick = 2

[owned]              # replaced by `template update` when unmodified
"psx-bevy/src/psx/mod.rs" = "sha256:…"
# … every file under psx-bevy/

[seeded]             # written once; the project owns them afterwards
"engine/src/objects.rs" = "sha256:…"
"Cargo.toml" = "sha256:…"
# … everything else create wrote
```

## Commands

- **`template create DEST [--name] [--region] [--fields-per-tick]`**
  - DEST may be non-empty. Refuse only if a path we would write already exists.
    The one exception is `AGENTS.md`, which is skipped if present and reported
    under `skipped`, with a hint to link the nested files. A disc and a user
    `AGENTS.md` in the folder are then fine.
  - Writes the manifest and returns `layout`, `owned_count`, `seeded_count`,
    `skipped` and `next` (adding `cargo run -p engine --example sandbox`).
- **`template update [DIR] [--dry-run]`** (new, CLI + MCP `template_update`)
  - Owned files that are unmodified or missing: write the new version.
  - Owned files that were modified: leave them, write `<file>.psx-new`, report
    as a conflict.
  - New owned files: add them. Owned files removed upstream: delete if
    unmodified, otherwise report.
  - Seeded files: never touched. Report which seeded *templates* changed
    upstream (e.g. root `Cargo.toml` dependency bumps) with a unified diff path
    under `.psx/template-update/`, for the agent to apply by hand.
  - Rewrite the manifest. Output: counts plus paths, following the "small
    replies" rule.
- **`template adopt [DIR]`** (new, small): for pre-existing projects such as TR.
  The user moves their copies into `psx-bevy/` first; adopt hashes them
  against the matching psx version if given (`--from <build id>`) or the
  current one, and writes the manifest. Documented as a migration recipe only.

## Guide and docs

- New topic **`port`** (`crates/psx/src/guide/port.md`): the generic parts of
  the prompt. Covers why the engine comes first, finding the engine model, the
  four placement rules, extension points, the sandbox rule, fidelity as the
  contract, the work order and "done means". Kept to the size of the other
  topics, with details pointing at the nested AGENTS.md files.
- `bevy.md` and `overview.md`: describe the workspace, `template update`, and
  point at `port`.
- `README_FOR_AGENTS.md`, `README.md` "Get started", and the template
  README's project-layout table rewritten for the three crates.
- A trimmed example prompt in `docs/port-prompt.md`: goal, evidence policy,
  "create the template here", "read `guide port`".

## Implementation steps

1. **Restructure `templates/psx-bevy/` into the workspace** (source of truth
   stays this directory).
   - Move the infrastructure modules into `psx-bevy/src`. Fix the hardcoded
     `"game/psx/shaders/…"` test path in `shaders.rs` to use `CARGO_CRATE_NAME`.
   - Add `launcher.rs` and shrink `game/src/main.rs`.
   - Split `tests/dev_mcp.rs`: the generic parts go to `psx-bevy/tests` against
     a test scene; the demo edit/step test stays in `game/tests`.
   - `cargo test --workspace --features dev-mcp --all-targets --no-run`,
     `cargo test --workspace`, and the ignored GPU tests with `--software`.
2. **Engine skeleton + sandbox.** Registries, `EngineSet`, `UnknownActions`;
   the sandbox example and its RON/PNG assets. Port the demo player onto the
   registry with `player.ron` tuning. Check the demo trajectory test still
   matches exactly.
3. **`tools/import` skeleton.** A plain-Rust CLI with a provenance helper
   (writes source path, size and sha256 next to each output) and one stub
   subcommand. It must build without the psx workspace, so it doesn't use
   psx-assets; raw files come from `psx` extraction.
4. **`template.rs`.**
   - Embed the new file list and split it into owned and seeded files.
   - Rewrite name, region and fields-per-tick in `game/` (`game/src/lib.rs`,
     `game/Cargo.toml`, root `Cargo.lock`).
   - Handle non-empty destinations, write the manifest, add `update`/`adopt`,
     and register the MCP tools.
   - Tests: create into a dir holding `AGENTS.md` and a fake `.cue`; update with
     unmodified, modified, added and removed owned files; `embeds_every_template_file`
     extended to nested crates; a guard test that `engine/Cargo.toml` and
     `psx-bevy/Cargo.toml` don't depend on `game`.
5. **Guide topic `port`, doc updates, example prompt.** Check the MCP tool
   definition budget (about 14K) after adding `template_update`.
6. **Smoke test end to end:** `psx template create` into a scratch folder with a
   disc, then `cargo run -p engine --example sandbox -- --headless --software`
   plus a dev-MCP screenshot, `cargo test --workspace`, then
   `psx template update --dry-run` reporting zero changes.

## Out of scope (follow-ups)

- ~~Migrating `examples/croc-movement` and `examples/megaman8-movement`.~~
  Done: both are generated workspaces with the study in `game/` (the engine is
  still the skeleton; they only port the player).
- Migrating the Tomb Raider project (use `adopt` once it exists).
- psx help for step 1 of the prompt, e.g. finding per-type control-function
  tables (function-pointer arrays) in RAM or EXE.

## Decisions

1. Workspace only; no single-crate layout.
2. Crates `psx-bevy`, `engine`, and `<name>` for the game (lib `game`).
3. The registries ship, small, with editor-readable metadata (type names,
   reflected tuning type).
4. An editor skeleton (`editor/`, egui via bevy_egui) with menus, outline,
   view, inspector and status bar over the engine's level format. Ports grow
   it one settled format at a time, depending on `engine/` only, as a modern
   high-resolution mouse-and-keyboard application that does not reuse
   psx-bevy's PS1 renderer. engine/ depends on bevy_egui too, so the game and
   editor resolve the same Bevy features and share one build. (Replaces the
   earlier decision to ship no editor skeleton.)

## As built

- `psx_bevy::launcher::{run, app, Game, Args}`: `Game { name, title, region,
  fields_per_tick, add }`. The depth-cue example and the sandbox use it too.
- `BindingStore::default_path(app)` takes the game's package name (the crate
  name is now `psx-bevy` for every game); `ControlsPlugin::default()` keeps
  bindings in memory, and the launcher passes the per-game path.
- `Alignment` has no `Default` (it read the game's `FIELDS_PER_TICK`): use
  `Alignment::new(game::FIELDS_PER_TICK, start)`.
- The dev-MCP test client is `psx_bevy::testing::mcp::Client`. The crate's GPU
  tests drive the depth-cue example; the demo's test lives in `game/tests`.
- Tuning and levels load synchronously from RON (`engine::config`), so
  headless tests see the same values; `TriggerVolume` reads `Transform`
  because headless apps don't propagate transforms.
- `tools/import` hashes sources with FNV-1a (no dependencies); the manifest
  uses sha256.
- `template update` takes the project root (`--root`/MCP project), not a
  path. Changed seeded files are copied to `.psx/template-update/` rather than
  diffed. `adopt` is CLI-only (rare migration; keeps the MCP tool budget).
- The `guide` tool description no longer repeats the topic list (the schema
  enum has it), to stay inside the 50 KB tool budget.
- Routine checks in a project: `cargo test --workspace --all-features`.

# Design: porting by goals and mechanics

This is the design for how psx ports a PlayStation game. It covers what
the person asks for, how the work is split into features and mechanics,
how the original's behaviour is captured and the port tested against it,
how the port is built, how progress is measured, and how a session knows
what to do next.

It replaces the clean-room protocol (investigator, reviewer and
implementer roles, publishing across a barrier, audits) and
[dispatch-plan.md](dispatch-plan.md), which planned a refactor of that
protocol's dispatch.

Decisions that are the person's to make are marked **Decision** and
collected at the end.

## 1. Goals

1. **Port what the person wants.** The person names a goal: the features
   they want. The port is done when those work. The whole game is one
   possible goal.
2. **Features are reusable.** An extracted feature works with the person's
   own content (a hand-made track, a new map), so a port can be the base of
   a new game.
3. **Exact where it matters.** Physics and mechanics behave like the
   original: the driving must not feel any different. Exactness
   constrains the port's arithmetic, not its structure.
4. **Modern, idiomatic code.** Idiomatic Rust and Bevy: plugins,
   components, systems, and assets in modern formats (glTF, PNG, WAV/OGG,
   RON). The measure is whether a good Rust/Bevy developer would write it
   that way, not whether it differs from the original. Where the
   original's shape is also the natural one, the port keeps it.
5. **Not locked to the PS1.** The original's look by default, with its
   quirks switchable off, any aspect ratio, and a replaceable renderer.
   Exact mechanics can be switched to `f32` later when a new game does not
   need bit-exactness.
6. **Progress you can see, and where to look next.**
7. **General.** Nothing assumes one game, or that the original was compiled
   rather than written by hand in assembly.
8. **Cheap to run.** One interactive session at a time, with state on disk
   so any session can continue the work. No loops, no restated documents,
   no files too big to read.

Not a goal: legal separation. The porting agent reads the original's code
freely. The port is derived from the original, so publishing it is a risk
the person takes on knowingly. psx makes nothing about this automatic.

## 2. What changes from today

- **The clean room goes.** No roles, no barrier, no publishing, no
  crossing, no audits, no clean mode, no allowances, no kept-back cases.
  One agent investigates and ports, with full access to psx and the
  original's code.
- **`port run` goes.** The person works with an ordinary interactive
  session (Claude Code, Codex) in the port's workspace. psx keeps the
  port's state on disk and says what is due next.
- **Specs go as a required step.** The evidence is mechanics with step
  traces, and scenarios. Prose is kept to a short note per mechanic.
- **Work is organised by feature, not by the original's functions.**
- **The idiomatic-structure check moves** from "never see the code" to a
  structure review: mechanical signals, a self-review, and a fresh-context
  review per feature.

## 3. The person's view

1. The person opens a session in the port's workspace and says what they
   want: "use psx to extract car handling and the tarmac tracks from this
   GT2 disc". They give one or more play-throughs: recordings of them
   playing the parts that matter.
2. The agent proposes a **catalogue**: the features the goal needs, what
   is in and what is out, and their dependencies. The person approves or
   edits it. This is the one checkpoint before porting starts.
3. The agent works through the features. It stops and asks when:
   - it needs a recording it cannot make itself ("the gap list needs a
     manual-gearbox car on a wet track");
   - a choice is open that the goal does not settle (exact or faithful for
     a borderline mechanic, an engine/game split);
   - something is wrong with the process or the tools.
4. The goal is done when psx says so (section 9), not when the agent thinks
   so. The person gets a Bevy app with the features as plugins, the disc's
   assets converted to modern formats, and a sandbox per feature built from
   hand-made assets.
5. Later goals build on the same port. "Car handling" today and "race
   rules and AI" next month add up, and the whole game is all of them.

## 4. The model

| Thing | What it is | File |
|---|---|---|
| **Goal** | what the person wants, and how exact each part must be | `goal.toml` |
| **Catalogue** | the game's features in game terms, with dependencies | `catalogue/F-NNNN.md` |
| **Feature** | one capability a player would name: "car handling on tarmac", "Lara's ledge climbing", "the race HUD" | an entry in the catalogue |
| **Mechanic** | one step the original runs, with typed state, inputs, params and world queries, captured from the game step by step | `mechanics/<id>/` |
| **Glossary** | the names quantities, assets and events go by | `glossary.toml` |
| **Scenario** | a reproducible situation both the original and the port can reach | `scenarios/<id>.toml` |
| **Format** | a disc file format, read by an importer | `formats/<id>.md` plus importer tests |
| **Asset contract** | the port's own asset type: schema, units, which feature reads it | `docs/assets/` |
| **Sandbox** | a scene per feature built from hand-made assets | `examples/sandbox_<feature>.rs` |
| **Attribution** | which of the original's code each feature and mechanic accounts for | computed by psx from traces |

### Goal and catalogue

```toml
# goal.toml
description = "Car handling on tarmac, for an open-world racer"
features = ["F-0003", "F-0007"]          # or features = "all"
confirmed = { at = "2026-11-02T10:14:00Z", features = ["F-0003", "F-0007"] }   # written by `psx port confirm --yes`
```

```yaml
# catalogue/F-0007.md front matter; the body is a paragraph in game terms
id: F-0007
title: Car handling on tarmac
area: driving
depends: [F-0002, F-0005]   # car setup data, track surfaces
mechanics: [gt2.drivetrain, gt2.tyre-forces, gt2.chassis, gt2.wall-contact]
scenarios: [time_trial_straight, time_trial_hairpin]
```

The flow:

1. The person states the goal in their own words.
2. The agent catalogues the features it implies and proposes the list.
3. The person confirms it (`psx port confirm` shows the list, its closure
   and any problems; `--yes` records the time and the list itself, so a
   later list can be checked for having grown or shrunk). psx adds the
   dependency closure.

Until a goal is confirmed, the agent only investigates and catalogues. A
confirmed list grows freely and never shrinks without the person.

Feature granularity follows two tests:

- **Could someone want this without its neighbours?** If so, it is a
  feature of its own.
- **Does it have behaviour of its own?** If not, it belongs inside another
  feature.

"Physics" is too coarse, and "the friction curve" is too fine. "Car
handling on tarmac" sits between, with "on gravel" as a separate feature
that depends on it.

### Mechanics

A mechanic is the unit of exactness inside a feature: one routine's step,
captured from the game with its inputs and results so the port's version
can be replayed against it without an emulator. Mechanics are how a
feature's exactness is proven and how its completion is tracked; the
feature is what the person gets. The file formats are psx's own, adapted
from the mechanics knowledge base's draft (Decision 4), and defined in
`docs/mechanics.md`.

```
mechanics/gt2.drivetrain/
  mechanic.toml     # typed state, inputs, params; interfaces it requires
  binding.toml      # where this release keeps them, and the capture bracket
  traces/*.trace.jsonl
  NOTES.md          # a few lines: what it does, quirks, what traces miss
```

**`mechanic.toml`** declares:

- **state**: what the step owns (rpm, gear);
- **inputs**: what it reads each step but does not own (throttle, the
  frame counter's parity);
- **params**: constants whose values come from the binding (gear ratios,
  a torque curve);
- **requires**: interfaces for world queries (`track.surface-at@1`);
- **rate**: how often the original runs the step (per tick, per frame,
  steps per second), so another game can run it at its native rate.

Each field has a type: `int` with bits and sign, `fixed` with integer and
fraction bits, `bool`, or `enum` with named variants.

**`binding.toml`** says where the values live and how the step is
bracketed:

```toml
[state.rpm]
parts = [{ at = { base = "a0", offset = 0x40 }, bytes = 2 }]
[params]
final_drive = 0x3800
[params_provenance]
final_drive = "car record field 0x1C, read at 0x800A1234"
[capture]
begin = { pc = 0x800A1200, overlay = "race", a0 = 0x801F0000 }
end = { pc = 0x800A13F8 }        # or "next_begin" or "frame"
explained = [{ at = { base = "a0", offset = 0x80 }, bytes = 0x20, why = "suspension, another mechanic" }]
```

(Addresses here are placeholders.)

R3000 specifics:

- **Locations** are RAM addresses, scratchpad, or `{ base = "<reg>",
  offset }` relative to a register latched at begin. Register values
  (`{ register = "v0" }`) bind arguments and returns.
- **A begin or end PC names its overlay**, since the same address runs
  different code as overlays change. The overlay is identified the way the
  inventory identifies it.
- **Register filters** on begin pick one caller or object: the player's
  car rather than the AI's, when they run the same routine.

**Capture** (`psx mechanic capture`) runs a recording with the binding and
writes one trace line per step: the inputs and the state at begin, the
state at end, and the world calls made in between. It reports:

- **The read audit.** Every location the step read that the binding does
  not account for: a hidden input, a param's source, unbound state, or a
  world query. The agent binds the ones the result depends on and explains
  the rest. psx drafts the list grouped by reading PC, so the agent edits
  it rather than writing it. It also covers what memory reads miss:
  - CPU registers read before the step writes them, beyond those bound;
  - GTE registers read before the step loads them (a rotation matrix set
    up by other code is a hidden input).
- **External changes.** Steps whose starting state differs from the
  previous step's result: code outside the mechanic changed it. Many of
  them mean the bracket is wrong, or that part of the state belongs to
  another mechanic. This is how feature and mechanic boundaries are found.
- **Branch coverage.** Which of the routine's branches the traces reached,
  from the executed instructions inside the bracket. The rest are listed
  in `NOTES.md` as not covered, or a recording that reaches them is made.
- **Refusals.** Values that do not fit the declared type, lossy rescales,
  enum values with no variant, and calls that change the mechanic's state.

**Interfaces** are the seams that make a mechanic portable. A world query
(the surface under a wheel, the floor height at a point) is an interface
call recorded with its request and response. The port's step takes the
world as a trait, so in another game the person implements the trait for
their own world. A good interface asks what the original's routine
actually asks, as low-level as it really is. The decision about what to
do with the answer stays in the mechanic, where traces prove it.

Calls are of two kinds. A **query** asks the world something and gets a
response. A **command** acts on the world with no response: damage the
object in this box, play the swipe sound, spawn a particle. Both are
recorded and checked in order. Random numbers are a query
(`rng.next`), so a replay gets the original's numbers and the port can
use its own generator. Commands are usually where one feature ends and
another begins: Croc's swipe is his, and how an enemy takes the hit is on
the far side of the command.

A mechanic is captured once the read audit is clear, the traces cover what
the feature's scenarios exercise, and the uncovered branches are listed.

**Size.** Any piece that brackets cleanly will do: one start and one end,
state that other code leaves alone between steps (few external changes),
and a clear audit. That may be part of a routine (ending at the
instruction before a world query), one routine, or a whole per-tick update
covering several behaviours. The size is a practical choice, not a rule.
Smaller mechanics make a failure point at a smaller piece of code; larger
ones need fewer bindings. Player-named behaviours (walk, jump) are the
port's structure, not the evidence's: the port splits a large mechanic's
step into states and systems, and the traces still replay the whole.

Each mechanic belongs to exactly one feature. If two features would need
the same mechanic, the code entangles them: they are one feature, or one
depends on the other. Interfaces, commands included, are where features
meet.

**Where capture runs.** On psx's native core, in process, like every
other run. The core reports each instruction's data access, and the CPU's
and GTE's registers are readable between instructions. Register and GTE
reads before writes are found by decoding each instruction as it steps.
Mednafen (psx-oracle) stays what it is: the reference the core is
calibrated against, never linked or run by psx. The core runs the lockstep
suite cycle for cycle with it, so a capture is Mednafen's behaviour; a
difference found later is fixed in the core.

### Scenarios

A scenario is a situation the port can reach by itself, named in game
terms:

```toml
# scenarios/time_trial_straight.toml
feature = "F-0007"
moment = "the first tick the player has control"
chain = "states/time_trial_straight.json"   # how the original gets there
[parameters]                                  # how the port gets there
mode = "time_trial"
course = { asset = "course/high_speed_ring" }
car = { asset = "car/0142", setup = "default" }
```

- **The original** reaches it by a state chain from power-on, as now.
- **The port** reaches it through a scenario launcher it registers per
  scenario kind. The launcher builds the world from the parameters with the
  port's own loading.
- **Determinism check.** psx reaches the scenario by a second route (idle
  fields added, or after another scenario) and compares. A difference
  means hidden state that is not a parameter, such as a random seed taken
  from boot time. It becomes a parameter.

Scenarios serve two purposes: they are where traces are recorded, and they
are the integration test (section 6).

**Isolation.** A feature's scenario may only depend on features in the
goal's closure. A time trial isolates car handling from AI and race rules;
a race does not. When the original's quantities in a scenario are changed
by something outside the closure (external changes, or the port diverging
where its own mechanics all replay exactly), that something is catalogued:
either the goal grows to include it, or the scenario is replaced by one
without it.

### Glossary and observation channels

Scenario comparisons use named **quantities**: a car's position, speed,
gear; the lap time; the selected menu item. The glossary gives each a
kind (`quantity`, `asset`, `event`), units, and the binding that reads it
in the original. The port registers a getter per quantity. The comparison
never depends on how the port stores the value.

| Channel | Original side (psx) | Port side (template hook) | Compared |
|---|---|---|---|
| **quantities** | glossary bindings, sampled per tick | the quantity registry | per tick, exact or within tolerance |
| **screen items** | per-field GPU capture, kept to primitives that use a named asset group (the HUD font, HUD sprites), each as content hash, rectangle in the original's frame, colour, blend and order | 2D items (sprites, text, UI) in the same reference frame, hashed the same way from the imported asset | as a set per tick, plus the order of overlapping items |
| **sound events** | SPU key-on/off with the sample's hash (`trace_spu`), pitch, volume, pan; XA and CD audio starts and stops | the sound plugin reports each played sound by its imported asset's hash | as a list per tick |
| **screens** | snapshots | screenshots in the reference frame | not compared mechanically; for looking |

- **3D is not compared as drawing.** Camera and object transforms are
  quantities, and geometry comes from formats.
- **Image identity is content.** Both sides decode with the template's
  `vendor/psx-assets`, so the hashes of the same texels match whatever the
  port's atlas layout.
- **Text** is a run of glyph items. The font's format gives the
  glyph-to-character mapping, so a comparison can show `"LAP 2/3"` at a
  position.

### Recordings and their files

Traces and scenario recordings are columnar, store changes only, and are
summarised by `psx case summary` (per column: range, distinct values,
rows that change). Nobody reads a raw trace to review it. This removes the
1 MB cases of today's format, where every write was its own pretty-printed
object.

### Formats

A format note describes a disc file format, and its importer converts it.
An importer test takes a real file from the disc and checks the importer's
records against a records CSV. Each record type the format describes
appears in it. Where the game loads the file into RAM, the CSV is recorded
from what the game holds after loading, so the test does not depend on
the agent's own reading of the format. GT2's FMT-0001 to 0004 become
format notes and importer tests.

## 5. The port's architecture

Required for every goal; they are what make features reusable.

1. **One plugin per feature**, named after it, declaring the feature
   plugins it depends on (matching the catalogue), usable with only those.
2. **Logic and rendering apart.** Logic runs in `FixedUpdate` at the game's
   tick rate and never reads the window, the camera's pixels or the frame
   rate. Rendering reads logic's state and may interpolate between ticks.
   The PS1 look is one render plugin (psx-bevy's, with its `PsxLook`
   switches); a modern renderer is another, with no change to logic.
3. **A fixed reference view.** Rules the original ties to the screen
   ("wakes when on screen", HUD layout) are defined against the original's
   4:3 view. `PsxAspect` widens what is shown and `PsxAnchor` anchors the
   HUD, without changing behaviour.
4. **Mechanics as pure steps.** Each mechanic is a function of its state,
   inputs, params and world, called from a system that adapts components
   to it. The world is the mechanic's interfaces as a trait, implemented by
   the feature over the port's own resources. The step is the port's own
   code, not a separate reference: the test harness calls the same
   function through a small adapter between trace fields and the port's
   types.
5. **Numbers.**
   - **Generic over a scalar.** Exact mechanics are written once, generic
     over a `Scalar` trait, which the template implements for
     `Fixed<W, F>` and for `f32`. The port instantiates the fixed type; a
     new game can instantiate `f32` with a type change.
   - **Name the rule, then convert the units.** A constant is written as
     the number it is, with a doc comment giving the raw value and where it
     was read.
   - **Integer behaviour that carries meaning is a named operation** on
     `Scalar`, never raw shifts and masks in a system. Examples: angles
     that wrap (`Angle`), rounding toward minus infinity, a speed that
     truncates to zero (`settle_to_zero`), keeping only the integer part
     (`snap_to_unit`), a table-based sine. `Fixed` implements each exactly
     as the original does it; `f32` implements what it is for (a stop
     threshold, `rem_euclid`, `f32::sin`).
   - **Faithful mechanics** may use `f32` directly.
   - Conversion to `f32` for rendering happens once, at the render
     boundary.
6. **Rules apart from content.** Tuning, setup and content are assets with a
   documented contract, never constants in code. Quirk switches are
   settings.
7. **Assets in modern formats, round-tripping.**
   - Importers convert the disc's formats once, on first start, to glTF
     (with game data as glTF extras so Blender can edit it), PNG, WAV/OGG
     and RON.
   - Assets store real numbers, not raw bits: `grip = 1.375`, not
     `0x16000`. A fixed value always has a power-of-two denominator, so its
     decimal form is exact. Loading as `Fixed` is lossless and refuses a
     value that is not representable; loading as `f32` just works.
   - Each asset type loads and saves, and a test checks that loading and
     saving an unedited asset gives the same file. Nothing writes the
     original's formats.
8. **The tick stays fixed.** The game's tick rate is fixed in the port, in
   both the exact and the `f32` versions. Per-second values are given for
   reference. Moving to a variable time step would change the integration
   and the feel, and is out of scope.
9. **Test hooks** from the template: the quantity registry, scenario
   launchers, the mechanic trace adapter, the screen-item and sound-event
   reporters, and the importer test runner.
10. **A sandbox per feature.** `cargo run --example sandbox_F-0007` builds a
    small scene from hand-made assets only, with the feature and its
    dependencies. psx checks that it builds and runs headless under
    scripted input without errors, and that none of its assets are
    importer outputs.

Chosen per goal, suggested for a complete port, never required: an
engine/game split, mod crates, the original's whole front end (menus,
saves, options), an editor.

## 6. Exactness and tests

Each mechanic is **exact** (the default) or **faithful**:

| | Arithmetic | Step replay | Scenario |
|---|---|---|---|
| **exact** | `Fixed`, the original's width and rounding | every field, every step, tolerance 0 | compared quantities every tick, tolerance 0 |
| **faithful** | `f32` allowed | each step starts from the trace's recorded state and is compared within tolerance | within the scenario's tolerances |

Tests run in three layers:

1. **Step replay.** `cargo test`, with no emulator: milliseconds per trace.
   A mismatch names the mechanic, the step and the field. Calls to the
   world are checked in order, with the recorded responses.
2. **Scenario.** The port runs the scenario from the same inputs and its
   quantities, screen items and sounds are compared with Mednafen's,
   replaying the original's recorded tick schedule (including the ticks it
   ran unevenly when drawing fell behind). This catches what lies between
   mechanics: system order, timing, more than one step per tick.
3. **The `f32` drift report.** For each exact mechanic, psx also runs the
   `f32` instantiation against its traces, resetting from the recorded
   state every K steps, and reports the worst error and how it grows.
   This tells the person what switching would cost before they lift a
   mechanic into a new game. It is also how a borderline mechanic is
   decided: if the error stays bounded, faithful is enough; if it grows,
   the mechanic is exact.

Verdicts are written only by psx, which runs the check itself (`psx port
check`), never by the agent. Each records the time and the commit it
judged, and counts only while that commit is HEAD and the check ran with
no uncommitted changes. The checks need no emulator on the port's side
(the original's recordings are kept), so re-running them after any commit
is cheap, and no per-feature file tracking is needed. Verdicts live in
`.psx/verdicts/`, outside the commit, so recording one does not make it
stale.

## 7. Structure review

Correctness is entirely mechanical (section 6). The structure review
answers a different question: is this good Rust and Bevy? It has three
layers.

1. **Signals psx computes**, shown by `port next`:
   - one Rust function per original routine with the same call shape;
   - resources or globals that map one to one onto RAM addresses;
   - hex literals with no named constant and citation;
   - raw shifts and masks outside `Scalar` operations;
   - constants in code that the original read from a table.

   A signal is not a failure. The agent either changes the code or writes
   one line on why the shape is right ("one pure step, the same as the
   original; it is the natural unit"). Only shapes that are wrong for Rust
   need changing: dispatch tables standing in for an enum, globals
   standing in for components, numbers with no name.
2. **A self-review** at the end of each mechanic, against a short
   checklist of what signals cannot see: does each system read as a rule
   someone would recognise, are states enums with their data in the
   variants, are params assets.
3. **A fresh-context review per feature.** A short subagent sees the
   design note, the feature's code and the signals list, and never the
   disassembly, so similarity to the original is invisible to it. It
   answers one question: is this idiomatic, and are the flagged shapes
   justified? Its findings go to the agent, which fixes them or replies.
   A feature passes review when the reviewer's latest pass has no open
   findings at the code's current commit.

   `psx port review F-0007` prints the reviewer's brief and the file
   list. Where the client has subagents, the agent starts one with it;
   where it does not, the person opens a fresh session with the brief.
   Either way the result is written to the feature's review file, which
   is what the stage check reads.

Mirroring the original is expected inside a mechanic's step, where the
order of operations is the behaviour. The structure matters most one level
up: how mechanics are composed into systems, what is an asset and what is
code, and how the world is reached through interfaces. That is what
decides whether a feature can be lifted into another game.

## 8. Attribution and where to look next

psx attributes the original's code to features and mechanics from traces,
instruction by instruction. It never relies on function boundaries, so
hand-written assembly works the same as compiled code.

1. **Mechanics** account for the instructions inside their brackets
   directly.
2. **Every scenario has a baseline:** the scenario run with no input. The
   code a feature's recordings run that the baseline does not is
   attributed to the feature. A feature that is a scenario's idle
   behaviour (the AI cars, the HUD clock) lists the baseline as its own.
3. **Platform code** (the BIOS, and SDK library code found by psx's
   signatures) is classified as platform and carries no weight. The agent
   can classify the rest by hand: dead, debug, or platform that signatures
   missed. It can also mark code **not reached: noted**: read, understood,
   but run by no recording (a two-player branch nobody played). Noted code
   is listed separately and counts toward nothing.

   Nobody credits code to a feature by hand. Credit comes only from
   traces, so the percentage stays a measurement (Decision 2).
4. **The corpus** is the person's play-throughs, recorded with tracing.
   The goal's universe is the code they run in the goal's areas.
5. **The gap list** is the code the corpus runs that no feature or
   mechanic accounts for and nothing classifies. It is grouped by region of
   code, and each region comes with the play-through and field where it
   first ran, a screenshot of that field, and the input around it. That
   shows what the player was doing, which is how unfound features are
   found.

The inventory and coverage stay as they are, for finding code (overlays,
decompressed code, code built at runtime).

## 9. Stages, progress and `port next`

Each feature has a stage. Each stage's check is a fact psx reads from
files.

| Stage | Holds when |
|---|---|
| 1 catalogued | it has a catalogue entry |
| 2 captured | its mechanics are captured (read audit clear, uncovered branches listed) and its scenarios are deterministic |
| 3 implemented | its plugin exists and registers its scenarios' launchers and quantities |
| 4 verified | step replay and scenario verdicts pass, fresh |
| 5 reviewed | the structure review has no open findings at the current commit |
| 6 extracted | its sandbox passes and its assets round-trip |

**The goal is complete when every goal feature is extracted and the gap
list in the goal's areas is empty.**

`psx port status` shows the goal's features by stage, grouped by area,
with dependencies; the percentage of the corpus's code accounted for,
weighted by instructions; the gap list; and the `f32` drift per exact
mechanic.

`psx port next` gives the next item. Work lives on disk, so any session can
pick it up, and the person can redirect at any point. Items, in order:

| Item | Due when | Done when |
|---|---|---|
| **Confirm** (person) | the goal file has no confirmation covering its current list | a confirmation matches the list |
| **PlayThrough** (person) | the gap list needs a situation the agent cannot reach | the new recording is in the corpus |
| **Fix** | a fresh verdict fails | a fresh verdict passes |
| **Review findings** | the feature reviewer left open findings | none open |
| **Catalogue** | the gap list in the goal's areas is not empty, or no goal is confirmed | every gap region is a feature or classified |
| **Capture** | a goal feature names no mechanics, or one is not captured | named and captured, as stage 2 |
| **Scenario** | a feature has no deterministic scenario | the determinism check passes |
| **Implement** | a captured feature has no passing verdicts | verified |
| **Signals** | a feature has unanswered structure signals | each changed or justified |
| **Review** | a verified feature has no review at its current commit | a review exists |
| **Assets** | an imported asset type has no contract or round trip | both exist and pass |
| **Extract** | a reviewed feature has no sandbox verdict | the sandbox verdict passes |

Until the person confirms the goal, only Catalogue and Confirm are handed
out: Catalogue while the goal or catalogue has problems, then Confirm. A
confirmation covers the list while every confirmed feature is still in the
goal's closure. Features are taken in dependency order, formats first. A loop guard
reports an item that comes back unchanged three times instead of handing
it out again. The facts are gathered once per call, and each item's
done-when is a fact the work leaves behind, never a session's say-so.

An unattended mode (a loop that starts sessions on `port next` with no
person) can come later on the same items. It is not part of this design.

## 10. What psx and the template need

**psx:**

1. `port init` for a single workspace (no role repositories), `port
   confirm`, `port status`, `port next`, the facts and the item list.
2. The goal and catalogue files, the dependency closure, feature stages.
3. `mechanic capture` for the R3000: bindings with register-relative
   locations, overlay-qualified PCs, register filters, the end modes,
   interface calls (by entry PC, and inline reads), external changes,
   branch coverage, and the read audit with CPU and GTE register reads,
   drafted by reading PC. It runs on the native core.
4. Scenarios and the determinism check.
5. The glossary, recordings that store changes only, and `case summary`.
6. The screen-item and sound channels.
7. Importer tests from records the game holds after loading.
8. Attribution: mechanic brackets, baselines, platform classification by
   signatures, and the gap list with first-run moments and screenshots.
9. Structure signals.
10. Verdicts keyed by the hashes of what they judged.
11. The guide rewritten for one agent, short (also the token-usage item in
    memory).
12. Removal of the clean-room commands (settled, Decision 1).

**Template (psx-bevy and the workspace template):**

1. `Scalar`, `Fixed<W, F>`, vectors, `Angle`, the named integer operations
   for both `Fixed` and `f32`. In psx-bevy, an exact `gte` module (the
   GTE's operations, flags and rounding) from psx's own `psx-gte` crate,
   written once for every PS1 port.
2. The mechanic trace adapter and the step-replay test harness, with the
   world-call check and the `f32` drift run.
3. The quantity registry, scenario launchers, and the screen-item and
   sound-event reporters.
4. Feature plugin conventions (dependencies declared and checked) and the
   sandbox per feature.
5. Asset contracts, real-number asset fields with exact `Fixed` loading,
   and the round-trip helper. glTF extras for game data.
6. Logic and render separation with interpolation, and the reference view.
7. `AGENTS.md` for one agent: the TR2 port's "name the rule, then convert
   the units" and evidence-citation guidance, the structure review, and
   the per-goal choices as suggestions.

## 11. The order of work

Each step is one pull request and ends with something that runs. Sizes are
rough working sessions: **S** about one, **M** two or three, **L** four to
six. The pilot decides whether Phases C and D go ahead as written.

### Phase A: the workspace and the loop (about 9 to 11 sessions)

| Step | Size | What |
|---|---|---|
| A1 | M | Single-workspace `port init`, `goal.toml`, catalogue files, `port confirm`, the dependency closure, feature stages. |
| A2 | M | `port status` and `port next`: facts gathered once, the items that need no capture (Confirm, Catalogue, Implement, Fix, Extract), done-when checks, the loop guard, verdicts keyed by hashes. Catalogue's done-when is "every goal feature has an entry" until D1 adds the gap list. |
| A3 | M | The guide and `AGENTS.md` for one agent, short. The template's layout reset: the engine/game split, editor and mods become per-goal suggestions. |
| A4 | M | Remove the clean-room commands, their guide topics, role templates and crates (Decision 1). |

### Phase B: mechanics (about 17 to 21 sessions)

| Step | Size | What |
|---|---|---|
| B0 | S | The mechanic, binding and trace formats (Decision 4): their definition in psx's documentation and their schemas. |
| B1 | L | `mechanic capture` core on the native core: `mechanic.toml` and `binding.toml` with the R3000 vocabulary and overlays, brackets, end modes, register filters, interface calls (entry PC and inline reads), refusals, the trace writer. |
| B2 | L | Capture's analysis: the read audit (memory, CPU and GTE registers) drafted by reading PC, external changes, branch coverage. |
| B3 | M | Template numbers: `Scalar`, `Fixed`, vectors, `Angle`, the named operations for both types, real-number asset fields; psx-bevy's `gte` module from `psx-gte`. |
| B4 | M | Step replay: the trace adapter, world-call checks, the drift report; the Capture item in `port next`. |
| B5 | M | What the pilot needs from scenarios: scenario files, port launchers, the determinism check, the quantity registry and comparison. |

### Pilot (about 6 to 10 sessions)

The pilot is Croc's movement, all of it (Decision 3). It runs from goal
to extracted on a fresh workspace, with the person reviewing the result.

- **It passes if:** the mechanics replay exactly; the port reads as
  ordinary Bevy; the drift report is believable; the sandbox works on
  hand-made assets; and `port next` led the work without the person
  steering it item by item.
- The pilot's findings change this document (S) before Phase C.

### Phase C: formats, review and channels (about 12 to 15 sessions)

| Step | Size | What |
|---|---|---|
| C1 | M | Importer tests from records the game holds after loading; asset contracts and round trips. |
| C2 | M | Structure signals and `port review`. |
| C3 | L | The screen-item channel. |
| C4 | M | The sound channel. |
| C5 | S | Recordings that store changes only, and `case summary`. |

### Phase D: the rest (about 6 to 8 sessions)

| Step | Size | What |
|---|---|---|
| D1 | L | Attribution: mechanic brackets, baselines, platform classification, "not reached: noted", the gap list with first-run moments and screenshots; Catalogue's done-when gains the gap list. |
| D2 | S | Quirk settings. |
| D3 | S | The whole-game view. |

In all, about 51 to 66 sessions. A calendar estimate is better made after
Phase A, from the pace it actually took.

## 12. The platform boundary

The method should later work for other sources (Mega Drive through mdx,
DOS and older PC games), so mechanics from different games can be mixed.
It is proven in psx first and split out only once a second producer
uses it. Until then, psx keeps the two sides apart in its code.

**Generic (the method):** the workspace, goal, catalogue, stages, `port
status` and `port next`; `mechanic.toml`, the trace format and step
replay; `Scalar` and `Fixed`; the drift report; the structure review;
attribution's arithmetic and the gap list; and the core of the template.

**Per producer (the platform):**

- running the game deterministically from a saved state with replayed
  input;
- `binding.toml`, whose location vocabulary is the platform's (R3000
  registers and overlays, 68000 registers, x86 modules);
- capture: brackets, interface calls, the read audit, external changes,
  branch coverage;
- executed code per recording, as **code regions with an identity** the
  method can compare across runs (address plus overlay for the PS1, ROM
  address plus bank for the Mega Drive, module plus offset for x86);
- platform classification by the platform's own signatures (PsyQ for the
  PS1, compiler runtimes and libraries elsewhere);
- screenshots, sound events and asset extraction;
- all platform investigation (disassembly, Ghidra, memory search).

Attribution's rules (credit from traces only, the classifications, "not
reached: noted") are the method's. The producer only supplies the code
regions each recording ran and the platform classification.

**Mixing mechanics** from different games is the combining game's work,
not the method's. The method makes it possible by making each mechanic
state its native tick rate and units in `mechanic.toml`, keeping params in
real units in assets, and keeping the `f32` instantiation available.

## 13. Existing ports

- **Croc** (`external_agent_tests/croc`, not edited): the pilot starts a
  fresh workspace. Its inventory, coverage, start-state chains and field
  map carry over as evidence; function-level specs and the clean code
  built on them do not.
- **GT2** (`external_agent_tests/gran_turismo_2`, not edited): a fresh
  workspace when it is next picked up. What carries over as evidence:
  - the inventory, coverage and start-state chains (scenario chains);
  - the field map, whose entries become glossary bindings and mechanic
    bindings;
  - FMT-0001 to 0004, as format notes and importer tests.

  SPEC-0001 to 0003 and the clean repository's code built on them are not
  carried over. They test internal routines with poked fixtures, and the
  behaviour comes back as parts of real features (loading a course, the
  menu text).
- **Tomb Raider II** (the person's repositories, not edited): a port under
  this model can start from its investigation and code as evidence. Its
  numeric rules (section 5's naming and helpers) came from there.

## 14. Checking the design against the goals

1. **Port what the person wants.** The goal, the catalogue the person
   confirms, the dependency closure, and done defined by psx as extracted
   with no gaps.
2. **Reusable features.** Plugins with declared dependencies, mechanics
   behind interfaces, rules apart from content, round-tripping assets, and
   the sandbox checked by psx.
3. **Exact where it matters.** Step replay at tolerance 0 for exact
   mechanics, with the read audit and external changes making sure the
   state is complete, and scenarios checking how mechanics combine.
   **Limit:** exactness is relative to Mednafen, which is ground truth by
   choice (the native core that captures runs cycle for cycle with it), and only as wide as the traces' branch coverage. The uncovered
   branches are listed, not hidden.
4. **Modern, idiomatic code.** The structure review judges the code as
   Rust and Bevy, not by its distance from the original. **Risk:** the
   agent reads the original's code, so its default is to transliterate.
   The signals catch the common forms, and the fresh-context reviewer
   cannot see the original. The pilot tests whether that is enough.
5. **Not locked to the PS1.** Separate render plugins, the reference view,
   `PsxAspect`, `PsxAnchor` and `PsxLook`. Mechanics generic over `Scalar`,
   with the drift report saying what `f32` costs.
6. **Progress and where to look next.** Feature stages, the accounted
   percentage, and the gap list with moments and screenshots.
7. **General.** Attribution is per instruction, bindings name overlays and
   registers rather than functions, and the read audit covers hand-written
   code that keeps state in registers. **Limit:** the screen-item channel
   assumes the HUD uses textured primitives from identifiable assets.
   Untextured elements match by rectangle and colour alone.
8. **Cheap to run.** One session, state on disk, done-checks as facts, the
   loop guard, columnar recordings with summaries, and step replay with no
   emulator. **Known cost:** capturing a mechanic costs more than porting
   it untested (mdx's Ren & Stimpy measurement: about 3.8 times the input).
   Most of that was the knowledge base's own files and writing the read
   audit by hand, which this design avoids. The rest bought correctness
   that the untested port lacked.

## Decisions for the person

1. ~~**The clean-room commands in psx.**~~ Settled: remove them (protocol,
   publish, barrier, audit, clean mode, `port run` with roles) once Phase A
   lands, with their guide topics and role templates. Existing role-based
   workspaces keep working with the last release that had them.
2. ~~**Manual attribution.**~~ Settled: credit from traces only. The agent
   can classify code (platform, dead, debug) and mark it "not reached:
   noted", but never credit it to a feature by hand (section 8). The
   producer supplies code regions and platform classification; the rules
   are the method's (section 12).
3. ~~**The pilot.**~~ Settled: Croc's movement, all of it (walking,
   jumping, collision, attacking and the rest of the player's controls),
   on a fresh workspace. Croc's existing repositories are evidence and are
   not edited.
4. ~~**The trace format.**~~ Settled: psx's own formats for mechanics,
   bindings and traces, adapted from the mechanics knowledge base's draft
   (version 0). The knowledge base itself is not used, and mdx is left as
   it is until psx's method is proven. Changes from the draft:
   - a `table` type for params (element type and length, values in the
     binding or in an imported asset);
   - interface calls of kind `query` (request and response) or `command`
     (request only, still checked in order), for acting on the world;
   - random numbers as a query interface (`rng.next`);
   - standard `audit` and `coverage` sections in the trace header, with
     producer-opaque locations, so stage checks can read them;
   - the default byte order taken from the platform;
   - step lines that record only the fields that changed;
   - a `scenario` field in the header;
   - a `float` type carried as its bit pattern (32, 64 and 80 bits).

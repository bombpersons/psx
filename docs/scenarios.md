# Scenarios

A scenario is a situation both the original and the port can reach, named in
game terms: "Croc standing at the start of 1-1". It is a feature's
integration test. psx records the original's quantities in it tick by tick,
and the port runs the same pad input from the same moment and must give the
same quantities, tick by tick ([port-design.md](port-design.md)).

The files:

- `crates/psx-scenario`: the scenario file and recording formats, the
  glossary read from its crate (`rust.rs`) and the generators for its
  binding's glue and the port's `Observed`, their checks, and JSON Schemas
  in `crates/psx-scenario/schemas/`.
- `crates/psx-capture`: the `Glossary` trait the glossary's binding
  implements.
- `crates/psx/src/scenario.rs`: `psx scenario record`, `psx scenario check`,
  `psx scenario show` and the facts `port next` reads; `crates/psx/src/binding.rs` builds and
  loads the glossary's binding.
- The template's `psx_bevy::scenario`: the launcher and observer registry
  and the runner, used by the game's `tests/scenarios.rs`.

## 1. The glossary: `glossary/`

The glossary holds the names quantities go by, and where the original keeps
them. A scenario compares quantities by name, never by how the port stores
them. It is two layers: the quantities, plain Rust in game terms, and their
binding, a program that knows the PlayStation.

```
glossary/
  Cargo.toml        # package glossary; no dependencies
  src/lib.rs        # Quantities
  binding/          # package glossary-binding, a cdylib: the tick and where each quantity is
```

```rust
//! Croc (Europe) SLES-00593: the quantities scenarios compare.

pub struct Quantities {
    /// Croc's position across the level
    /// Unit: world units
    pub croc_x: i32,
    /// Croc stands on the ground; false through a jump
    pub croc_on_ground: bool,
}
```

- **`Quantities`** declares each quantity as a field: its type (`bool`,
  integers, fixed-point aliases, enums, floats; no tables, lists or
  records), its description and `Unit: …` line in its doc comment. The
  crate keeps to a portable subset of Rust (no macros of its own, no
  generics, no unsafe code), with no dependencies. The template seeds it
  empty.
- **The binding**, `glossary/binding/`, is a `cdylib` crate psx builds and
  loads. It depends on `glossary` and `psx-capture`, and its `build.rs`
  calls `psx_scenario::rust::write_glossary_glue("..")`:

  ```rust
  use glossary::Quantities;
  use psx_capture::prelude::*;

  mod glue {
      include!(concat!(env!("OUT_DIR"), "/glue.rs"));
  }

  /// The player object, the pointer the camera code reads Croc through.
  const PLAYER: u32 = 0x8007_528C;

  struct Croc;

  impl Glossary for Croc {
      type Glue = glue::Glossary;
      const TITLE: &str = "Croc (Europe) SLES-00593";
      const IMAGE_SHA256: &str = "…";

      /// The logic step: the main loop's `accumulator -= 256` at the end of each step.
      fn tick(&self, t: &mut Tick) {
          t.store(0x8007_5240, 4).by(0x8001_3CA8);
      }

      fn quantities(&self, m: &Machine) -> Quantities {
          let croc = m.u32(PLAYER);
          Quantities { croc_x: m.i32(croc + 0x370), croc_on_ground: m.u32(croc + 0x178) & 2 != 0 }
      }
  }

  psx_capture::export_glossary!(Croc);
  ```

  - **`TITLE` and `IMAGE_SHA256`** are the release. Recording refuses media
    with another identity.
  - **`tick`** is the store that ends one step of the game's logic: a store
    of `bytes` bytes to an address, made by the instruction at `by` when it
    is given. The quantities are read right after it.
  - **`quantities`** reads every quantity from the machine, anew at every
    tick, so an object found through a pointer, a list walk or a value that
    means something only while a flag is set is ordinary code. Only main
    RAM and the scratchpad can be read; a read refused (a null pointer) is
    the recording's refusal.
  - **The glue** carries the glossary's fingerprint; psx refuses a binding
    built from other quantities.
- **Bind the logic step, not the render loop.** Many games draw every
  field but run their logic every second field, or draw as often as they
  can. The loop's own counter then ticks at the drawing rate, and runs
  unevenly when drawing falls behind. A logic quantity such as a position
  changes only on the logic step.
- **How to find it.** Run `run --find-tick` while the player moves and the
  scene is busy. The logic step's group stores at the logic rate even when
  the drawing rate changes. Check that a compared quantity changes only on
  that store, and pick a store at the end of the step.
- `glossary.toml`, the earlier format, is no longer read.

## 2. Scenario files: `scenarios/<id>.toml`

```toml
feature = "F-0001"
moment = "Croc standing at the start of 1-1, the first step he can move"
chain = "notes/states/level_1_1.json"   # how the original gets there
kind = "level"                           # the port's launcher
idle = 60                                # the determinism check's idle fields (default 60)
[compare]                                # quantity = tolerance, in raw units
croc_x = 0
[parameters]                             # how the port gets there
level = "1-1"
```

- **`chain`** is a `psx states` chain. The original starts from the state it
  made, which must be the one in `.psx/states/` (`psx states rebuild` puts it
  back).
  - **End the chain at a game moment.** Its last run should stop with
    `until` on the game's own state: a level timer, the first step the
    player has control. A count of fields after a button press does not
    work. Disc reads take a little longer or shorter depending on the
    drive's state, so the same input reaches a level a field or two sooner
    or later. A chain that stops after a fixed number of fields then lands
    at a different moment of the game.
- **`kind`** names the port's launcher. The feature's plugin registers it,
  and it builds the world from `parameters` with the port's own loading.
- **`compare`** lists the glossary quantities compared, each with its
  tolerance in raw units. Use 0 for an exact feature. A bool or enum is
  always compared exactly.
- **The catalogue entry** lists the scenario's id in its `scenarios`.
- **Isolation.** A scenario may only depend on features in the goal's
  closure (port-design.md, Scenarios).

## 3. Recordings: `scenarios/<id>/<name>.jsonl`

`psx scenario record ID --name NAME --frames N [--input|--replay]` runs the
game on psx's native core. It starts from the scenario's state with the pad
released until the first tick ends, then records N more fields holding the
pad as given (the input's frames count from the field after that tick). It
builds the glossary's binding, stops at each tick store and keeps the
compared quantities. Analog sticks are refused: the port's runner has
digital buttons only.

It also traces the run and keeps what the original executed beside the
recording, in `scenarios/<id>/<name>.coverage.json`, credited to the
workspace's inventory (`psx code inventory`, which must exist first) as
`code coverage` credits a traced run: per executable or overlay, the
address ranges run and how often, the ways each conditional branch went,
and a hash of the instructions run. No instruction is kept, so the file
is committed with the `.jsonl`. `psx code coverage` and `code coverage
diff` take the recording's `.jsonl` as one of their runs, so a feature's
coverage is the union over its recordings without running them again.
An inventory taken again keeps crediting a recording while it holds the
same code where the recording ran; where it does not (an overlay unpacked
since), coverage says so and the recording is made again. Code the
inventory held only partly when recording (an overlay known only from the
code that ran, or none) is in the record's result: unpack it first
(guide topic code), then record.

```json
{"format": "scenario-recording", "format_version": 1, "scenario": "level_start",
 "source": {"kind": "captured", "platform": "psx", "image_sha256": "…"},
 "producer": {"name": "psx", "version": "0.8.0", "details": {"chain": "…", "frames": 100, "input": {…}}},
 "quantities": {"croc_x": {"type": "int", "bits": 32, "signed": true}},
 "initial": {"croc_x": 12288},
 "description": "Walk right for two seconds, then stand"}
{"tick": 1, "field": 1, "pad": ["UP"], "values": {"croc_x": 12293}}
{"tick": 2, "field": 3, "values": {"croc_x": 12301}}
```

- **`initial`** holds the quantities at the end of the recording's first
  tick, which runs with the pad released. That is the moment the port's
  launcher builds.
- **Each later line is one tick.**
  - `field` is the field the tick ended in, counted from the field after
    the first tick (field 0, the input's frame 0): the original's schedule. Two ticks in one field, or a field with
    none, is the original falling behind.
  - `pad` holds the buttons held during that field, when they changed.
  - `values` holds the quantities that changed.
- **The header carries the quantities' declarations**, so the recording
  stands alone. A recording made before the scenario's `compare` or a
  compared quantity's type changed must be recorded again: the facts say
  so. A changed unit or description does not count.

`psx scenario show ID --name NAME` reads a recording back as a table: one
row per tick from tick 0 (`initial`), with the field, the pad and every
quantity, and beside each numeric one its change since the tick before
(a walk's speed, a jump's gravity). A fixed shows as its exact decimal and
a 32- or 64-bit float as its number; `--raw` shows the integers recorded.
`--quantities` and `--ticks A..B` narrow it, `--text` aligns it.

## 4. The determinism check: `psx scenario check [ID…]`

The check reaches the scenario a second way and records each captured
recording again with the same input:

1. It replays the chain from power-on, with `idle` fields of no input added
   before the chain's last run.
2. It records again from the state that run ends in, and compares tick by
   tick.

- **A difference in a value fails the check.** The difference names the
  tick and quantity. It is state the scenario's parameters do not hold,
  such as a random seed taken from the time since power-on: it becomes a
  parameter. Check the two usual mistakes first: a chain that ends after a
  count of fields, and a glossary tick on the render loop.
- **A difference in the schedule is a note, not a failure.** When a tick
  ends is timing no parameter holds: where the game's loop is when a field
  ends, or how long a disc read takes. The port replays the recorded
  schedule anyway. The note names the first tick that ended in another
  field. A recording made again may also have a tick more or fewer in the
  same fields.
- **Where the result goes.** The result is kept in `.psx/scenarios/<id>.json`
  with the sha256 of everything it judged: the scenario, its chain, the
  glossary and its binding, and the recordings. It counts only while those
  are unchanged.

## 5. In the port

The game's build generates `Observed` from the glossary (`game/build.rs`
calls `psx_scenario::rust::write_observed("../glossary")`; `game::observed`
includes it): one `Option` per quantity, of its type in the glossary crate,
in the original's raw units. Each feature's plugin registers its launchers,
and a function that fills the quantities it keeps
(`psx_bevy::scenario::ScenarioApp`):

```rust
use crate::observed::Observed;

app.register_scenario("level", |app, p| { /* build the level named p["level"] */ Ok(()) })
    .observe(|w: &mut World| Observed {
        croc_x: w.query::<&Croc>().iter(w).next().map(|c| c.x),
        ..Observed::none()
    });
```

- A misspelt or removed quantity is a compile error.
- A quantity left `None` is one the port has no such thing for (yet); a
  scenario that compares it fails at that tick and says so.
- Several plugins may each observe their own quantities; one that two
  observe is a failure.

The game's `tests/scenarios.rs` gives the runner a function that makes a
fresh headless app with the features' logic plugins. For each recording,
the runner:

1. makes an app and calls the scenario's launcher;
2. compares the observed quantities with `initial`;
3. runs one tick per line with the line's pad, and compares again.

Each quantity's first tick out of tolerance is reported.

`psx port check` runs that test and writes each feature's scenario verdict.
**A feature passes when:**

- it lists at least one scenario;
- each scenario runs (its launcher is registered);
- each recording on disk runs with no quantity out of tolerance;
- each scenario ran at least one captured recording.

## 6. Stages

- **Stage 2 (captured)** needs the feature's scenarios:
  - it lists at least one scenario;
  - each scenario's files check, and it has a captured recording;
  - each scenario passed `psx scenario check` as its files stand.

  `port next` gives the Scenario item until they do.
- **Stage 3 (implemented)** holds when the last `port check` at HEAD shows a
  launcher for each of the feature's scenario kinds, and each quantity they
  compare observed in a scenario the port launched.
- **Stage 4 (verified)** needs the scenario verdict to pass at HEAD.

A recording may take a branch whose effect does not reach a compared
quantity before it ends, so a port that took the other way could still
pass: compare more quantities, record longer routes, and hold routes out.

## 7. Not yet

- **The port's runner ignores the schedule.** It runs one tick per line and
  does not tell the port how many fields each tick took. A game whose logic
  scales by the fields elapsed needs that, and the pilot decides how.
- **Screen items and sound events** are later channels (port-design.md,
  section 4).

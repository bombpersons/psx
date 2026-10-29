# Scenarios

A scenario is a situation both the original and the port can reach, named in
game terms: "Croc standing at the start of 1-1". It is a feature's
integration test. psx records the original's quantities in it tick by tick,
and the port runs the same pad input from the same moment and must give the
same quantities. Mechanics ([mechanics.md](mechanics.md)) prove each step.
Scenarios prove what lies between steps: system order, timing, and more than
one step per tick.

The files:

- `crates/psx-mechanics`: the scenario file and recording formats, the
  glossary's platform-free shape, their checks, and JSON Schemas in
  `crates/psx-mechanics/schemas/`.
- `crates/psx/src/scenario.rs`: `psx scenario record`, `psx scenario check`,
  and the facts `port next` reads. The glossary's schema, with the R3000
  locations, is `crates/psx/schemas/glossary.schema.json`.
- The template's `psx_bevy::scenario`: the launcher and quantity registry
  and the runner, used by the game's `tests/scenarios.rs`.

## 1. The glossary: `glossary.toml`

The glossary holds the names quantities go by, and where the original keeps
them. A scenario compares quantities by name, never by how the port stores
them.

```toml
source = { platform = "psx", title = "Croc (Europe) SLES-00593", image_sha256 = "…" }
# The logic step: the main loop's `accumulator -= 256` at the end of each step.
tick = { at = 0x80075240, bytes = 4, pc = 0x80013CA8 }

[quantities.croc_x]
type = "int"
bits = 32
signed = true
unit = "world units"
description = "Croc's position across the level"

[bindings.croc_x]   # the player object, through the pointer at 0x8007528C
parts = [{ at = { pointer = 0x8007528C, offset = 0x370 }, bytes = 4 }]
stored_signed = true
```

- **`source`** is the release, as in a binding. Recording refuses media with
  another identity.
- **`tick`** is the store that ends one step of the game's logic: a store of
  `bytes` bytes to `at`, made by the instruction at `pc` when one is given.
  The quantities are read right after it.
  - **Bind the logic step, not the render loop.** Many games draw every
    field but run their logic every second field, or draw as often as they
    can. The loop's own counter then ticks at the drawing rate, and runs
    unevenly when drawing falls behind. A logic quantity such as a position
    changes only on the logic step.
  - **How to find it.** Run `run --find-tick` while the player moves and
    the scene is busy. The logic step's group stores at the logic rate even
    when the drawing rate changes. Check that a compared quantity changes
    only on that store, and pick a store at the end of the step.
- **`quantities`** declare each quantity's type as mechanic fields do
  (`int`, `fixed`, `bool`, `enum`, `float`; no tables). They also give its
  `unit` and a `description` in game terms.
- **`bindings`** say where each quantity is, as a binding's `Read` does
  (parts, mask, shift, `stored_signed`, `map`, `stored_frac_bits`). A part's
  `at` is one of:
  - `{ address }`: main RAM in any segment, or the scratchpad;
  - `{ pointer, offset }`: the address held at `pointer`, plus `offset`,
    read anew at every tick (an object found through a pointer).

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
stops at each tick store and reads the compared quantities. Analog sticks are refused: the port's runner has digital
buttons only.

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
  glossary and the recordings. It counts only while those are unchanged.

## 5. In the port

Each feature's plugin registers its launchers and getters
(`psx_bevy::scenario::ScenarioApp`):

```rust
app.register_scenario("level", |app, p| { /* build the level named p["level"] */ Ok(()) })
    .register_quantity("croc_x", |w| w.query::<&Croc>().iter(w).next().map(|c| c.x));
```

A getter returns the quantity in the original's raw units, as a replay
adapter's field does (`psx_bevy::replay::Field`). `None` means the port has
no such thing now.

The game's `tests/scenarios.rs` gives the runner a function that makes a
fresh headless app with the features' logic plugins. For each recording,
the runner:

1. makes an app and calls the scenario's launcher;
2. compares the quantities with `initial`;
3. runs one tick per line with the line's pad, and compares again.

Each quantity's first tick out of tolerance is reported.

`psx port check` runs that test with the replay test and writes each
feature's scenario verdict. **A feature passes when:**

- it lists at least one scenario;
- each scenario runs (its launcher and getters are registered);
- each recording on disk runs with no quantity out of tolerance;
- each scenario ran at least one captured recording.

## 6. Stages

- **Stage 2 (captured)** needs the feature's scenarios as well as its
  mechanics:
  - it lists at least one scenario;
  - each scenario's files check, and it has a captured recording;
  - each scenario passed `psx scenario check` as its files stand.

  `port next` gives the Scenario item until they do, after Capture.
- **Stage 3 (implemented)** holds when the last `port check` at HEAD shows a
  launcher for each of the feature's scenario kinds, and a getter for each
  quantity they compare.
- **Stage 4 (verified)** needs the replay and scenario verdicts to pass at
  HEAD.

## 7. Not yet

- **The port's runner ignores the schedule.** It runs one tick per line and
  does not tell the port how many fields each tick took. A game whose logic
  scales by the fields elapsed needs that, and the pilot decides how.
- **Screen items and sound events** are later channels (port-design.md,
  section 4).

# Scenarios

A scenario is a situation both the original and the port can reach, named in
game terms: "Croc standing at the start of 1-1". It is a feature's
integration test. psx records the original's quantities in it tick by tick,
and the port runs the same pad input from the same moment and must give the
same quantities, tick by tick ([port-design.md](port-design.md)).

The files:

- `crates/psx-scenario`: the scenario file and recording formats, the
  glossary read from its crate (`rust.rs`) and the generators for its
  binding's glue and the port's `Observed`, their checks, JSON Schemas
  in `crates/psx-scenario/schemas/`, and the runner (`runner.rs`): the
  `Port` trait a port implements and the replay of each recording against
  it; and the port protocol (`protocol.rs`): `serve`, the port's side, and
  `ProcessPort`, psx's, which `psx port check` and psx web drive the
  port's program through.
- `crates/psx-capture`: the `Glossary` trait the glossary's binding
  implements.
- `crates/psx/src/scenario.rs`: `psx scenario record`, `psx scenario check`,
  `psx scenario show` and the facts `port next` reads; `crates/psx/src/binding.rs` builds and
  loads the glossary's binding.
- `crates/psx-scenario/src/replay.rs`: a recording drawn by the port a
  PNG per tick for `psx web`, and the game's `main`'s `--psx` and
  `--replay`.

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
- **The catalogue entry** names the scenario's recordings, as
  `<id>/<name>` under `recordings.work` or `held_out`; the scenario does
  not name a feature. `scenario record` and `play` say which entries name
  a new recording, or which name the scenario's others.
- **Isolation.** A scenario may only depend on features in the goal's
  closure (port-design.md, Scenarios).

## 3. Recordings: `scenarios/<id>/<name>.jsonl`

`psx scenario record ID --name NAME --frames N [--input|--replay]` runs the
game on psx's native core. It starts from the scenario's state with the pad
released until the first tick ends, then records N more fields holding the
pad as given (the input's frames count from the field after that tick). It
builds the glossary's binding, stops at each tick store and keeps the
compared quantities. The analog sticks are kept as the game read them:
that needs a DualShock (`pad dualshock` in the chain) in analog mode,
which the game switches to itself or the input does with a press of
`ANALOG` (the controller's mode button, as on hardware), and sticks moved
while it is not are refused. A recording's `pad` keeps only what the game
reads, so `ANALOG` is not in it; its effect is, in the pad's mode and the
sticks.

A recording is made of the original adjusted to the port's model
(docs/master-plan.md), unless `--stock`; it names the adjustments in
`producer.details.adjust` and keeps what they did in
`producer.details.adjusted`:

- **latch**: the game's pad changes only where a tick ends, to that tick's
  input. Input waits for the tick to end before it reaches the
  controller, and psx writes it into the game's pad buffer at once (the
  buffer it finds by following the controller's reply from the serial
  port into RAM, through each byte's mask: a copy, or an inverted one),
  and puts the buffer back when the interrupt handler polls into it
  during the tick. Every read the game makes in a tick then sees that
  tick's input, however its loop and the polls fall, so a tick's `pad`
  is exactly what the port's tick gets. `adjusted` keeps the buffer, the
  ticks and the lag frames: ticks in which no game code read the pad.
  `late_input` (only when there are any) lists the ticks in which a
  latched change waited a whole field (after the one it was latched in)
  with no poll into the buffer, and no game code read the buffer before
  the tick ended (a read sees the latch, so a game that reads its buffer
  itself is never late), with the count of such fields. A game that
  reads the pad only through its polls ran that field on the last poll's
  buttons, so the change can reach its rules a tick late: the original's driver
  delays it (GT2's pad library skips its poll while the serial port is
  busy). The port never copies that; the game is patched so input
  arrives every tick. `adjust-check` repeats each recording's
  `late_input`.
- **steady**: no tick overruns its fields. The CPU's own work costs a
  quarter of its clocks and the GPU draws without taking time, so where
  the console slowed down every tick still takes its usual fields;
  timers, the CD, the SPU and video timing keep the real clock.

Every recording keeps `fields_per_tick`, how many ticks took each number
of fields (stock, a game that slowed down shows more than one). Both
adjustments can change what a game does: `psx scenario adjust-check`
records a scenario's recordings again from its state, stock and adjusted,
driven by the same ticks, and reports each quantity's first tick where
they differ; `--adjust latch`, `--adjust steady` or `--adjust off` (the
scenario's `[off]` table) measures one alone, to tell which a difference
comes from, and each recording's `off_calls` names what was turned off
and how often it was called. A press taking effect a tick
sooner under latch is its expected cost; a value the stock game never
reaches is a rule that depends on timing, and that scenario is recorded
`--stock`.

A person's play (`scenario play`) runs the adjustments while they play,
as `scenario record` runs them, so they play the game the recording is
made of. `adjust` chooses which (`latch`, `steady`, `off`; none is the
stock game), so a person can feel what one changes: psx web's Play now
has a checkbox for each. The recording is made with the same set; one
`scenario record` cannot name (latch without steady, say) is reported as
such.

A play without a scenario (from power-on or a state) has no adjustment
unless asked; it can ask for steady, and for latch, which then takes the
glossary binding's tick. The boot and the menus may not run that tick (it
was found in the game's main loop), so there a play's latch lets input
through once ten fields pass without one, as on the stock game, and holds
it for ticks again when one ends; the play's status says which. Only a
play does this: a recording refuses a tick that does not end.

Each tick's `pad` is the controller's buttons when the tick ended. With
latch, that is what the game's buffer holds from then on; without it
(`--stock`), the controller's view leads or trails the game's by a field
that depends on the game's phase.

`psx run --pad-reads` shows where a game keeps its pad: the buffer an
interrupt handler fills, the copies the game makes, the polls, the code
that reads it and the fields it read nothing in.

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
{"tick": 2, "field": 3, "analog": true, "sticks": {"lx": 143}, "values": {"croc_x": 12301}}
```

- **`initial`** holds the quantities at the end of the recording's first
  tick, which runs with the pad released. That is the moment the port's
  launcher builds.
- **Each later line is one tick.**
  - `field` is the field the tick ended in, counted from the field after
    the first tick (field 0, the input's frame 0): the original's schedule. Two ticks in one field, or a field with
    none, is the original falling behind. A tick runs the fields after
    the tick before's through its own; latched, input from frame F is
    first read by the tick after the one field F is in.
  - `pad` holds the buttons held during that field, when they changed.
  - `analog` is whether the pad was in analog mode, when it changed
    (digital at the start): what the game read in the pad's ID each poll,
    which it can act on with the sticks at rest. The port's runner gives
    it as `ControllerState::analog`; live, a port keeps the pad analog.
  - `sticks` holds the stick axes the game read in that field that
    changed (`lx`, `ly`, `rx`, `ry`): raw bytes, 0 left or up, 255 right
    or down, 128 at rest, never deadzoned or rounded. With none, the
    sticks are at rest; on a pad not in analog mode they always are. The
    port's runner gives the game each tick's sticks
    (`ControllerState::sticks`), so it can apply the original's own
    deadzone and curve.
  - `values` holds the quantities that changed.
- **The header carries the quantities' declarations**, so the recording
  stands alone. A recording made before the scenario's `compare` or a
  compared quantity's type changed must be recorded again: the facts say
  so. A changed unit or description does not count.

`--from OTHER` starts at the end of the scenario's recording OTHER instead
of at its state, so a long play-through is recorded in parts; `--from
OTHER@TICK` starts after OTHER's tick TICK, so a short recording starts
near a moment of a play: its first tick is OTHER's tick TICK+1, so its
tick N is OTHER's tick TICK+1+N. The recording's first tick still runs with the
pad released, and its details name `from`, which the determinism check
replays first and the port's runner runs first (OTHER's ticks, through
those it continues, each followed by the released tick that started the
next). A held-out play is never a start: a recording from inside it
would show what it holds.

`psx scenario play ID --name NAME --request "…"` makes the recording from
a person's play (guide topic run). `--held-out` adds it to goal.toml's
`held_out`, and its answer leaves the play out; a play the person starts
in psx web is held out unless they give it to the agent. The request joins the project's play
queue and the call returns; the person plays it in `psx web` or with `psx
play`, or declines it with a note. The play starts where `scenario
record` starts counting fields, after the first tick. When the person
hands back, psx records their input as `scenario record` does, with its
coverage, and says whether it ended where the play did; `port next` gives
a Read item and `--status` returns the answer. psx commits the play's
recording and coverage (and goal.toml, for a held-out one) by
themselves, as it does when psx web holds a recording out or gives it
back, so the agent meets the person's files committed, not as changes
in its way; the commit leaves anything else changed or staged as it was.
The port's `cargo test` leaves the held-out plays to `port check`.

`psx scenario show ID --name NAME` reads a recording back as a table: one
row per tick from tick 0 (`initial`), with the field, the pad (and the
stick axes away from rest, when the recording moves them) and every
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
in the original's raw units. The game implements `psx_scenario::Port`:
it lists the scenario kinds it can launch, builds the world for one from
its parameters, runs a tick with a pad, and fills the quantities it keeps:

```rust
use crate::observed::Observed;

impl Port for Game {
    type Observed = Observed;
    fn kinds(&self) -> Vec<String> { vec!["level".into()] }
    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
        /* build the level named p["level"] */ Ok(())
    }
    fn tick(&mut self, pad: &Pad) { /* the game's rules, once */ }
    fn observe(&mut self) -> Result<Observed, String> {
        Ok(Observed { croc_x: Some(self.croc.x), ..Observed::none() })
    }
}
```

- A misspelt or removed quantity is a compile error.
- A quantity left `None` is one the port has no such thing for (yet); a
  scenario that compares it fails at that tick and says so.
- A quantity given twice in the observation is a failure. (A Bevy port's
  `psx_bevy::scenario` is a `Port` whose plugins each observe their own.)

The game's `main` hands its arguments to `psx_scenario::replay::main`
first, with a function that makes a fresh headless game with every
feature's rules. With `--psx` it serves the port protocol (below) on stdin
and stdout. `psx port check` builds the game and starts it with `--psx`,
by the commands the workspace's `psx-port.toml` names (cargo's without
one), and runs psx_scenario's runner against it on psx's side. Any program
that speaks the protocol is a port: examples/c-port is one in C, and psx's
guide topic `setup` gives the manifest and the protocol, and topic
`arithmetic` the original's integer arithmetic, which `psx port vectors`
writes test vectors for. For each recording, the runner:

1. launches the scenario in a fresh game;
2. compares the observed quantities with `initial`;
3. runs one tick per line with the line's pad, and compares again.

Each quantity's first tick out of tolerance is reported. Each recording
then runs again in another fresh game, and where the two runs part is a
failure too. The game's `tests/scenarios.rs` runs the same runner in
process (`cargo test -p game --test scenarios`), with the same results: the
quick check while working.

psx web draws the same replay with the game's renderer for the person to
watch: through the protocol, it launches the scenario, runs the
recording's ticks and asks for `Port::screen` after each, saved as
`<tick>.png`. A port that draws nothing yet writes `frames.json` with
`no_screen`, and psx web says so. `cargo run -p game -- --replay
scenarios/<id>/<name>.jsonl --frames DIR` draws it into `DIR` without psx.

### The port protocol

JSON lines: psx writes one request a line to the port's stdin and reads
one reply a line from its stdout. stdout carries the protocol alone; logs
go to stderr (a stdout line that is not a JSON object is skipped as a
log).

```
-> {"hello":1}                                     <- {"kinds":["level"]}
-> {"launch":{"kind":"level","parameters":{...}}}  <- {"ok":true} | {"error":"..."}
-> {"tick":{"buttons":["CROSS","UP"],"analog":false,"sticks":[128,128,128,128]}}
                                                   <- {}
-> {"observe":{}}                                  <- {"values":{"croc_x":1234,"croc_gait":"walking"}}
-> {"screen":{"path":"/tmp/f/12.rgba"}}            <- {"width":320,"height":240} | {"none":true}
```

- `hello` gives the protocol version (1) and is answered with the kinds the
  port launches.
- `launch` starts a fresh game: nothing of the last one is kept.
- A tick's `buttons` are the recordings' names (any case); `sticks` are
  lx, ly, rx, ry, raw bytes.
- Values are in the recordings' encoding: raw integers (a fixed-point
  quantity's underlying integer, a float's bit pattern), as decimal
  strings outside ±2^53; bools; an enum's variant by name. psx checks each
  against its glossary type. A quantity left out is one the port has none
  of; a name the glossary lacks is a failure.
- `screen` writes `width * height` RGBA pixels, rows top to bottom, to the
  path.
- Any request may be answered `{"error":"..."}` (it could not be done) or
  `{"panic":"..."}` (the port panicked: the recording fails at that tick,
  and the next launch starts afresh). A port that exits or takes over two
  minutes on a reply is stopped and started again for the next recording.

`psx port check` writes each feature's scenario verdict.
**A feature passes when:**

- its entry names at least one recording;
- each of their scenarios runs (its launcher is registered);
- each recording on disk runs with no quantity out of tolerance;
- each scenario ran at least one captured recording.

## 6. Stages

The stages are port-design.md's (section 5). Scenarios decide two of them:

- **Recorded** needs the feature's work recordings ready: its entry names
  at least one; each of their scenarios' files check, with a captured
  recording, and passed `psx scenario check` as its files stand; and each
  recording's coverage counts against the inventory. `port next` gives the
  Record item until they do.
- **Matching** needs every work recording to pass the last `port check`
  at HEAD. Before that, `port next` gives Build while the port has no
  launcher for a scenario kind or does not observe a compared quantity.

A recording may take a branch whose effect does not reach a compared
quantity before it ends, so a port that took the other way could still
pass: compare more quantities, record longer routes, and hold routes out.

## 7. Not yet

- **The port's runner ignores the schedule.** It runs one tick per line and
  does not tell the port how many fields each tick took. A game whose logic
  scales by the fields elapsed needs that, and the pilot decides how.
- **Screen items and sound events** are later channels (port-design.md,
  section 4).

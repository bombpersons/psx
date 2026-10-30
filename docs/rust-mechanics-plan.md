# Plan: mechanics and bindings as Rust

Status: proposed, for the person to review. Nothing here is built yet.

The mechanic formats (`docs/mechanics.md`) describe a step in TOML: typed
fields in `mechanic.toml`, and where the PlayStation keeps them in
`binding.toml`. The Croc pilot keeps finding data the binding's vocabulary
cannot reach. List and record params were added for collision geometry, and
the next cases are already in view:

- references by index (a placed piece names its model by number);
- parallel arrays (the model numbers are a second array beside the pieces);
- computed indices (a grid cell, a hash, a scaled index);
- linked lists (Croc's object list, walked by next pointers);
- lists ended by a marker rather than a count;
- records whose size or shape depends on a tag;
- fields valid only when a flag is set;
- begin filters that depend on memory (Croc's object is at a different
  address on each level);
- state that is an array (collision's floor cache, the script's variables).

Each could be a small addition to the TOML, but together they make a
programming language. This plan uses Rust instead: the mechanic is a short,
plain Rust program, and the binding is a Rust program against a small
capture library. psx keeps what only it can do: run the game, watch every
access, account for what was read, and check the results.

## 1. What stays and what changes

| | Today | After |
|---|---|---|
| The mechanic's fields and types | `mechanic.toml` | Rust structs in the mechanic's crate |
| Its header (id, version, rate, exactness) | `mechanic.toml` | constants and the crate's doc comment |
| Its behaviour | the port's code | the mechanic's `step` function, the reference the port is translated from |
| Interfaces | `interfaces/<id>.toml` | traits in a crate per interface |
| Where the PlayStation keeps things | `binding.toml` | a Rust binding crate using `psx_capture` |
| Traces | JSON lines, captured from a state and input | unchanged, plus a fingerprint of the mechanic's types; captured from scenario recordings only |
| Checking the port | step replay through an adapter, and scenarios | scenarios only, with mechanic coverage linked to them |
| Glossary quantities | `glossary.toml`, `[quantities]` | a Rust struct in the `glossary` crate |
| Where the PlayStation keeps them, and the tick | `glossary.toml`, `[bindings]` and `tick` | the glossary's Rust binding using `psx_capture` |
| The port's quantity getters | registered by name | one function filling a generated `Observed` struct |
| Scenario files and recordings | TOML and JSON lines | unchanged |
| `NOTES.md` | quirks and uncovered branches | unchanged |
| Capture, the read audit, coverage, external changes | psx | psx, unchanged in what they report |

The traces stay the specification. The Rust mechanic is the reference
implementation that passes them; anyone porting to another language
translates it and replays the same traces.

## 2. The layers

```
interfaces/world.floor-slope/      trait, plain Rust, no dependencies
        ↑
mechanics/croc.turn/               the mechanic: plain, portable Rust
   ↑                  ↑
mechanics/croc.turn/binding/       the port (Bevy), or a translation
(PlayStation only; uses psx_capture)  into any other language
```

The dependencies are the rule. A mechanic depends only on interfaces and
other mechanics in the workspace, so it cannot see addresses, registers or
layouts. Only the binding knows the PlayStation. That keeps the mechanic
in game terms by construction rather than by review.

Workspace layout:

```
interfaces/world.floor-slope/
  Cargo.toml
  src/lib.rs
glossary/
  Cargo.toml          # package glossary; no dependencies
  src/lib.rs          # Quantities
  binding/            # package glossary-binding; the tick and where each quantity is
scenarios/<id>.toml   # unchanged
mechanics/croc.turn/
  Cargo.toml          # package croc-turn; depends on interfaces and mechanics only
  src/lib.rs          # header, types, step
  binding/
    Cargo.toml        # package croc-turn-binding, a cdylib; depends on croc-turn and psx-capture
    src/lib.rs
  traces/*.trace.jsonl
  NOTES.md
```

## 3. The mechanic

```rust
//! Croc's turning.
//!
//! LEFT and RIGHT ease Croc's turn speed toward a target that depends on
//! whether he is on the ground and moving, and it turns his heading; with
//! the analog stick he turns toward the stick's direction seen from the
//! camera.

pub const ID: &str = "croc.turn";
pub const VERSION: u32 = 1;
pub const RATE: &str = "tick";
pub const RATE_HZ: u32 = 25;
pub const EXACTNESS: &str = "exact";

/// Fixed point: 16 integer bits, 16 fraction bits.
pub type Fx16_16 = i32;

pub struct State {
    /// 1/4096 turn
    pub heading: u16,
    /// 1/4096 turn per tick
    pub turn_speed: Fx16_16,
}

pub struct Inputs {
    /// Bit 1: on the ground
    pub contact: u32,
    // …
}

pub fn step(state: &mut State, inputs: &Inputs) {
    // …
}
```

- **The header** is constants with plain values (strings and numbers), so
  the crate needs no psx types. The first doc line is the title and the
  rest the summary. A faithful mechanic adds `TOLERANCE`, a list of field
  names and raw differences.
- **`State`, `Inputs` and `Params`** are structs, each optional. Field doc
  comments carry units and descriptions.
- **Types.** `bool`; `u8` to `u64` and `i8` to `i64`; fixed point as an
  alias named `Fx<int>_<frac>` (signed) or `UFx<int>_<frac>` (unsigned)
  whose widths add up to the aliased integer's; `f32` and `f64` only where
  the original computes in floating point; enums with explicit
  discriminants; arrays `[T; N]` for tables; `Vec<T>` for lists; plain
  structs for records. Arrays and structs are allowed in state as well as
  params, which covers state that is a table.
- **`step`** takes `&mut State`, then `&Inputs`, `&Params` and
  `&mut impl Calls` as needed. `Calls` is a trait the crate declares,
  listing its interfaces' methods and one method per used mechanic's step.
  Replay answers them from the trace; the port implements them over its
  world and by calling the used mechanic's own `step`. Going through
  `Calls` keeps each mechanic's replay apart from the mechanics it uses,
  as recorded calls do today.
- **Helpers and constants.** The crate may define its own functions, types
  and `const` tables (a sine table belongs here, not in a param, once it is
  known to be constant). Integer behaviour is written out: `wrapping_add`
  where the original wraps, `>>` where it shifts, a widening multiply
  where it does one. A small named helper is used for an operation the game
  repeats, defined in the mechanic so it can be read with the rest.

**The portable subset.** `psx mechanic check` parses the crate with `syn`,
without building it, and refuses:

- dependencies other than workspace interfaces and mechanics;
- macros other than the standard library's basic ones (`assert!`,
  `debug_assert!`, `unreachable!`, `vec!`), and procedural macros;
- generics, lifetimes in field types, trait objects, and references in
  fields;
- collections other than `Vec`; `unsafe`; floats in an exact mechanic
  where the field's type is integer;
- field types it cannot record (anything outside the types above).

What a tool cannot check, the guide asks for: game-term names, a comment
on each wrap, shift or truncation the result depends on, and no
transliterated assembly (register-named temporaries, flow that mirrors the
original's branches rather than the rule).

**The fingerprint.** psx hashes the parsed types and header. A trace
records the fingerprint it was captured with, and check refuses a changed
fingerprint under the same version, so a type change forces a version bump.

## 4. Interfaces

```rust
//! world.floor-slope: the slope of the floor under a point.

pub const ID: &str = "world.floor-slope";
pub const VERSION: u32 = 1;

pub trait FloorSlope {
    /// The floor's slope at (x, z), in 1/4096 per unit along x and z.
    fn floor_slope(&mut self, x: i32, z: i32) -> Slope;
}

pub struct Slope { pub x: i16, pub z: i16 }
```

A method that returns a value is a query; one that returns `()` is a
command. The doc comments are the contract a reimplementer's world must
honour; the traces show example answers.

## 5. The binding

The binding is a small program built against `psx_capture`, a library psx
provides. It only gives access to the machine; everything else is the
binding's own Rust.

```rust
use psx_capture::prelude::*;
use croc_turn::{Inputs, State};

struct Binding;

impl capture::Binding for Binding {
    type Mechanic = croc_turn::Mechanic;   // generated glue, section 6

    fn bracket(&self, b: &mut Bracket) {
        b.begin(0x8002_C1F0).end(0x8002_C3A4);
        b.query::<FloorSlope>(Site::entry(0x8003_06B0), request, response);
    }

    fn is_step(&self, m: &Machine) -> bool {
        m.reg(A0) == m.read_u32(0x8007_4B90)     // the player object, on any level
    }

    fn state(&self, m: &Machine) -> State {
        let croc = m.latched(A0);
        State { heading: m.read_u16(croc + 0x3E), turn_speed: m.read_i32(croc + 0x60) }
    }

    fn inputs(&self, m: &Machine) -> Inputs { /* … */ }
    fn params(&self, m: &Machine) -> Params { /* readers: pointers, counts, references */ }
    fn explained(&self, e: &mut Explained) { /* reads that are not behaviour, and why */ }
}

psx_capture::export!(Binding);
```

(Addresses are placeholders.)

- **`Machine`** reads RAM, the scratchpad, CPU and GTE registers, and the
  registers latched at begin. It records every byte the binding reads, so
  the read audit accounts for them exactly as it accounts for TOML
  locations today. Reads outside main RAM and the scratchpad are errors.
- **The same function reads state before and after a step.** The compiler
  checks that every field is bound.
- **Hooks are declared up front** in `bracket`, so psx knows which PCs to
  stop at: begin and end, overlays, call sites by entry PC or inline load.
- **`is_step`** replaces register filters, so a filter may read memory.
- **Params** are read once, at the first step, by ordinary code: follow a
  pointer, read a count, look up a model by the index in a parallel array.
  Every case in the list at the top is a few lines here.
- **What psx still checks:** that state read at a step's end matches what
  the next step reads at its begin (external changes), the read audit,
  branch coverage, and that each value fits the mechanic's type.

**How a binding runs.** psx is used as a release binary, without its
source, so the emulator and the capture engine stay inside psx and the
binding is loaded into it:

- `psx-capture` is a small crate the template vendors, as it vendors
  `psx-mechanics` today. It holds the API (`Machine`, `Bracket`, `Site`,
  the `Binding` and `Glossary` traits) and `export!`, which wraps the
  binding in a few C-ABI functions. It has no emulator and builds in
  seconds.
- A binding crate is a `cdylib`. `psx mechanic capture` builds it with
  cargo and loads the library. The workspace needs only the Rust toolchain
  the port already needs.
- psx calls the binding at its hooks. `Machine`'s reads call back into psx,
  which records them for the read audit. Values cross the boundary in the
  trace's own encoding, so the C ABI stays a handful of functions and does
  not depend on the Rust compiler's version.
- `export!` records the crate's ABI version, and psx refuses a binding
  built against another, naming `psx template update` as the fix.
- A panic in the binding is caught at the boundary and reported as the
  capture's error.
- The emulator, the hooks, the read audit, coverage, external changes and
  the trace writer are psx's alone, as verdicts are: nothing the workspace
  builds can change how a capture is judged.

The trace format is unchanged.

## 6. Replay of the reference

Replay turns trace values into the mechanic's structs and back, and answers
`Calls` from the trace. The mechanic has no derives, so the glue is
generated: a `build.rs` in the workspace's replay crate parses each
mechanic with psx's `syn` front end and writes the conversions and a
`Calls` implementation that answers from the trace and checks calls in
order. Nothing generated is committed.

- `cargo test -p replay` replays every trace of every mechanic against the
  mechanic's own `step`, with no emulator. This proves the reference: that
  the rules as written reproduce the original.
- The template's `replay.rs` moves into this crate.
- The port is not replayed step by step (section 7).

## 7. Checking the port

The mechanics prove that the original is understood: followed properly,
they give a game that behaves as the PlayStation's does. How the port
merges or splits them is the porter's choice. An idiomatic Bevy port may
split Croc's tick into several systems over components, use events where
the original sets flags, and keep heading in a component of its own. So
the port neither links the mechanic crates nor adapts to them step by
step: it is a translation, like one into any other language. (Linking a
mechanic is still allowed, for a quick prototype; the check is the same.)

**The port is checked from outside, by scenarios** (`docs/scenarios.md`).
This is how the Tomb Raider II port checks itself: a recorded route of the
original, the port started where the original was, the same input each
tick, and the observable quantities compared every tick, with routes
recorded after the port was written held out to catch fitting. psx already
has this: scenario files, recordings of glossary quantities, the port's
launchers and getters, and `port check`'s scenario verdict. The port owes
only:

- a launcher for each scenario kind, building the scenario's start;
- a getter for each compared quantity, in the original's raw units;
- input from the recording, and interfaces such as `rng` answered from it.

**Every recording is a scenario recording.** Mechanic capture runs over a
scenario's recording, not over an arbitrary state and input:

- `psx mechanic capture ID --scenario SCENARIO --recording NAME` replays
  the recording's input from the scenario's state, and the trace's header
  names both. One run can capture every mechanic bound in the workspace.
- A scenario starts where the port's launcher can build it: a level start,
  just after loading, when the game's hidden state is at its defaults, or
  a point the launcher sets up from glossary quantities alone (where Croc
  first stands still). A savestate in the middle of a jump does not work,
  because the port would have to rebuild hidden state, timers or the floor
  cache, that it may not model the same way.

**Coverage links the two layers.** Each trace records which way every
branch went at each step. For each mechanic:

- the reference's coverage is the union of branch outcomes over its
  traces;
- the port's coverage is that union over the traces whose scenario
  recordings the port passes.

Because every trace comes from a scenario recording, a port that passes
every recording covers every outcome the reference does. One that fails a
recording leaves exactly that recording's outcomes unconfirmed. `port
status` reports it per mechanic: "croc.collide: 41 of 44 branch outcomes
confirmed in the port; the other 3 appear only in `slope_jump`, which
fails at tick 812".

**Diagnosis.** When a recording fails at a tick, the traces captured from
the same recording show which mechanics stepped in that tick, their inputs
and state, and what the reference's `step` gives. That points at the rule
the port got wrong without the port having a matching seam.

**What is left unchecked.** A branch outcome exercised in a recording may
not reach a compared quantity before the recording ends, so a port that
took the other side could still pass. The remedy for now is the one TR2
uses: compare more quantities (a mechanic's state fields that are glossary
quantities show a difference at once), record longer routes, and hold
routes out. A stronger check is possible later if that proves not to be
enough: psx re-runs a recording on the native core with one branch forced
the other way at one step, and reports outcomes whose forcing changes no
compared quantity, meaning that recording does not really test them. It
is not part of this plan.

## 8. The glossary and scenario files

`glossary.toml` (`docs/scenarios.md`) is two files in one, the same two
layers as a mechanic:

- `[quantities.*]`: names, types, units and descriptions in game terms;
- `[bindings.*]` and `tick`: PlayStation addresses, `{ pointer, offset }`
  chains, and the store that ends a logic step.

Its bindings already need vocabulary of their own (one pointer level), and
the next quantities will want more: an enemy found through the object
list, a value that means something only while a flag is set. So it splits
the same way:

- **The quantities** are a plain Rust struct in a `glossary` crate, under
  the mechanic's rules: no dependencies, units and descriptions in doc
  comments, read by psx with `syn`.

  ```rust
  //! Croc (Europe) SLES-00593: the quantities scenarios compare.

  pub struct Quantities {
      /// Croc's position across the level, in world units.
      pub croc_x: i32,
      // …
  }
  ```

- **Their binding** is a Rust program against `psx_capture`, beside the
  mechanics' bindings: the tick hook, and one function that reads every
  quantity from a `Machine`. Pointer chains and list walks are ordinary
  code there.

  ```rust
  impl capture::Glossary for Glossary {
      fn tick(&self, b: &mut Bracket) { b.store(0x8007_5240, 4).at_pc(0x8001_3CA8); }
      fn quantities(&self, m: &Machine) -> Quantities {
          let croc = m.read_u32(0x8007_528C);
          Quantities { croc_x: m.read_i32(croc + 0x370), /* … */ }
      }
  }
  ```

  The release's identity (`source`) moves there too.

`psx scenario record` builds the glossary's binding the way `mechanic
capture` builds a mechanic's. Once capture runs over scenario recordings
(section 7), one run records the quantities and every mechanic's traces
in the same run.

**The port fills the quantities by type, not by name.** Today a getter is
registered by a string (`register_quantity("croc_x", …)`), so a misspelt
name fails only when a scenario runs. Instead, the generated glue gives the
port an `Observed` struct with each quantity as an `Option`, and the port
registers one function that fills it:

```rust
app.observe(|w: &World| Observed {
    croc_x: w.query::<&Croc>().iter(w).next().map(|c| c.x),
    ..Observed::none()
});
```

A misspelt or removed quantity is a compile error. A quantity left out is
`None`, which keeps today's meaning: the port has no such thing yet. A
scenario that compares it fails and says so, and stage 3's check (a getter
for each compared quantity) reads the same answer from the function's
result.

**Scenario files stay TOML.** `scenarios/<id>.toml` holds a feature id, a
moment in prose, a states chain, the port's launcher name, idle fields,
the quantities compared with their tolerances, and the launcher's
parameters. Nothing in it is a location, a layout or a rule; it is
configuration a person writes and edits by hand. psx checks it against the
Rust glossary: each compared name must be a quantity there. Recordings
stay JSON lines.

## 9. What it means for the port design

`docs/port-design.md` changes in five places, and `docs/scenarios.md` in
its glossary section (section 8):

- **Section 5.4.** "The step is the port's own code" stays true, but the
  port no longer has a trace adapter per mechanic. The mechanic's `step`
  is the reference the port is translated from.
- **Section 5.5 and section 6, numbers.** The template's `Scalar` trait,
  generic over `Fixed` and `f32`, was never built (B3). Mechanics written
  generically would hide the original's arithmetic behind a library and
  need a dependency, against both rules above. This plan drops it: exact
  mechanics are plain integers, and the port chooses its own types, held
  to the scenarios' tolerances. The `f32` drift report goes with it.
  Settled (section 12).
- **Section 6, tests.** Layer 1 (step replay) proves the reference, not
  the port. Layer 2 (scenarios) is the port's test, with the coverage
  link above.
- **Section 4, Mechanics, and section 10.** The formats and the capture
  items are described in Rust terms; capture runs over scenario
  recordings; the read audit, external changes, coverage and refusals are
  unchanged.
- **Section 4, Glossary.** Quantities are a Rust struct and their binding
  a Rust program (section 8).

## 10. Level data

Data params are read from RAM by the binding and written into the trace,
so a trace replays on its own: checking a mechanic never needs the disc.
A playable port needs every level's data, which is extraction's job. What
capture records is the game's in-memory form, after its loader; for Croc,
the map loader shifts positions and scales heights, and the track loader
frees models the level does not use. An extractor reading the disc must
reproduce that, and each captured level is its test: extract level 1-1 and
compare with the params in its traces.

## 11. The order of work

Each step ends with something that runs, and is committed.

| Step | Size | What |
|---|---|---|
| R1 | S | This plan, and the person's decisions on it. |
| R2 | M | The `syn` front end in `psx-mechanics`: header, types, the portable subset, the fingerprint, into the model capture uses today. `psx mechanic check` reads Rust mechanics. `croc.turn` converted, types only. |
| R3 | L | `psx_capture`: `Machine` with tracked reads, `Bracket` and hooks, `is_step`, params, explained reads; the vendored crate's C-ABI boundary and ABI version; `psx mechanic capture` builds the binding as a `cdylib` and loads it. `croc.turn` bound in Rust. **Its traces must equal the TOML binding's for the same recordings**, which checks the new capture against the old. |
| R4 | M | The replay crate and its generated glue; `croc.turn`'s `step` written and replaying exactly. |
| R5 | L | `croc.ground-move` and `croc.tick` converted the same way, traces compared with the old ones. The glossary converted: `Quantities` and its Rust binding, `scenario record` building it, recordings compared with the old ones; the port's `Observed` and `app.observe` replace getters registered by name. The TOML mechanic, interface, binding and glossary formats, their schemas and the TOML binding vocabulary removed; `docs/mechanics.md`, `docs/scenarios.md`, the guide and the template rewritten. |
| R6 | M | Capture over scenario recordings: `--scenario` and `--recording`, the glossary and every bound mechanic in one run, the trace header naming both. Croc's scenarios start at level starts; his traces are captured again from their recordings. `port status` reports each mechanic's branch outcomes confirmed in the port, and a failing recording's tick points at the traces of that tick. |
| R7 | L | Collision: the map reader (pieces with their models), the floor cache as array state, the objects interface, the probe points. Captured from scenario recordings with a clear audit and replayed. |

Then the pilot continues where it stopped: `croc.tick`'s sound and rng
interfaces, Croc's fields and script variables, and the port.

## 12. Decisions for the person

1. **How the binding runs.** Recommended: a `cdylib` against a small
   vendored `psx-capture` crate, built by psx with cargo and loaded into
   psx, which keeps the emulator and the capture engine (section 5). It
   works with a release binary of psx and no psx source. The alternatives:
   - the binding links psx's emulator and capture engine as a vendored
     crate and runs as its own program: no FFI, but the workspace compiles
     the emulator, and the engine that judges captures becomes code the
     workspace can edit;
   - bindings compiled to WebAssembly and run inside psx: sandboxed and
     portable, but psx gains a WebAssembly runtime and the workspace a
     target to install;
   - a binding in its own process asking psx for reads: decoupled, but
     slow for bindings that read thousands of bytes per step.
2. ~~**Numbers.**~~ Settled: `Scalar`, `Fixed`, the `f32` instantiation
   and the drift report are dropped (section 9). Mechanics are plain
   integers; the port chooses its own types, held to the scenarios'
   tolerances.
3. ~~**TOML alongside Rust.**~~ Settled: one format. The TOML formats are
   removed once Croc's mechanics and glossary are converted (R5).
4. ~~**How the port is checked.**~~ Settled: by scenarios only, with every
   recording a scenario recording and mechanic coverage linked to them
   (section 7). The port does not link or adapt to the mechanics; how it
   merges or splits them is the porter's choice.

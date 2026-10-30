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
| Its behaviour | the port's code | the mechanic's `step` function |
| Interfaces | `interfaces/<id>.toml` | traits in a crate per interface |
| Where the PlayStation keeps things | `binding.toml` | a Rust binding crate using `psx_capture` |
| Traces | JSON lines | unchanged, plus a fingerprint of the mechanic's types |
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
mechanics/croc.turn/
  Cargo.toml          # package croc-turn; depends on interfaces and mechanics only
  src/lib.rs          # header, types, step
  binding/
    Cargo.toml        # package croc-turn-binding; depends on croc-turn and psx-capture
    src/main.rs
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

fn main() { psx_capture::run(Binding) }
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

`psx mechanic capture` builds the binding and runs it with the recording's
arguments; the binding links psx's native core through `psx_capture`, and
the capture runs in that process. The trace format is unchanged.

## 6. Replay and the glue

Replay needs to turn trace values into the mechanic's structs and back,
and to answer `Calls` from the trace. The mechanic has no derives, so the
glue is generated: a `build.rs` in the workspace's replay crate parses each
mechanic with psx's `syn` front end and writes the conversions and a
`Calls` implementation that answers from the trace and checks calls in
order. Nothing generated is committed.

- `cargo test -p replay` replays every trace of every mechanic with no
  emulator, as the template's harness does today.
- `psx port check` runs it and records the verdict.
- The port depends on the mechanic crates and calls their `step` from its
  systems; it does not reimplement them. Its own tests cover the wiring
  (`port check`'s scenarios).
- The template's `replay.rs` moves into this shared runtime.

## 7. What it means for the port design

`docs/port-design.md` changes in three places:

- **Section 5.4.** "The step is the port's own code" becomes: the step is
  the mechanic's code, and the port calls it.
- **Section 5.5 and section 6, numbers.** The template's `Scalar` trait,
  generic over `Fixed` and `f32`, was never built (B3). Mechanics written
  generically would hide the original's arithmetic behind a library and
  need a dependency, against both rules above. This plan drops it: exact
  mechanics are plain integers, and a new game that wants `f32` translates
  the mechanic as it would into any language, then replays the traces as
  a faithful mechanic. The `f32` drift report goes with it. **This is a
  decision for the person** (section 10).
- **Section 4, Mechanics, and section 10.** The formats and the capture
  items are described in Rust terms; the read audit, external changes,
  coverage and refusals are unchanged.

## 8. Level data

Data params are read from RAM by the binding and written into the trace,
so a trace replays on its own: checking a mechanic never needs the disc.
A playable port needs every level's data, which is extraction's job. What
capture records is the game's in-memory form, after its loader; for Croc,
the map loader shifts positions and scales heights, and the track loader
frees models the level does not use. An extractor reading the disc must
reproduce that, and each captured level is its test: extract level 1-1 and
compare with the params in its traces.

## 9. The order of work

Each step ends with something that runs, and is committed.

| Step | Size | What |
|---|---|---|
| R1 | S | This plan, and the person's decisions on it. |
| R2 | M | The `syn` front end in `psx-mechanics`: header, types, the portable subset, the fingerprint, into the model capture uses today. `psx mechanic check` reads Rust mechanics. `croc.turn` converted, types only. |
| R3 | L | `psx_capture`: `Machine` with tracked reads, `Bracket` and hooks, `is_step`, params, explained reads; `psx mechanic capture` builds and runs a binding. `croc.turn` bound in Rust. **Its traces must equal the TOML binding's for the same recordings**, which checks the new capture against the old. |
| R4 | M | The replay crate and its generated glue; `croc.turn`'s `step` written and replaying exactly. |
| R5 | M | `croc.ground-move` and `croc.tick` converted the same way, traces compared with the old ones. The TOML formats, their schemas and the TOML binding vocabulary removed; `docs/mechanics.md`, the guide and the template rewritten. |
| R6 | L | Collision: the map reader (pieces with their models), the floor cache as array state, the objects interface, the probe points. Captured with a clear audit and replayed. |

Then the pilot continues where it stopped: `croc.tick`'s sound and rng
interfaces, Croc's fields and script variables, and the port.

## 10. Decisions for the person

1. **How the binding runs.** Recommended: it links `psx_capture`, and
   with it psx's native core, as a normal Rust dependency pinned by `port
   init`; psx builds and runs it. Fast and simple, but the workspace
   compiles the emulator once, and a workspace must be updated when psx's
   capture library changes. The alternatives are compiling bindings to
   WebAssembly and running them inside psx (decoupled and sandboxed, but
   adds a WebAssembly runtime to psx and a target to install), or a binding
   in its own process asking psx for reads (decoupled, but slow for
   bindings that read thousands of bytes per step).
2. **Numbers.** Drop `Scalar`, `Fixed`, the `f32` instantiation and the
   drift report (section 7). Recommended.
3. **TOML alongside Rust.** Recommended: no. One format; the TOML formats
   are removed once Croc's mechanics are converted (R5).

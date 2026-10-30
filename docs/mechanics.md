# Mechanic formats

A **mechanic** is one step the original runs, captured from the game step by
step so the port's version can be replayed against it without an emulator
(`docs/port-design.md`, section 4). This document defines its files. They are
psx's own, adapted from the mechanics knowledge base's draft (version 0).

The formats are platform-free except for the binding's locations, call sites
and capture bracket, which are the producer's (psx's R3000 vocabulary is in
section 5). The code that reads and checks them is in two places:

- `crates/psx-mechanics`: the mechanic, interface and trace formats, the
  binding's shape, and their checks. Its JSON Schemas are in
  `crates/psx-mechanics/schemas/`.
- `crates/psx/src/mechanic.rs`: the PlayStation binding vocabulary. Its
  schema is `crates/psx/schemas/binding.schema.json`.

The schemas describe the TOML files after conversion to JSON, and the trace's
lines as written. Tests regenerate the schemas from the types and fail when a
committed one is stale; `PSX_UPDATE_SCHEMAS=1 cargo test` rewrites them. The
checks catch more than the schemas can, such as a binding that misses a field
or a value too wide for its type.

## 1. Files

```
interfaces/<interface-id>.toml       the calls mechanics make to the world
mechanics/<mechanic-id>/
  mechanic.toml                      typed state, inputs, params; rate; interfaces it requires, mechanics it uses
  binding.toml                       where this release keeps them, and the capture bracket
  traces/<name>.trace.jsonl          captured steps
  NOTES.md                           a few lines: what it does, quirks, what traces miss
```

- **Ids.** A mechanic id is `<game>.<name>` (`croc.walk`, `gt2.drivetrain`), and
  is also the directory's name. An interface id runs broad to narrow
  (`world.floor-height`, `rng`) and is also the file's name.
- **Segments** are lowercase letters and digits joined by single hyphens,
  separated by dots.
- **Names** of fields, calls and enum variants are `[a-z][a-z0-9_]*`.
- **References.** A mechanic requires an interface, or uses another
  mechanic, as `<id>@<version>`; the workspace holds one version of each.
- **A mechanic written as Rust.** In place of `mechanic.toml`, the directory
  may hold a crate (section 3a). Mechanics are moving to that form
  (`docs/rust-mechanics-plan.md`); a directory holds one or the other.
- **A binding written as Rust.** In place of `binding.toml`, the directory
  may hold a binding crate in `binding/` (section 5a); again one or the
  other.

## 2. Value types

Every state field, input, param and interface field declares a type:

| `type` | Keys | Raw value |
|---|---|---|
| `bool` | | `true` / `false` |
| `int` | `bits` (1 to 64), `signed` | the integer |
| `fixed` | `signed`, `int_bits` (sign included), `frac_bits` | the underlying integer: the real value × 2^`frac_bits` |
| `enum` | `variants` (names) | the variant's name |
| `float` | `bits` (32, 64 or 80) | its bit pattern, as an unsigned integer |
| `table` | `of` (an element type, not a table), `len` | a list of `len` raw values; params only |
| `list` | `of` (an element type) | a list of raw values, of any length; params only |
| `record` | `fields` (names to types) | an object of raw values by name; params only |

Any type may add `unit` (`"units/tick"`, `"angle/4096"`) and `description`;
both are documentation only.

Lists and records hold data the step reads but never changes, such as a
level's collision polygons. They describe that data in the game's terms
(pieces, polygons, edges), not its memory layout. Pointers, counts that
only say how long a list is, strides and padding belong to the binding.
Declare only the fields the step reads.

**Raw values** are always exact. In JSON, an integer outside ±2^53 is written
as a decimal string (`"1152921504606846976"`). A JSON string is read as a
variant name when it is a name, and as an integer otherwise. Fractional
numbers are refused. Arithmetic wraps at the declared width unless the
mechanic's notes say otherwise.

## 3. `mechanic.toml`

```toml
id = "croc.walk"
version = 1                  # bump on any change to fields, requires or uses
title = "Walking"
summary = "Speed closes a quarter of the gap to four times the target each tick."
exactness = "exact"          # the default; or "faithful"
rate = { per = "tick", hz = 25 }
requires = ["world.floor-height@1"]

[state.speed]
type = "fixed"
signed = true
int_bits = 20
frac_bits = 12
unit = "units/tick"

[state.anim_timer]
type = "int"
bits = 8
signed = false

[inputs.forward]
type = "bool"
description = "Forward is held, as the step sees it."

[params.target]
type = "int"
bits = 16
signed = false

[params.curve]
type = "table"
len = 8
of = { type = "fixed", signed = false, int_bits = 4, frac_bits = 12 }

[params.pieces]              # the level's placed pieces, in the game's order
type = "list"
of = { type = "record", fields = { heading = { type = "int", bits = 16, signed = false }, x = { type = "int", bits = 32, signed = true }, z = { type = "int", bits = 32, signed = true } } }
```

- **The three kinds of field.**
  - `state` is what the step owns and carries from one step to the next.
  - `inputs` are what it reads each step but does not own, as the step sees
    them: after the game's own input handling, or a frame counter's parity.
  - `params` are constant for the length of a trace; the binding gives their
    values.
- **`rate`** is how often the original runs the step, so another game can run
  it at its native rate:
  - `per = "tick"`: once per game tick;
  - `per = "vblank"`: once per display refresh;
  - `per = "event"`: when something happens.

  `hz` is the nominal number of steps per second. `tick` and `vblank` need it;
  `event` has none.
- **`exactness`.**
  - `exact`: the port's step reproduces every field of every step, with
    tolerance 0.
  - `faithful`: each step starts from the trace's recorded state and is
    compared within `[tolerance]`. That table gives the largest difference
    allowed per int or fixed state field, in raw units:
    `tolerance = { speed = 16 }`.
- **`requires`** lists the interfaces the step calls.
- **`uses`** lists the mechanics whose steps run inside this one's, such as
  Croc's ground movement inside the tick of his script:
  `uses = ["croc.ground-move@1"]`.
  - A used mechanic's step is recorded as a call: `<id>@<version>`, call
    `step`, a query. Its request is the used mechanic's inputs and state as
    its step begins, and its response is its state as it ends. Its own
    traces pin down what happens in between.
  - A name is a state field or an input, not both, so a step's request has
    one value per name.

## 3a. A mechanic written as Rust

```
mechanics/croc.turn/
  Cargo.toml        # package croc-turn: the id with dots as hyphens
  src/lib.rs        # header constants, documentation, State, Inputs, Params
```

The crate is plain Rust that any language can be translated from. psx reads
it from source, without building it, into the same mechanic `mechanic.toml`
gives:

```rust
//! Croc's turning
//!
//! LEFT and RIGHT ease Croc's turn speed toward a target that depends on
//! whether he is on the ground and moving, and it turns his heading.

pub const ID: &str = "croc.turn";
pub const VERSION: u32 = 1;
pub const RATE: &str = "tick";          // or "vblank", "event"
pub const RATE_HZ: u32 = 25;            // tick and vblank
pub const EXACTNESS: &str = "exact";    // the default; or "faithful"
// pub const TOLERANCE: &[(&str, u64)] = &[("speed", 16)];   faithful only
// pub const REQUIRES: &[&str] = &["world.floor-slope@1"];
// pub const USES: &[&str] = &["croc.ground-move@1"];

/// Fixed point: 16 integer bits, 16 fraction bits.
pub type Fx16_16 = i32;

pub struct State {
    /// Unit: 1/4096 turn
    pub heading: u16,
    /// Unit: 1/4096 turn per tick
    pub turn_speed: Fx16_16,
}

pub struct Inputs {
    /// Bit 1: on the ground
    pub contact: u32,
}
```

- **Title and summary** are the crate documentation's first two
  paragraphs (a trailing full stop is dropped from the title).
- **`State`, `Inputs` and `Params`** are each optional. A field's doc
  comment is its description, except a line `Unit: …`, which is its unit.
- **Types:** `bool`; `u8` to `u64` and `i8` to `i64`; `f32` and `f64`; an
  alias `Fx<int>_<frac>` (signed) or `UFx<int>_<frac>` (unsigned) for fixed
  point, whose widths add up to the integer it aliases; other aliases;
  enums of unit variants (`BackingUp` is the variant `backing_up`);
  structs with named fields (records); `[T; N]`, with `N` a literal or a
  `const` of the file (tables); `Vec<T>` (lists). Types are declared in
  `src/lib.rs`.
- **The portable subset.** psx refuses, in every file under `src/`:
  - dependencies other than the workspace's mechanics (`path = "../<id>"`)
    and interfaces (`path = "../../interfaces/<id>"`), a build script, and
    `use` of anything else;
  - `unsafe`, foreign functions, `static mut`;
  - macro definitions, and macros other than `assert!` and its kin,
    `matches!`, `panic!`, `unreachable!` and `vec!`;
  - derives other than the standard library's;
  - generic parameters (lifetimes on functions excepted; `impl Trait`
    arguments are fine), trait objects;
  - `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, `VecDeque`, `BinaryHeap`,
    `LinkedList`, `Rc`, `Arc`, `Cell`, `RefCell`, `Mutex`, `RwLock`;
  - floats in an exact mechanic whose fields are all integers.

## 4. Interfaces: `interfaces/<id>.toml`

```toml
id = "world.floor-height"
version = 1
title = "Height of the floor under a point"
description = "What the original's routine asks, as low-level as it really is."

[calls.height]
kind = "query"
request.x = { type = "int", bits = 32, signed = true }
request.z = { type = "int", bits = 32, signed = true }
response.y = { type = "int", bits = 32, signed = true }

[calls.splash]
kind = "command"
request.x = { type = "int", bits = 32, signed = true }
```

- **Two kinds of call.**
  - A `query` asks the world something and gets a response.
  - A `command` acts on the world and has no response: damage the object in
    this box, play the swipe sound.
- **Order.** Both kinds are recorded and replayed in order. The port's step
  takes the world as a trait with one method per call.
- **Random numbers** are a query, such as `rng@1` with a call `next`, so a
  replay gets the original's numbers and the port can use its own generator.
- **Field types.** Request and response fields use the value types, without
  `table`.

## 5. `binding.toml`

The binding says where one release of the game keeps the mechanic's fields,
and how its step is bracketed. Its shape is the same for every platform. Its
locations, call sites and `[capture]` are the producer's vocabulary; psx's are
below.

```toml
mechanic = "croc.walk"
mechanic_version = 1

[source]
platform = "psx"
title = "Croc (Europe) SLES-00593"
image_sha256 = "…"           # the media identity psx context reports

[state.speed]
parts = [{ at = { base = "a0", offset = 0x40 }, bytes = 4 }]

[state.anim_timer]
unobserved = true
reason = "kept in s3 across the step and never stored"

[inputs.forward]
parts = [{ at = { address = 0x800A5C10 }, bytes = 2 }]
mask = 0x1000

[params]
target = 35                  # a value

[params.curve]               # or a location, read once at the first step's start
parts = [{ at = { base = "a0", offset = 0x1C }, bytes = 2 }]
stride = 2

[params.pieces]              # or data: where it starts, and its layout from there
at = { address = 0x8007515C }
follow = [0]                 # the header the word there points to
[params.pieces.layout]
count = { parts = [{ at = { offset = 0x6C }, bytes = 2 }] }
follow = [0x54]              # the elements, through the header's word at +0x54
stride = 0x18
[params.pieces.layout.of.fields]
heading = { parts = [{ at = { offset = 2 }, bytes = 2 }] }
x = { parts = [{ at = { offset = 8 }, bytes = 4 }], stored_signed = true }
z = { parts = [{ at = { offset = 0x10 }, bytes = 4 }], stored_signed = true }

[params_provenance]
target = "immediate operand at 0x800437F8"
curve = "the object's record, from LEVEL.BIN"
pieces = "MAPS/MP010_00.MAP, as the loader places it"

[interfaces."world.floor-height@1".height]
on = { entry = { pc = 0x80051A00 } }
request.x = { parts = [{ at = { register = "a0" }, bytes = 4 }] }
request.z = { parts = [{ at = { register = "a1" }, bytes = 4 }] }
response.y = { parts = [{ at = { register = "v0" }, bytes = 4 }] }

[interfaces."world.floor-height@1".splash]
on = { entry = { pc = 0x80051B40 } }
request.x = { parts = [{ at = { register = "a0" }, bytes = 4 }] }

[capture]
begin = { pc = 0x80043234, a0 = 0x801F0000 }
end = "next_begin"
explained = [
  { at = { base = "a0", offset = 0x80 }, bytes = 0x20, why = "the animation, another mechanic" },
  { by = { pc = 0x80043A30 }, why = "finding the floor polygon: level data, wherever the level put it" },
]
```

(Addresses here are placeholders.)

The binding must cover the mechanic completely:

- every state field and input, and every call of every required interface,
  with each request and response field;
- every param, as a value or as a location.

A state field the producer cannot read is `{ unobserved = true, reason }`:
traces leave it out and replay does not compare it. Inputs are never
unobserved.

### Reading a field

Each field is read from its `parts`, as follows:

1. Read each part (`at`, `bytes`) in the part's byte order: `endian` (`"big"`
   or `"little"`), defaulting to the platform's. The PlayStation's is little.
2. Join the parts into one unsigned integer, the first part most
   significant (16 bytes at most).
3. Apply `mask`, then `shift` (a right shift).
4. With `stored_signed = true`, sign-extend from the read width: the bits
   left after the mask (up to its highest set bit) and the shift, or the
   parts' bits less the shift.
5. For a `fixed` field whose stored fraction differs from the mechanic's,
   `stored_frac_bits` gives the stored one. Capture refuses a rescale that
   would lose bits.
6. `map` turns the stored integer (a decimal key) into the value. An `enum`
   needs it; a `bool` may use it, and is otherwise true when nonzero.
7. A `float` is read whole, with no mask, shift or sign extension.

**A param** is a raw value, a read or data. Capture reads it once, at the
first step's start, and the trace's header holds the values either way. For a
table param, `parts` reads the first element, and the others follow every
`stride` bytes (by default, the size of the parts). Constants in the code go
in as values; values the game loads, such as a car's gear ratios, are read,
so every trace carries the values of the run it came from.

**Data** reads a list, a record, or a table of records, laid out in memory:

- `at` is where it starts: an address, or a base and offset. `follow`
  lists pointers to take first: at each offset in turn, the 4-byte address
  held there becomes the base.
- `layout` says where the value lies from that base:
  - a field: `parts` whose `at` is `{ offset }` from the base, read as
    above;
  - a record: `fields`, each laid out from the same base;
  - a table or list: `stride` and `of` (each element, from its own start).
    `follow` and `offset` lead from the base to the first element. A list
    reads its length from `count`, a field from the base; when it is 0,
    no pointer is followed.
- The read audit accounts for every byte data reads: the pointers, the
  counts and the declared fields. A field the step reads but the layout
  leaves out stays in the audit, so the mechanic declares it.
- Capture refuses a null pointer, or a count over 65536.

### PlayStation vocabulary

**Locations** (`at`):

| Form | Where |
|---|---|
| `{ address = 0x800A5C10 }` | main RAM in any segment, or the scratchpad (`0x1F800000` to `0x1F8003FF`); in `explained`, any address |
| `{ base = "a0", offset = 0x40 }` | relative to a register's value, latched when the step begins, or for an entry call's request and response when the call is entered |
| `{ register = "v0" }` | a CPU register: at begin for inputs and params, at a call's entry for a request, at its return for a response |
| `{ gte = 32 }` | a GTE register: data 0 to 31 as `mfc2` numbers them, control 32 to 63 (`cfc2` n is 32 + n) |
| `{ load = "address" }`, `{ load = "value" }` | an inline read's request (the address loaded from) and response (the value loaded) |
| `{ pointer = 0x800A1234, offset = 0x34 }` | glossary quantities only ([scenarios.md](scenarios.md)): the address held at `pointer`, plus `offset` |

- **Register names** are the conventional ones: `zero at v0 v1 a0`–`a3`
  `t0`–`t9` `s0`–`s7` `k0 k1 gp sp fp ra`, plus `hi` and `lo`.
- **State is always in memory.** Registers are clobbered between steps, so
  state bound to a register is refused; such state is unobserved.

**PCs** are `{ pc, overlay }`.
- `pc` is a word-aligned address in main RAM.
- `overlay` names the overlay the PC is in, exactly as `psx inventory` names
  it, since the same address runs different code as overlays change.
- Leave `overlay` out for resident code.

**Call sites** (`on`):
- `{ entry = PC }` is a routine entered at that PC. The request is read at
  entry, and the response when it returns to its caller.
- `{ read = PC }` is a load at that PC that asks the world inline, such as a
  table lookup.

**`[capture]`**:
- **`begin`**: `{ pc, overlay }` plus register filters (`a0 = 0x801F0000`). A
  step begins when the PC runs with every filter matching. A filter picks the
  player's object when others run the same routine.
- **`end`**:
  - a PC: the step ends when it runs;
  - `"next_begin"`: it ends at the next begin;
  - `"frame"`: it ends at the next vblank.
- **`explained`**: reads the step makes that the binding deliberately leaves
  out, each with `at` (an address, base and offset, register or GTE
  register), `bytes`, and `why`. Or `by` (a PC) in place of `at` and
  `bytes`: every load that instruction makes, wherever it reads, for data
  whose address changes from level to level. Capture's read audit lists every other read
  the binding does not account for, and drafts these entries.
- **Hardware reads** (a hardware register or the BIOS ROM) are never state,
  inputs or params: the port has no such registers. When the step's result
  depends on the value (a timer as a seed, the pad read from the serial
  port), bind the load as an inline read of an interface call, and the
  trace records what the hardware gave each step. When it does not (waiting
  for the GPU, acknowledging an interrupt), explain it by its address.

### Capturing on the PlayStation

`psx mechanic check [ID]` checks `interfaces/`, and each mechanic's
`mechanic.toml`, `binding.toml` and traces. `psx mechanic capture ID --name
NAME --from-state STATE --frames N [--input|--replay]` runs the game on
psx's native core and writes `traces/NAME.trace.jsonl`. It captures only
when the check finds nothing, and when the media is the binding's image.

- **Reads at a PC** are taken just before the instruction there runs, with
  the registers as that instruction reads them. If an interrupt is taken
  there instead, they are taken again when it runs.
- **A step** begins at `begin` with every filter matching, reading the
  params (first step only), the inputs and the state, and latching the
  registers for `base` locations. It ends at `end` with the state read
  again. With `next_begin`, the last step, which never ended, is dropped. A
  begin while a step is open is an error with an end PC or `frame`: the
  bracket is wrong.
- **An entry call** is recorded when its PC runs inside a step. Its
  response is read when the PC reaches the return address with the stack
  pointer as at entry. Calls the world makes while answering are not
  recorded.
  - A response may be in memory, such as a result written through a
    pointer the call was given. The call may change the step's state
    there, and those bytes are the step's from then on.
- **An inline read** is recorded when its PC runs inside a step, outside a
  call, and loads.
- **The read audit** (the header's `audit`) lists what the step's own
  instructions read that the binding does not account for, grouped by
  reading PC. Instructions of a world call or of an exception handler are
  not the step's.
  - **Memory**: a load from bytes the step did not write, outside the
    state, inputs, params, calls and `explained`. It counts once the step
    uses what it loaded (reads the register, stores it, or passes it to the
    world), so an epilogue restoring the caller's registers reads nothing.
    Bytes a call wrote outside its response are named as the call's.
  - **CPU registers** read before the step wrote them, other than the
    bases, filters, register inputs and params, and `explained`. zero, gp,
    sp and ra are the ABI's and never listed. A register saved to the
    step's stack frame and restored is not used. A register a call wrote,
    outside its response, is named as the call's.
  - **GTE registers** read before the step wrote them, by moves and by
    commands (RTPS reads the rotation matrix), other than GTE inputs,
    params and `explained`.
  - The result drafts `explained` from the list: base and offset for
    small offsets or moving addresses, else the address, and the range read
    for a table. The agent binds what the result depends on and gives the
    rest a `why`.
- **External changes** are counted per state field in the result: code
  outside the step changed the field between steps. The trace keeps them
  as `state_before`.
- **Branch coverage** (the header's `coverage`) counts each conditional
  branch's two ways. The branches are those of the step's code: walked from
  the begin PC and from every register jump the step took, through its
  calls (not the world's, nor the kernel's below 0x80010000), to returns
  and the end PC, in RAM as at the first step. `missed` names each branch
  with a way not taken: "never taken", "never fell through" or "never ran".
- **A used mechanic's step** runs from its binding's `begin` PC, with its
  filters matching, to the return to its caller, so it begins at a
  routine's entry. Its own `end` is for its own traces.
  - It is recorded as a call. Its fields are read through its own binding,
    with the registers as its step begins.
  - Its instructions are its own: they are left out of the step's read
    audit and coverage, as a world call's are. It must have no unobserved
    state.
  - Like an entry call's, its response's bytes may change and are the
    step's from then on.
- **Refusals** stop the trace from being written, and are listed with step
  and field:
  - a value that is not of the field's type;
  - a lossy rescale;
  - a stored integer the map does not name;
  - a store to the state while a call is under way;
  - a step that ends before its call returns.

## 5a. A binding written as Rust

In place of `binding.toml`, `mechanics/<id>/binding/` may hold a crate: a
small program against `psx-capture`, the vendored API, that says where the
step is and reads the fields itself. `psx mechanic capture` builds it with
cargo, loads it as a plugin and asks it questions at the step's hooks; the
emulator, the read audit, coverage, external changes, refusals and the trace
stay psx's. For the same recordings its traces are the TOML binding's.

```toml
# mechanics/croc.turn/binding/Cargo.toml (a member of the workspace)
[package]
name = "croc-turn-binding"        # the mechanic's id, dots as hyphens, then -binding
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
croc-turn = { path = ".." }
psx-capture = { workspace = true }

[build-dependencies]
psx-mechanics = { workspace = true }
```

```rust
// build.rs: the glue between the mechanic's structs and the trace
fn main() {
    psx_mechanics::rust::write_glue(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
}
```

```rust
// src/lib.rs
use croc_turn::{Inputs, State};
use psx_capture::prelude::*;

mod glue {
    include!(concat!(env!("OUT_DIR"), "/glue.rs"));
}

struct Turn;

impl Binding for Turn {
    type Mechanic = glue::Mechanic;
    const TITLE: &str = "Croc - Legend of the Gobbos (Europe) SLES-00593";
    const IMAGE_SHA256: &str = "b30a07f5…";

    fn bracket(&self, b: &mut Bracket) {
        b.begin(0x8004_425C).end(0x8002_D024);
        b.explain(At::Base(A0, 0x0), 4, "the strat program counter: the interpreter's");
    }

    fn state(&self, m: &Machine) -> State {
        State { heading: m.u16(m.latched(A0) + 0x36A), turn_speed: m.i32(0x8007_51D0) }
    }

    fn inputs(&self, m: &Machine) -> Inputs { /* … */ }

    fn params(&self, _: &Machine) {}
}

psx_capture::export!(Turn);
```

- **`bracket`** names the begin and the end (`end`, `end_at_next_begin`,
  `end_at_frame`), PCs in an overlay as `Pc::in_overlay("name", pc)`, and
  the explained reads: `explain(At::…, bytes, why)` for a place,
  `explain_reader(pc, why)` for every load an instruction makes.
- **`is_step`** (default: always) is asked each time the begin PC is about
  to run: whether this call of the routine is the mechanic's step. It
  replaces register filters, and can compare with memory: the game's own
  pointer to the player, or a mode. On `false` nothing is recorded.
- **`state`** is read at the begin and at the end, **`inputs`** at the
  begin, **`params`** once, at the first step's begin. The compiler checks
  every field is given; psx checks each value is of its field's type.
- **`Machine`** reads memory (`u8` to `i32`, `bytes`), CPU registers now
  (`reg`) and as latched at the begin (`latched`), and the GTE (`gte`).
  Only main RAM and the scratchpad can be read: any other address stops
  the binding and is the capture's refusal. Every byte and register it
  reads at the begin is accounted for in the read audit; params' bytes in
  every step. The audit's draft is written as `b.explain(…)` calls.
- **The glue** (`build.rs`) is generated from the mechanic's source: its
  id, version and fingerprint, and its structs in the trace's encoding.
  psx refuses a binding whose fingerprint is not the mechanic's.
- **The boundary.** The crate is a plugin: `export!` wraps it in two C
  functions, so it loads into a psx built by another compiler. psx refuses
  one built against another `psx-capture`, naming `psx template update`. A
  panic in the binding is caught and reported as the capture's error.
- **Not yet:** interface calls and used mechanics, and state left
  unobserved. A mechanic that requires an interface or uses a mechanic is
  bound in `binding.toml` for now.

## 6. Traces: `traces/<name>.trace.jsonl`

UTF-8 JSON Lines. The first line is the header and each later line is one
step. Lines hold only what changed, so a trace stays small; `Trace::expand`
fills the steps back in.

### Header

```json
{"format": "mechanic-trace", "format_version": 1,
 "mechanic": "croc.walk", "mechanic_version": 1, "fingerprint": "3f9a0c21d4e87b56",
 "source": {"kind": "captured", "platform": "psx", "image_sha256": "…"},
 "producer": {"name": "psx", "version": "0.8.0", "details": {"from_state": "…"}},
 "scenario": "walk_forward",
 "params": {"target": 35, "curve": [4096, 4096, 3072, 2048, 1024, 512, 256, 0]},
 "initial_state": {"speed": 0},
 "unobserved": ["anim_timer"],
 "audit": {"unaccounted": [{"at": {"pc": "0x80043260", "address": "0x800A6000"}, "note": "4 bytes, every step"}]},
 "coverage": {"reached": 11, "total": 14, "missed": [{"at": {"pc": "0x800432A8"}}]},
 "description": "Walk forward from standstill, then let go."}
```

- **`fingerprint`** is a hash of what the mechanic's version covers when
  the trace was captured: its id, its fields' types, `requires` and `uses`
  (not titles, units or descriptions). A trace whose mechanic has changed
  them under the same version is refused: bump the version and capture
  again.
- **`source.kind`**: `captured` means recorded from the original, and only a
  captured trace counts as evidence. It names its `platform` and
  `image_sha256`. `synthetic` is for examples and tests.
- **`producer.details`** is the producer's own: how to make the recording
  again.
- **`scenario`** is the scenario the recording was made in, when there is one.
- **`params`** holds every param's value, so the trace stands alone.
- **`initial_state`** is the state at the first step's start: every field but
  the `unobserved` ones.
- **`audit`** and **`coverage`** are standard sections, so stage checks can
  read them. Their `at` is the producer's location, opaque to everything else.
  - `audit.unaccounted` lists reads the binding neither binds nor explains.
    The audit is clear when it is empty.
  - `coverage` counts the branches inside the bracket that the recording
    took (`reached`) out of all of them (`total`). `missed` lists the rest,
    and `branches` every branch, so the traces of a mechanic can be
    combined.

### Steps

```json
{"step": 0, "inputs": {"forward": true}, "calls": [{"interface": "world.floor-height@1", "call": "height", "request": {"x": 0, "z": 0}, "response": {"y": -5}}], "state": {"speed": 35}}
{"step": 1, "state": {"speed": 61}}
{"step": 2, "inputs": {"forward": false}, "state_before": {"speed": 0}}
```

- **`step`** counts from 0, with no gaps.
- **`inputs`** are those that differ from the previous step's. Step 0 has all
  of them.
- **`calls`** are the world calls the step made, in order, with the steps
  of the mechanics it uses. A query has a `response`; a command has none.
- **`state_before`** holds the state fields that code outside the step
  changed since the previous step's end (an external change), with their
  values at this step's start. It is there only when something did.
- **`state`** holds the fields the step changed, with their values at its
  end.
- **`source_frame`** (the producer's field number) and **`note`** are
  optional.

### Conformance

A port's step conforms to a trace when it is given the header's `params`,
starts from `initial_state`, and takes each step's `state_before` wherever
there is one. Fed each step's inputs and recorded responses, it must:

1. make exactly the recorded calls, with equal requests, in order;
2. produce exactly the recorded state after every step. A faithful mechanic
   starts every step from the recorded state and is compared within its
   tolerances.

The first divergence (step, field, expected, actual) is the result of a
failed check.

### Replay in a psx port

The template's `psx_bevy::replay` checks conformance with no emulator. The
game's `tests/mechanics.rs` registers one adapter per mechanic, which moves
trace fields to the port's types and back and answers the world's calls from
the trace. `psx port check` runs that test with `cargo test --locked` and
writes each feature's replay verdict:

- **A feature passes** when each mechanic its catalogue entry lists has an
  adapter, replays every trace in `traces/` without a divergence, and has at
  least one captured trace. A feature with no mechanics does not get this
  far: `port next` asks for them first (Capture).
- **Exact** mechanics run in `Exact` through every step. Every field of
  every step and every request must be equal.
- **Faithful** mechanics start every step from the recorded state. Each
  state field is compared within its `tolerance` (0 when none is given), in
  raw units. Requests must still be equal: an `f32` value equals the
  recorded one only when it is that integer.
- **The adapter is checked.** It must read every param, input and observed
  state field, and record every observed state field.
- **Lists and records** read into the port's own types: `Vec<T>` for a
  list, and a struct whose `Field::from_value` takes its fields with
  `replay::fields(v)`.
- **A used mechanic** is a call too. The port's step calls the used
  mechanic through a trait. The game's implementation runs the port's
  own step. The adapter's implementation queries `<id>@<version>` `step`
  with the used mechanic's inputs and state, and sets its state from the
  response. So a step's replay checks its own logic, and the used
  mechanic's replay checks that mechanic. A divergence names the one that
  differs.
- **The `f32` drift report** runs each exact mechanic in `Float` as well:
  - Each run starts from the recorded state and restarts every K steps. K is
    the mechanic's `hz` rounded (`tick`, `vblank`), or 60 for `event`.
  - It reports each state and request field's worst error in raw units,
    overall and after 1, K/4, K/2 and K steps.
  - A run diverges when a bool, a variant or the calls differ. Diverged runs
    are counted and restart at the next step.
  - Drift never fails a verdict. `psx port status` shows the last check's
    drift while that check's commit is HEAD.

A mechanic counts as **captured** (stage 2 of its feature) when:

- `psx mechanic check` finds no problem in its files;
- it has at least one captured trace;
- every captured trace's read audit is clear;
- `NOTES.md` names the PC of every branch that no captured trace took both
  ways (taken in one trace and fallen through in another counts), as the
  coverage writes it (`0x800432a8`, in any case).

## 7. Changes from the knowledge base's draft

- **Layout.** One workspace is one game. A mechanic has one `binding.toml`
  (no `bindings/<release>.toml`), and there is no `game.toml`. Interfaces
  are `interfaces/<id>.toml`. `step.md` becomes `NOTES.md`: the port's code
  is the step, proven by the traces.
- **New types.** `float` carried as its bit pattern, and `table`, `list` and
  `record` for params.
- **Params.** A param is a value or a location read at the first step's
  start, with `stride` for tables.
- **Calls** are of kind `query` or `command`. Random numbers are a query
  interface.
- **The mechanic** declares `rate` and `exactness`. A faithful mechanic has
  `tolerance`.
- **Byte order** defaults to the platform's.
- **The trace header** gains `scenario`, `audit` and `coverage`, and its
  format is `mechanic-trace` version 1.
- **Step lines** record only what changed: inputs, state, and `state_before`.
- **The binding** names `mechanic_version` and the release under `source`.
  There is no binding `id`.

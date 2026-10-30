# Mechanic formats

A **mechanic** is one step the original runs, captured from the game step by
step: its traces are the specification, and its `step`, written as plain
Rust, is the reference a port is translated from (`docs/port-design.md`,
section 4; `docs/rust-mechanics-plan.md`). This document defines its files.

Mechanics, interfaces and bindings are Rust crates in the port's workspace.
The code that reads and checks them is in three places:

- `crates/psx-mechanics`: reads mechanics, interfaces and the glossary from
  source with `syn`, without building them (`rust.rs`), generates the glue
  bindings and replay need, and defines traces and replay. The JSON Schemas
  of the trace's lines are in `crates/psx-mechanics/schemas/`.
- `crates/psx-capture`: the API a binding is written against.
- `crates/psx/src/mechanic/`: `psx mechanic check` and `psx mechanic
  capture`, which builds a binding and loads it.

The template vendors `psx-mechanics` and `psx-capture`; `psx template
update` brings them to the psx in use.

## 1. Files

```
interfaces/<interface-id>/           the calls mechanics make to the world: a crate with one trait
mechanics/<mechanic-id>/
  Cargo.toml, src/lib.rs             the mechanic: typed state, inputs, params; header; step
  binding/                           where this release keeps them: a plugin psx loads
  traces/<scenario>/<recording>.trace.jsonl
                                     captured steps, from a scenario recording's run
  NOTES.md                           a few lines: what it does, quirks, what traces miss
replay/                              cargo test -p replay: every trace through its mechanic's step
```

- **Ids.** A mechanic id is `<game>.<name>` (`croc.walk`, `gt2.drivetrain`),
  and is also the directory's name. An interface id runs broad to narrow
  (`world.floor-height`, `rng`) and is also its directory's name.
- **Packages** are named after the id with dots as hyphens: `croc-walk`,
  `world-floor-height`; a binding is `croc-walk-binding`. Each crate is a
  member of the workspace's `Cargo.toml`.
- **Segments** are lowercase letters and digits joined by single hyphens,
  separated by dots.
- **Names** of fields, calls and enum variants are `[a-z][a-z0-9_]*`.
- **References.** A mechanic requires an interface, or uses another
  mechanic, as `<id>@<version>`; the workspace holds one version of each.
- `mechanic.toml`, `binding.toml` and `interfaces/<id>.toml`, the earlier
  formats, are no longer read; `psx mechanic check` names any left.

## 2. Value types

Every state field, input, param and interface field has a type, which
fixes its raw value in a trace:

| Rust | Kind | Raw value |
|---|---|---|
| `bool` | bool | `true` / `false` |
| `u8` to `u64`, `i8` to `i64` | int | the integer |
| `type Fx20_12 = i32;` (`UFx…` unsigned) | fixed: integer bits (sign included) and fraction bits | the underlying integer: the real value × 2^frac |
| an enum of unit variants | enum | the variant's name, in snake case (`BackingUp` is `backing_up`) |
| `f32`, `f64` | float | its bit pattern, as an unsigned integer |
| `[T; N]` | table | a list of `N` raw values |
| `Vec<T>` | list | a list of raw values, of any length |
| a struct with named fields | record | an object of raw values by name |

A field's doc comment is its description, except a line `Unit: …`, which is
its unit (`Unit: units/tick`); both are documentation only.

Tables, lists and records hold data in any field: a level's collision
polygons in params, a cache per probe point in state, an object's polygons
in a call's response. They describe it in the game's terms (pieces,
polygons, edges), not its memory layout. Pointers, counts that
only say how long a list is, strides and padding belong to the binding.
Declare only the fields the step reads.

**Raw values** are always exact. In JSON, an integer outside ±2^53 is written
as a decimal string (`"1152921504606846976"`). A JSON string is read as a
variant name when it is a name, and as an integer otherwise. Fractional
numbers are refused.

## 3. The mechanic

```
mechanics/croc.walk/
  Cargo.toml        # package croc-walk
  src/lib.rs        # header constants, documentation, State, Inputs, Params, step
```

```rust
//! Walking
//!
//! Speed closes a quarter of the gap to four times the target each tick.

use world_floor_height::FloorHeight;

pub const ID: &str = "croc.walk";
pub const VERSION: u32 = 1;            // bump on any change to the fields, REQUIRES or USES
pub const RATE: &str = "tick";         // or "vblank", "event"
pub const RATE_HZ: u32 = 25;           // tick and vblank
pub const EXACTNESS: &str = "exact";   // the default; or "faithful"
// pub const TOLERANCE: &[(&str, u64)] = &[("speed", 16)];   faithful only
pub const REQUIRES: &[&str] = &["world.floor-height@1"];
// pub const USES: &[&str] = &["croc.ground-move@1"];

/// Fixed point: 20 integer bits, 12 fraction bits.
pub type Fx20_12 = i32;
/// Fixed point: 4 integer bits, 12 fraction bits, unsigned.
pub type UFx4_12 = u16;

pub struct State {
    /// Unit: units/tick
    pub speed: Fx20_12,
    pub anim_timer: u8,
}

pub struct Inputs {
    /// Forward is held, as the step sees it.
    pub forward: bool,
}

/// One of the level's placed pieces.
pub struct Piece {
    pub heading: u16,
    pub x: i32,
    pub z: i32,
}

pub struct Params {
    pub target: u16,
    pub curve: [UFx4_12; 8],
    /// The level's placed pieces, in the game's order.
    pub pieces: Vec<Piece>,
}

pub fn step(state: &mut State, inputs: &Inputs, params: &Params, world: &mut impl FloorHeight) {
    // …
}
```

`Cargo.toml` names the package and its dependencies, the interfaces and
mechanics it names (`world-floor-height = { path =
"../../interfaces/world.floor-height" }`, `croc-ground-move = { path =
"../croc.ground-move" }`), and nothing else.

- **Title and summary** are the crate documentation's first two
  paragraphs (a trailing full stop is dropped from the title).
- **The three kinds of field**, each a struct, each optional:
  - `State` is what the step owns and carries from one step to the next.
  - `Inputs` are what it reads each step but does not own, as the step sees
    them: after the game's own input handling, or a frame counter's parity.
  - `Params` are constant for the length of a trace; the binding reads
    them once.
- **`RATE`** is how often the original runs the step, so another game can
  run it at its native rate: once per game tick, once per display refresh,
  or when something happens. `RATE_HZ` is the nominal number of steps per
  second; `tick` and `vblank` need it, `event` has none.
- **`EXACTNESS`.**
  - `exact`: the step reproduces every field of every step.
  - `faithful`: each step starts from the trace's recorded state and is
    compared within `TOLERANCE`, the largest difference allowed per int or
    fixed state field, in raw units.
- **`REQUIRES`** lists the interfaces the step calls.
- **`USES`** lists the mechanics whose steps run inside this one's, such as
  Croc's ground movement inside the tick of his script.
  - A used mechanic's step is recorded as a call: `<id>@<version>`, call
    `step`, a query. Its request is the used mechanic's inputs and state as
    its step begins, and its response is its state as it ends. Its own
    traces pin down what happens in between.
  - A name is a state field or an input, not both, so a step's request has
    one value per name.
  - The using step calls the used mechanic's `step` itself, so the
    reference shows how the original composes them. It builds the used
    step's `State` and `Inputs` from its own fields and writes the result
    back: a field both share (Croc's x and z) is the using mechanic's,
    once. It passes its world on, so its `&mut impl` names the used
    mechanic's interfaces' traits too; `REQUIRES` lists only the
    interfaces it calls itself.
  - A used mechanic's params are a field of the using mechanic's
    `Params`, typed by its crate (`pub collide: croc_collide::Params`).
    The used mechanic's binding gives them.
- **`step`** is the rule: `pub fn step(state: &mut State, inputs: &Inputs,
  params: &Params, calls: &mut impl Calls)`, taking `&Inputs`, `&Params`
  and the calls only as needed, in that order. It is the reference a port
  is translated from, so it reads as the rule rather than the original's
  instructions: names in the game's terms, a comment on each wrap, shift or
  truncation the result depends on, and helpers and `const` tables of its
  own where they make it clearer. Replay runs it against the traces
  (section 6). A mechanic whose step is not written yet declares its types
  only.
- **Types** are declared in `src/lib.rs`: fixed-point aliases whose widths
  add up to the integer they alias, other aliases, enums, structs, and
  array lengths as literals or `const`s of the file.
- **The portable subset.** psx refuses, in every file under `src/`:
  - dependencies other than the workspace's mechanics and interfaces, a
    build script, and `use` of anything else;
  - `unsafe`, foreign functions, `static mut`;
  - macro definitions, and macros other than `assert!` and its kin,
    `matches!`, `panic!`, `unreachable!` and `vec!`;
  - derives other than the standard library's;
  - generic parameters (lifetimes on functions excepted; `impl Trait`
    arguments are fine), trait objects;
  - `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, `VecDeque`, `BinaryHeap`,
    `LinkedList`, `Rc`, `Arc`, `Cell`, `RefCell`, `Mutex`, `RwLock`;
  - floats in an exact mechanic whose fields are all integers.

  What a tool cannot check is asked of the author: game-term names, and no
  transliterated assembly (register-named temporaries, flow that mirrors
  the original's branches rather than the rule).
- **The fingerprint** is a hash of what the version covers: the id, the
  fields' types, `REQUIRES` and `USES` (not titles, units or descriptions).
  Traces record it, and a trace whose mechanic changed them under the same
  version is refused.

## 4. Interfaces

```
interfaces/world.floor-height/
  Cargo.toml        # package world-floor-height; no dependencies
  src/lib.rs
```

```rust
//! Height of the floor under a point
//!
//! What the original's routine asks, as low-level as it really is.

pub const ID: &str = "world.floor-height";
pub const VERSION: u32 = 1;

pub struct Floor {
    /// Unit: world units
    pub y: i32,
}

pub trait FloorHeight {
    /// The floor under (x, z).
    fn height(&mut self, x: i32, z: i32) -> Floor;
    /// A splash at x.
    fn splash(&mut self, x: i32);
    /// Mark the object at this address as touched.
    fn touched(&mut self, at: u32);
}
```

- **One trait**, whose methods are the calls. Each takes `&mut self`, then
  its request as named arguments; it has no body. The method's doc comment
  is the call's description, the contract a reimplementer's world honours.
- **Two kinds of call.**
  - A method that returns something is a **query**: it asks the world
    something and gets a response. Returning a struct of the crate, its
    fields are the response's; returning any other type, the response is one
    field, `value`.
  - A method that returns nothing is a **command**: it acts on the world,
    such as damaging the object in a box or playing the swipe sound.
- **Order.** Both kinds are recorded and replayed in order.
- **Random numbers** are a query, such as `rng@1` with a call `next`, so a
  replay gets the original's numbers and the port can use its own generator.
- **Field types.** Request and response fields are any of the value types
  (section 2), tables, lists and records included: an object the world
  answers with, its collision polygons and all.
- The crate keeps to the portable subset, with no dependencies.

## 5. The binding

The binding says where one release of the game keeps the mechanic's fields,
and where its step and calls are. It is a small Rust program against
`psx-capture`: `mechanics/<id>/binding/`, a `cdylib` that `psx mechanic
capture` builds with cargo, loads as a plugin and asks questions at the
step's hooks. The emulator, the read audit, coverage, external changes,
refusals and the trace are psx's.

```toml
# mechanics/croc.walk/binding/Cargo.toml
[package]
name = "croc-walk-binding"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
croc-walk = { path = ".." }
psx-capture = { workspace = true }

[build-dependencies]
psx-mechanics = { workspace = true }
```

```rust
// build.rs: the glue between the mechanic's structs and the trace, and
// a marker type per call of each interface the mechanic requires
fn main() {
    psx_mechanics::rust::write_glue(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
}
```

```rust
// src/lib.rs
use croc_walk::{Inputs, Params, Piece, State};
use psx_capture::prelude::*;

mod glue {
    include!(concat!(env!("OUT_DIR"), "/glue.rs"));
}

use glue::floor_height::{Height, Splash, Touched};

/// The game's pointer to the player's object, on any level.
const PLAYER: u32 = 0x8007_4B90;

struct Walk;

impl Binding for Walk {
    type Mechanic = glue::Mechanic;
    const TITLE: &str = "Croc (Europe) SLES-00593";
    const IMAGE_SHA256: &str = "…";   // the media identity psx context reports

    fn bracket(&self, b: &mut Bracket) {
        b.begin(0x8004_3234).end_at_next_begin();
        b.query::<Height>(Site::entry(0x8005_1A00), |m| (m.reg(A0) as i32, m.reg(A1) as i32), |m| m.reg(V0) as i32);
        b.command::<Splash>(Site::entry(0x8005_1B40), |m| m.reg(A0) as i32);
        b.command::<Touched>(Site::store(0x8004_3B10), |m| m.store().address);
        b.explain(At::Base(A0, 0x80), 0x20, "the animation, another mechanic");
        b.explain_reader(0x8004_3A30, "finding the floor polygon: level data, wherever the level put it");
    }

    fn is_step(&self, m: &Machine) -> bool {
        m.reg(A0) == m.u32(PLAYER)
    }

    fn state(&self, m: &Machine) -> State {
        State { speed: m.i32(m.latched(A0) + 0x40), anim_timer: m.u8(m.latched(A0) + 0x44) }
    }

    fn inputs(&self, m: &Machine) -> Inputs {
        Inputs { forward: m.u16(0x800A_5C10) & 0x1000 != 0 }
    }

    /// The target is an immediate operand (0x800437F8); the curve is the
    /// object's record, from LEVEL.BIN; the pieces are MP010_00.MAP's, as
    /// the loader placed them.
    fn params(&self, m: &Machine) -> Params {
        let curve_at = m.latched(A0) + 0x1C;
        let header = m.u32(0x8007_515C);
        let pieces = m.u32(header + 0x54);
        Params {
            target: 35,
            curve: std::array::from_fn(|k| m.u16(curve_at + 2 * k as u32)),
            pieces: (0..m.u16(header + 0x6C) as u32)
                .map(|k| pieces + 0x18 * k)
                .map(|p| Piece { heading: m.u16(p + 2), x: m.i32(p + 8), z: m.i32(p + 0x10) })
                .collect(),
        }
    }
}

psx_capture::export!(Walk);
```

(Addresses here are placeholders.)

- **`bracket`** says where the step is:
  - `begin(pc)`, and the end: `end(pc)` (when that PC runs),
    `end_at_next_begin()` or `end_at_frame()` (the next vblank). A PC in an
    overlay is `Pc::in_overlay("name", pc)`, the overlay named exactly as
    `psx inventory` names it, since the same address runs different code as
    overlays change; a plain `u32` is resident code.
  - the interface calls, each with its marker type from the glue: a query
    with `query::<Call>(site, request, response)`, a command with
    `command::<Call>(site, request)`. The request is the method's
    arguments (one value, or a tuple of them), the response what it
    returns. The compiler checks the types. Every call of every required
    interface is declared at least once; a call may be declared at several
    sites.
  - the explained reads (below).
- **Call sites.**
  - `Site::entry(pc)` is a routine entered at that PC. The request is read
    as it is entered and the response when it returns to its caller; at
    both, `latched` is the registers as it was entered.
  - `Site::load(pc)` is a load at that PC that asks the world inline, such as
    a table lookup. The request and response are read once it has loaded;
    `m.load()` gives its `address`, `bytes` and the `value` it gave.
  - `Site::store(pc)` is a store at that PC that tells the world, such as a
    flag set on another object. Only a command can be there. Its request is
    read once it has stored; `m.store()` gives its `address`, `bytes` and
    the `value` stored.
- **`is_step`** (default: always) is asked each time the begin PC is about
  to run: whether this call of the routine is the mechanic's step, not an
  enemy's that runs the same code. It can compare with the game's own
  pointer to the player, or check a mode, so the binding works on every
  level. On `false` nothing is recorded and the routine runs on.
- **`state`** is read at the begin and at the end, **`inputs`** at the
  begin, **`params`** at the first step's begin. Capture reads them again
  at a later begin whenever the memory they were read from has changed
  (at every begin when the binding read registers for them), and stops
  with the field that differs: params that change, such as level geometry
  the game swaps in, belong somewhere else. The compiler checks
  every field is given; psx checks each value is of its field's type.
  Constants in the code go in as values; values the game loads, such as a
  car's gear ratios, are read, so every trace carries the values of the run
  it came from.
- **`Machine`** reads memory (`u8` to `i32`, `bytes`), CPU registers now
  (`reg`) and as latched at the begin or a call's entry (`latched`), the
  GTE (`gte`: data 0 to 31 as `mfc2` numbers them, control 32 to 63), an
  inline call's load (`load`) and a command's store (`store`). Only main RAM and the scratchpad can be
  read: any other address stops the binding and is the capture's refusal.
  Registers are named as `A0`, `V0`, `SP`, …, `HI` and `LO`.
- **State is read where it is**, at the begin and again at the return:
  usually memory, but a value the game keeps in a register from one step to
  the next is read with `reg` like any other.
- **A value the game leaves meaningless**, such as a found polygon's number
  when no piece was found (the routine stores whatever its stack held, and
  every reader checks the piece first), is reported as 0. The binding still
  reads its bytes, so the audits account for them, and the mechanic writes
  0 there too. Every reader of the value must be checked to ignore it.
- **A used mechanic** needs nothing in the using mechanic's binding: its
  own binding records its step, the calls it makes and its params (section
  5, "Capturing"). The using binding gives only its own params: with a
  used mechanic's params among them, the glue declares `Params` without
  those fields, and `params` returns it (or `()` when nothing is left).
- **The glue** (`build.rs`) is generated from the mechanic's source and the
  interfaces it depends on: its id, version and fingerprint, its structs in
  the trace's encoding, and a module per interface (`FloorHeight` is
  `floor_height`) with a marker per call (`height` is `Height`). psx
  refuses a binding whose fingerprint is not the mechanic's. Nothing
  generated is committed. A binding whose glue names an interface's own
  types (a struct it returns) depends on that interface's crate too.
- **The boundary.** The crate is a plugin: `export!` wraps it in two C
  functions, so it loads into a psx built by another compiler. psx refuses
  one built against another `psx-capture`, naming `psx template update`. A
  panic in the binding is caught and reported as the capture's error.

### Explained reads and stores, and hardware

- **`explain(at, bytes, why)`**: a read or store the step makes that the
  binding deliberately leaves out, at `At::Address(a)` (any address), `At::Base(reg,
  offset)` (from a register as latched at the begin), `At::Register(reg)` or
  `At::Gte(n)`. **`explain_reader(pc, why)`**: every load that instruction
  makes, wherever it reads, for data whose address changes from level to
  level. Capture's read and write audits list every other read and store
  the binding does not account for, and draft these calls.
- **Hardware reads** (a hardware register or the BIOS ROM) are never state,
  inputs or params: the port has no such registers. When the step's result
  depends on the value (a timer as a seed, the pad read from the serial
  port), declare the load as an inline call of an interface, and the trace
  records what the hardware gave each step (`m.load().value`). When it does
  not (waiting for the GPU, acknowledging an interrupt), explain it by its
  address.

### Capturing on the PlayStation

`psx mechanic check [ID]` checks `interfaces/` and each mechanic's crate and
traces. Every trace comes from a scenario recording (`docs/scenarios.md`):
`psx mechanic capture [ID...] --scenario S --recording R` runs that
recording again on psx's native core, from the scenario's state with the
recording's input, and captures in that one run every mechanic named, or
every mechanic with a binding. Each is written to
`traces/S/R.trace.jsonl`.

- **The run is the recording's.** The glossary's binding runs alongside, and
  its quantities must equal the recording's at every tick; otherwise
  nothing is written (`unlike_the_recording`, with the first differences).
  A recording made from a state its chain no longer makes is refused: record
  it again.
- **From the initial moment.** Steps are captured from the end of the
  recording's first tick, where the port's launcher starts, and each step
  carries the tick it ran in.
- **Per mechanic.** A mechanic captures only when the check finds nothing in
  its files, the binding checks against the mechanic and its interfaces, and
  the image is the binding's. Named mechanics must; with none named, one
  that cannot is listed under `skipped` and the rest are captured. A
  mechanic whose step never ran in the recording reports `no_steps` and
  gets no trace.

- **Reads at a PC** are taken just before the instruction there runs, with
  the registers as that instruction reads them. If an interrupt is taken
  there instead, they are taken again when it runs.
- **A step** begins at the begin PC when `is_step` says yes, reading the
  params (first step only), the inputs and the state, and latching the
  registers. It ends at the end with the state read again. With
  `end_at_next_begin`, the last step, which never ended, is dropped. A begin
  while a step is open is an error with an end PC or `end_at_frame`: the
  bracket is wrong.
- **An entry call** is recorded when its PC runs inside a step. Its
  response is read when the PC reaches the return address with the stack
  pointer as at entry. Calls the world makes while answering are not
  recorded.
  - A response may be in memory, such as a result written through a
    pointer the call was given. The call may change the step's state
    there: what the binding reads for the response is the step's from then
    on.
- **An inline call** is recorded when its PC runs inside a step, outside a
  call, and loads.
- **A used mechanic's step** runs from its binding's begin PC, when its
  `is_step` says yes, to the return to its caller, so it begins at a
  routine's entry. Its own end is for its own traces.
  - It is recorded as a call: its binding reads its inputs and state as
    the request, and its state on return as the response, with the
    registers as its step begins.
  - The calls its step makes are recorded by its own binding, in its
    entry's `calls`, in order.
  - Its params are read by its binding as its step is first entered, into
    the using mechanic's params field for them.
  - Its instructions are its own: they are left out of the step's read
    audit and coverage, as a world call's are.
- **The read audit** (the header's `audit`) lists what the step's own
  instructions read that the binding does not account for, grouped by
  reading PC. Instructions of a world call or of an exception handler are
  not the step's.
  - **Accounted for**: every byte and register the binding read at the
    begin (to choose the step and read its inputs and state) and for an
    inline call, the params' bytes in every step, and the explained reads.
  - **Memory**: a load from bytes the step did not write and the binding
    does not account for. It counts once the step uses what it loaded
    (reads the register, stores it, or passes it to the world), so an
    epilogue restoring the caller's registers reads nothing. What an entry
    call's request reads is what the step passes the world, and counts the
    same way. Bytes a call wrote outside its response are named as the
    call's.
  - **CPU registers** read before the step wrote them, other than those the
    binding read and the explained ones. zero, gp, sp and ra are the ABI's
    and never listed. A register saved to the step's stack frame and
    restored is not used. A register a call wrote, outside what its
    response reads, is named as the call's.
  - **GTE registers** read before the step wrote them, by moves and by
    commands (RTPS reads the rotation matrix), likewise.
  - The result drafts `b.explain(…)` calls from the list: base and offset
    for small offsets or moving addresses, else the address, and the range
    read for a table. The agent reads what the result depends on and
    explains the rest with a `why`.
- **The write audit** (the header's `audit.unaccounted_writes`) lists what
  the step's own instructions store that is read afterwards and the binding
  does not account for, grouped by storing PC, with who read it.
  - **An output** is a store whose bytes something reads before anything
    stores there again: the mechanic's next step, other code after the
    step, an exception handler, or a world call during the step. A store
    nothing reads before it is overwritten, such as a scratch variable, is
    not one. Neither is a store through sp, to the step's own stack frame.
  - **Accounted for**: bytes the binding reads for the state at the step's
    end, what a call's request reads (what the step passes the world, a
    used mechanic's request included), a command's store, and the
    explained stores. Every other output is listed.
  - A store to a hardware register is an output unless explained.
  - An output only the next step reads is state the mechanic carries: bind
    it in `State`. One other code reads is the step telling the world: bind
    the store as a command, or, when it is not behaviour, explain it.
  - The draft's `b.explain(…)` lines say `stored at` for these.
  - Like coverage, it sees what the recording ran: a store whose reader
    never runs in the recording looks like a scratch variable.
- **External changes** are counted per state field in the result: code
  outside the step changed the field between steps. The trace keeps them
  as `state_before`.
- **Branch coverage** (the header's `coverage`) counts each conditional
  branch's two ways. The branches are those of the step's code: walked from
  the begin PC and from every register jump the step took, through its
  calls (not the world's, nor the kernel's below 0x80010000), to returns
  and the end PC, in RAM as at the first step. `missed` names each branch
  with a way not taken: "never taken", "never fell through" or "never ran".
- **Refusals** stop the trace from being written, and are listed with step
  and field:
  - a value that is not of the field's type;
  - a read outside main RAM and the scratchpad, or a panic in the binding;
  - a store a call makes to the step's state outside its response;
  - a step that ends before its call returns.

## 6. Traces: `traces/<scenario>/<recording>.trace.jsonl`

UTF-8 JSON Lines. The first line is the header and each later line is one
step. Lines hold only what changed, so a trace stays small; `Trace::expand`
fills the steps back in.

### Header

```json
{"format": "mechanic-trace", "format_version": 1,
 "mechanic": "croc.walk", "mechanic_version": 1, "fingerprint": "3f9a0c21d4e87b56",
 "source": {"kind": "captured", "platform": "psx", "image_sha256": "…"},
 "producer": {"name": "psx", "version": "0.8.0", "details": {"chain": "notes/states/level_1_1_start.json", "from_state_sha256": "…"}},
 "scenario": "level_1_1_ground", "recording": "walk_forward",
 "params": {"target": 35, "curve": [4096, 4096, 3072, 2048, 1024, 512, 256, 0]},
 "initial_state": {"speed": 0},
 "audit": {"unaccounted": [{"at": {"pc": "0x80043260", "address": "0x800A6000"}, "note": "4 bytes, every step"}]},
 "coverage": {"reached": 11, "total": 14, "missed": [{"at": {"pc": "0x800432A8"}}]},
 "description": "Walk forward from standstill, then let go."}
```

- **`fingerprint`** is the mechanic's (section 3) when the trace was
  captured. A trace whose mechanic has changed under the same version is
  refused: bump the version and capture again.
- **`source.kind`**: `captured` means recorded from the original, and only a
  captured trace counts as evidence. It names its `platform` and
  `image_sha256`. `synthetic` is for examples and tests.
- **`producer.details`** is the producer's own: how to make the recording
  again.
- **`scenario`** and **`recording`** name the scenario recording whose run
  the trace was captured in, as its path does.
- **`params`** holds every param's value, so the trace stands alone.
- **`initial_state`** is the state at the first step's start.
- **`audit`** and **`coverage`** are standard sections, so stage checks can
  read them. Their `at` is the producer's location, opaque to everything else.
  - `audit.unaccounted` lists reads the binding neither reads nor explains,
    and `audit.unaccounted_writes` stores read afterwards that the binding
    neither binds nor explains. The audit is clear when both are empty.
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
  A used mechanic's step has `calls` of its own: the world calls it made.
- **`state_before`** holds the state fields that code outside the step
  changed since the previous step's end (an external change), with their
  values at this step's start. It is there only when something did.
- **`state`** holds the fields the step changed, with their values at its
  end.
- **`tick`** is the recording's tick the step ran in: its quantities are the
  recording's at that tick's end. **`source_frame`** is the recording's
  field, and **`note`** is optional.

### Conformance

A step conforms to a trace when it is given the header's `params`, starts
from `initial_state`, and takes each step's `state_before` wherever there
is one. Fed each step's inputs and recorded responses, it must:

1. make exactly the recorded calls, with equal requests, in order;
2. produce exactly the recorded state after every step. A faithful mechanic
   starts every step from the recorded state and is compared within its
   tolerances.

The first divergence (step, field, expected, actual) is the result of a
failed check.

### Replay of the reference

The workspace's `replay` crate runs every trace of every mechanic through
the mechanic's own `step`, with no emulator, and checks conformance as
above:

```
cargo test -p replay
```

- **Each mechanic is a dependency** of `replay/Cargo.toml`, by path
  (`croc-turn = { path = "../mechanics/croc.turn" }`). One that is not
  fails the run, saying so. A mechanic with no `step` yet is listed and
  skipped.
- **The glue is generated.** `replay/build.rs` reads each mechanic's source
  (`psx_mechanics::rust::write_replay`) and writes the conversions between
  its structs and the trace's values and the call to its `step`. Nothing
  generated is committed.
- **Exact** mechanics run from the initial state through every step;
  **faithful** ones start every step from the recorded state and compare
  within `TOLERANCE`. A trace captured with another fingerprint is refused.
- **Calls are answered from the trace.** A `step` that takes `&mut impl`
  its interfaces' traits is given `World`, generated per mechanic, whose
  methods check that the step makes the trace's next call with an equal
  request and return its recorded response. Each interface the mechanic
  depends on is a dependency of `replay/Cargo.toml` too.
- **A failure** names the trace, the step and the field, with the path into
  a table or record (`last.heading is 7, the trace has 8`), or the call
  (`call 2 (world.floor-height@1.height) request.x is 5, the trace has 6`;
  `the step made 1 calls, the trace has 2`).
- **A used mechanic's step** runs as the using step calls it. Replay does
  not see it as a call: the world calls it makes are answered from its
  entry's, in order, and a failure inside it names the entry (`call 0
  (croc.collide@1.step): call 3 (world.objects@1.surfaces) request.x is 5,
  the trace has 6`). Its entry's request and response are not compared
  here: `psx mechanic check` compares each entry with the used mechanic's
  own trace of the same recording (the same steps at each tick, with the
  same calls, and the same params), and replay proves that trace; a
  recording where the used step never runs needs no trace of it. So a
  mistake in what the using step gives the used one also shows inside
  the entry, at the first call it changes: the used mechanic's own traces
  replaying exactly is what puts it in the using step. A used mechanic
  that uses mechanics itself is not replayed yet.

The port is not replayed step by step: it is a translation, checked from
outside by scenarios (`docs/scenarios.md`).

A mechanic counts as **captured** (stage 2 of its feature) when:

- `psx mechanic check` finds no problem in its files;
- it has at least one captured trace;
- every captured trace's read and write audits are clear;
- `NOTES.md` names the PC of every branch that no captured trace took both
  ways (taken in one trace and fallen through in another counts), as the
  coverage writes it (`0x800432a8`, in any case).

## 7. Changes from the knowledge base's draft

- **Layout.** One workspace is one game. A mechanic has one binding (no
  `bindings/<release>/`), and there is no `game.toml`. `step.md` becomes
  `NOTES.md` and the mechanic's `step`.
- **Rust in place of TOML.** The mechanic, its interfaces and its binding
  are Rust crates: the mechanic's types and step are plain, portable Rust,
  and the binding is a program against `psx-capture`, so pointers, counts,
  parallel arrays and the player's object on any level are ordinary code.
- **New types.** `float` carried as its bit pattern, and `table`, `list` and
  `record`.
- **Calls** are queries or commands. Random numbers are a query interface.
- **The mechanic** declares `RATE` and `EXACTNESS`. A faithful mechanic has
  `TOLERANCE`.
- **The trace header** gains `fingerprint`, `scenario`, `audit` and
  `coverage`, and its format is `mechanic-trace` version 1.
- **Step lines** record only what changed: inputs, state, and `state_before`.

# Plan: a plain Rust base template, with add-ons

Today every port is a Bevy workspace: `psx port init` writes the
`psx-bevy` template, and an agent learns Bevy's ECS, scheduling and
renderer before it writes a rule. psx itself never compiles Bevy, but
every port does: a cold Bevy build, `App`/`World`/`FixedUpdate` in every
scenario launcher, and a developer MCP built on reflected components.

Agents can write their own engine and keep it clean, and the review stage
checks that they do. What a port needs from psx is a head start: the parts
that let it talk to psx, done right, so the agent's first turn is the
game.

So:

- **The base template** holds only what connects the port to psx. It
  has no window, no renderer and no engine.
- **Add-ons** can be added to a workspace at any time (`psx port add
  NAME`). Each one writes example code: a working, tested crate the agent
  adapts into its game, merges, or deletes. Once added, it is the
  project's code, like the template's seeded files.

Bevy stops being a dependency of anything psx ships. A port may still use
Bevy, or any other library, as the agent chooses.

## What psx relies on today

psx calls the port through four contracts. Only the code behind them is
Bevy:

| Contract | What psx calls and reads | Behind it now |
|---|---|---|
| port check (`port/check.rs`) | `cargo test --test scenarios` with `PSX_SCENARIO_REPORT`; the report JSON (kinds, quantities, scenarios, problems, failures) | `psx_bevy::scenario`: launchers take `&mut App`, observers `&mut World`, ticks run through `TickApp` |
| `Observed` (`psx-scenario/src/rust.rs`) | the game's build generates it from `glossary/` | the generated code names `psx_bevy::scenario::Observation` |
| psx web's port replay (`web.rs`) | `cargo run -p game -- --replay REC --frames DIR`: `<tick>.png`, `frames.json` | `psx_bevy::frames`, Bevy's renderer and screenshots; psx checks for `psx-bevy/src/frames.rs` to know the port has it |
| port init / port update (`template.rs`) | the embedded template; `psx-bevy/` and `vendor/` tracked by hash | the whole template |

Smaller ties: `assets_textures` writes `scene.psxscene` for the Bevy
template's `--scene` viewer; `psx play`'s default bindings copy the
template's; psx web's port key hashes the `psx-bevy` directory; guide
topics, tool descriptions and port next's items say "the Bevy template"
and `psx_bevy::scenario`; CI formats `templates/psx-bevy`.

Already free of Bevy: `tools/import`, `glossary/`, the vendored
`psx-fixed`, `psx-gte`, `psx-media`, `psx-assets` and `psx-scenario`'s
data, and `psx-bevy`'s `gte.rs` and `settings.rs`.

## The base template

Everything here exists because psx calls or reads it:

```
Cargo.toml            the workspace
game/                 the port: a library and a thin binary
  src/lib.rs          the game value, implementing psx_scenario::Port
  src/main.rs         --replay, and an empty place for the game's own loop
  build.rs            Observed, generated from glossary/
  tests/scenarios.rs  port check's entry point
tools/import/         importers: the player's disc -> the port's formats
glossary/             the quantities scenarios compare
vendor/               psx-scenario, psx-media, psx-assets, psx-fixed,
                      psx-gte, psx-capture
AGENTS.md             what psx expects of the port, and the add-ons
.psx-template.toml    hashes; region, fields_per_tick; the add-ons added
```

### The Port trait and the runner

The runner in `psx_bevy::scenario` uses `App` only to make a fresh
instance, launch it, tick it and observe it. It moves to
`psx_scenario::runner` over a trait:

```rust
pub trait Port {
    /// The scenario kinds this port can launch.
    fn kinds(&self) -> Vec<String>;
    /// Builds the world from the scenario's parameters.
    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String>;
    /// One logic tick with exactly this pad.
    fn tick(&mut self, pad: &Pad);
    /// The glossary's quantities as the port has them now.
    fn observe(&mut self) -> Observed;
    /// The screen after the last tick, for --replay; None until the port draws.
    fn screen(&mut self) -> Option<Frame> { None }
}

#[test]
fn scenarios() {
    Scenarios::new(env!("CARGO_MANIFEST_DIR"), || Game::headless()).run();
}
```

The runner keeps all of its behaviour: a fresh instance per recording,
the initial compare, one tick per line, tolerances, a second run to check
determinism, and a panic failing one recording only. The report JSON does
not change, so port check, port next and verdicts need no changes.

`Pad` is the PS1 pad as recordings hold it: 16 buttons, analog mode and
sticks as raw bytes. `Observation` moves to `psx_scenario`, and the
generated `Observed` implements it. `Frame` is RGBA at the game's own
resolution.

### --replay

A driver in `psx_scenario` runs a `Port` through a recording. It writes
`<tick>.png` from `screen()` and the same `frames.json` psx web reads now.
The base's `main.rs` calls it for `--replay`. Before the port draws
anything, the driver writes `frames.json` saying there is no screen, and
psx web shows that instead of starting a build that can never finish.

### What the base leaves out

The base has no window, game loop, input devices, renderer, audio,
menus, start-up import or developer MCP. These are add-ons. `cargo run`
without `--replay` prints what the base is for and that `psx port add
full` makes it a game someone can play.

## Add-ons

`psx port add NAME` writes `addons/NAME/` as a workspace member. That
crate builds and tests on its own, and has an example that runs it. The
agent may use it as it is, adapt it in place, or move what it wants into
game/ and delete the rest. The
command also notes the add-on in `.psx-template.toml` and AGENTS.md.
Add-ons are seeded files: `port update` never overwrites them. When a
newer version exists, `port update` puts it beside them as a
`.psx-new` copy, the same way it treats edited template files. An add-on
may depend on another (the renderer on the shell), and `port add` adds
that one too.

Ports read only modern formats: glTF, PNG, common audio and video
formats, and the port's own data files. The original's formats (TIM, TMD,
VAG, XA, STR, `.mcd` and the game's own) are read only by the importers
in `tools/import`, which convert them once when the player imports the
disc. No add-on reads them either.

None of them is required, and port next never asks for one. A port that
only extracts one feature's rules needs none of them; its scenarios pass
on the base alone. The guide lists them in topic setup.

A full port wants the same few things every time: a window that goes
borderless fullscreen at any aspect ratio, a switch between an accurate
PS1 look and a modern one, and the player's disc imported on first start.
`psx port add full` adds that set in one step (shell, renderer,
disc-import, menus), wired together in `game/src/main.rs` so `cargo run`
plays the demo straight away. Each piece can also be added alone.

1. **shell**: the window and the game loop. A fixed-step tick at the
   region's rate divided by `fields_per_tick`, with drawing as often as
   the display allows. It uses winit: windowed or borderless fullscreen,
   any window size and aspect ratio, toggled at run time and remembered.
   It also covers the pad from keyboard and gamepad (gilrs), using psx
   play's default bindings, so a person plays the port with the keys they
   used for the original. This is the minimum for a port someone can play.
2. **renderer**: wgpu, with two looks the player switches between at run
   time. The interface is the same for both: models, materials, textures,
   a camera and sprites, not GPU commands.
   - *PS1*: draws at the PS1's resolution into an offscreen target and
     scales it to the window. The height stays the game's, and the width
     grows with the aspect ratio, as psx-bevy does now. The look comes
     from the PS1: affine texture mapping, vertices snapped to whole
     pixels, ordered dithering to 15-bit colour, the semi-transparency
     modes, depth cue and fog, nearest-neighbour texels.
   - *Modern*: draws at the window's resolution with perspective-correct
     texturing, filtered textures, full colour and antialiasing, from the
     same models. The game's own field of view, widened for the aspect
     ratio as in the PS1 look.

   `Frame` comes from it, in either look, so `--replay` works once it is
   in. Headless runs (`--replay`, dev-mcp screenshots, its own tests) use
   wgpu's fallback adapter, so CI and agent sandboxes need lavapipe or
   SwiftShader installed. Its image tests compare with a tolerance, since
   drivers rasterise differently. It loads psx-assets' glTF and the
   import's textures as they are.
3. **disc-import**: the player's disc imported on first start. It finds
   the disc (a path given on the command line, the last one used, or one
   the player chooses). It runs `tools/import` with a progress screen and
   keeps the result in the player's data directory. It imports again when
   the importers change. The importers themselves stay in the base,
   because port check runs their tests; this add-on is only the start-up
   flow around them.
4. **text**: a bitmap font for menus and HUDs, drawn through the
   renderer, from a PNG atlas and a glyph table. A game that wants the
   original's font has its importer write it in that form.
5. **menus**: controls (rebinding) and display (look, fullscreen,
   aspect, the PS1 look's effects) settings, saved to a file. This is
   psx-bevy's controls and display menus, without Bevy.
6. **dev-mcp**: the port's own MCP for the agent: status, step, set input,
   pause, screenshot, and query. Query returns `Observed` and whatever
   JSON the game chooses to give. It needs only the `Port` trait, plus
   the shell for live play.
7. **saves** (low priority): a one-time import of the player's memory
   card saves from the original (`.mcd`, as `assets_memcard` reads them)
   into the port's own save format. The port never reads `.mcd` itself.

`full`'s four (shell, renderer, disc-import, menus) cover what psx-bevy
gives now plus the modern look, and phase 3 builds them. text and dev-mcp
follow when a port wants them, and saves comes last.

There is no audio or movie add-on. Sound and video in a port come in
modern formats, and the agent picks the crates that play them (for
example kira or rodio for sound). Turning the disc's VAG, VAB, XA, CD-DA
and STR into those formats is the importers' job, as it is for models
and textures.

## psx

- `port init` writes the base template. `port add NAME` writes an add-on.
  `port update` updates the base's tracked files and offers newer
  add-ons beside the project's copies.
- psx web's replay runs `--replay` and reads `frames.json`. It no longer
  looks for `frames.rs`. The port key hashes the workspace's code.
- `assets_textures`' `scene.psxscene` goes, along with the Bevy scene
  viewer it was for. The renderer add-on's example could take over that
  job later.
- The guide, tool descriptions and port next's items say "the template"
  and `psx_scenario`'s runner. Topic setup lists the add-ons.
- CI formats and tests the base template and each add-on on its own.

### Existing workspaces

The Croc example and GT2 are Bevy workspaces with their own copy of
`psx-bevy/`. Phase 1 makes `psx-bevy/src/scenario.rs` an adapter over
the new runner, keeping `register_scenario`, `.observe(...)` and
`Scenarios::new(root, || App)`. They take that with `port update` and
change no code. After that, psx stops shipping psx-bevy, and those
workspaces keep theirs as their own code. The Croc example has moved to
the base template plus `full`, since it is the showcase: its rules are
plain functions `Game::tick` calls in the Bevy phases' order, and every
quantity of every recording matches the Bevy version's at every tick.

## Phases

Each phase ends with psx's tests and the templates' tests passing.

1. **The runner in psx-scenario.** Move the runner, `Pad` and
   `Observation`. Change the generated `Observed`. Make
   `psx-bevy` an adapter over them. Check: the Bevy template's tests and
   the scenario reports for the Croc example and GT2 (on a scratch copy,
   after `port update`) are byte for byte the same as before.
2. **The base template and psx's side.** The replay driver, the
   template, `port init` writing it, the web's replay check, and the wording in the guide, tool
   descriptions and port next. Check: `cargo test --workspace` on a fresh
   `port init` builds in well under a minute cold. A scenario recorded
   from a disc passes against a launcher written for the test.
3. **`port add` and `full`: shell, renderer, disc-import, menus.**
   Check: on a fresh `port init` plus `port add full`, `cargo run` imports
   a disc and plays the demo; the renderer's example draws a disc model in
   both looks at 4:3 and 16:9, windowed and borderless; `--replay` writes
   frames from it headless.
4. **Trial.** A fresh agent ports one feature of a game on the base
   template and adds what it wants. We compare with the Bevy runs we
   have: build times, how much of its work went to the engine, turns to a
   passing verdict, and what the review stage says about the engine it
   wrote.
5. **The other add-ons**, in the order ports ask for them, starting with
   dev-mcp. (The Croc example moved to `full` before the
   any-executable phases below.)

After this plan: ports in other languages (designed in the section below).
Not started.

Phase 1 is a refactor on its own and worth doing first: it lets a port
leave Bevy without psx noticing.

## After this plan: any executable as a port

Phases (a), (b) and (c) are done; the design below is agreed. psx treats any program that keeps a
contract as a port, in any language, and tests it as it tests a Rust one.

### What ties a port to Rust

- The runner runs inside the port: `port check` runs `cargo test --test
  scenarios`, and psx web's replay runs `cargo run -- --replay`.
- `Observed` is generated as Rust from the glossary (the game's
  `build.rs`).
- The importers are run with `cargo run -p import`, and the build is
  cargo's.
- psx-fixed and psx-gte, which give a port the original's exact
  arithmetic, are Rust libraries.
- The port key hashes the cargo workspace, and the guide and port next's
  items speak of cargo.

### What stays Rust: the glossary and its binding

The glossary (`glossary/src/lib.rs`) and its binding (`glossary/binding/`,
a cdylib `scenario record` loads) stay Rust, by decision. They describe the
original, not the port: where its RAM keeps each quantity, which takes
following pointers, indexing and conditions, so a data format would grow
into a language of its own. They run inside psx and never reach the port;
a C port and a Rust port of one game share the same glossary. Ports in any
language see only the glossary's names and types, over the protocol. A
port in another language therefore keeps a small cargo workspace holding
just the glossary.

### The contract

Phase 1 put the runner behind the `Port` trait (`kinds`, `launch`, `tick`,
`observe`, `screen`). psx implements that trait itself as a `ProcessPort`
that drives a child process, and runs the runner, the comparison and the
verdicts on its own side, unchanged.

A manifest, `psx-port.toml`, names the commands:

```toml
[port]
build  = ["make", "-C", "game"]
run    = ["game/build/game", "--psx"]   # speaks the protocol on stdin/stdout
import = ["game/build/import", "{disc}", "{out}"]
```

The protocol is JSON lines on stdin and stdout, one reply per request, with
a protocol version in `hello`. stdout carries only the protocol; logs go to
stderr.

```
-> {"hello":1}                                     <- {"kinds":["level","menu"]}
-> {"launch":{"kind":"level","parameters":{...}}}  <- {"ok":true} | {"error":"..."}
-> {"tick":{"buttons":["Cross","Up"],"analog":false,"sticks":[128,128,128,128]}}
                                                   <- {}
-> {"observe":{}}                                  <- {"values":{"croc_x":1234,"croc_mode":"Walk"}}
-> {"screen":{"path":"/tmp/f/12.rgba"}}            <- {"width":320,"height":240} | {"none":true}
```

- **Pads** are psx-scenario's `Pad` as it is: button names from `Button`
  (`Cross`, `L1`, `Start`, ...), the analog flag, and the four stick bytes.
  Not the raw 16-bit mask, which is active-low and easy to get the bit
  order of wrong; a misspelt name is refused outright.
- **Observations** use the encoding recordings already use
  (psx-scenario's `Value`): raw integers as JSON numbers, or decimal
  strings outside ±2^53; a fixed-point quantity as its underlying integer,
  never a real; a float as its bit pattern; bools; a variant by name; lists
  and records as JSON arrays and objects. psx checks each value against its
  glossary type with the checks that exist (a value outside its type's
  range is reported as that, which catches a port that forgot to wrap),
  then compares as the runner does today, with `[compare]` and its
  tolerances unchanged.
- **A quantity the port leaves out** is one it has none of, as `None` in
  `Observed` is today ("expected X, the port has none"), so a port grows a
  quantity at a time. A name the glossary does not have is a protocol
  error.
- **Screens**: the port writes the tick's frame as RGBA bytes to the path,
  at the original's display size, or answers `none` when it does not draw
  yet.

Rust ports take the same path: psx-scenario gains `serve(Game)`, which
speaks the protocol and turns `Observed` into values, so existing ports
keep their code. `cargo test` stays a quick local check, and psx keeps a
single runner rather than two.

### Help for exact numbers: the arithmetic spec

The comparison is exact (GT2's steering fails on one unit of `car_speed`),
so a port must compute what the original's integer code computed. Rust
ports get psx-fixed and psx-gte. Ports in other languages get a spec and
test vectors instead; psx never checks how a port reaches its numbers,
only that they match, so the spec is help, not part of the contract.

- **The integer rules** (psx-fixed's `Exact` mode), a couple of pages: a
  number is a raw word of a stated width and fraction bits; arithmetic in
  32-bit `int` (`unsigned` with a `u32` operand), kept to the word's width
  as a store keeps it; `a * b` is the product's low 32 bits shifted right
  arithmetically by `b`'s fraction bits; `a / b` is `(a << G) / b`,
  truncating toward zero, with the R3000's results for a zero divisor; and
  each named operation (`rescale`, `settle_to_zero`, `snap_to_unit`, the
  angle helpers) in pseudocode. It warns C ports that signed overflow is
  undefined and right shifts of negatives implementation-defined:
  `-fwrapv` or small helpers. The `Float` mode is left out; it is for
  drawing.
- **The GTE**: psx-spx's description, plus what psx-gte pins down (the
  44-bit MAC, `sf` and `lm`, saturation into IR, SX/SY and SZ/OTZ, the FLAG
  bits, UNR division) and the libgte-style helpers ports call. The
  `SIN_QUARTER` and UNR tables ship as data files.
- **Test vectors**, made by the Rust crates as the reference: JSON lines,
  hand-picked edge cases (−2^31 / −1, a zero divisor, overflow and
  saturation at each limit, negative shifts) and a seeded batch of random
  cases per operation. psx prints the spec (`psx guide arithmetic`) and the
  vectors (`psx port vectors`), so they match the installed psx. A port
  runs them in its own tests.

A spec rather than a C ABI: other languages write these rules natively in
an afternoon and the vectors catch mistakes, where an ABI means
per-platform builds and an interface to keep stable.

### A C port, end to end

1. **The workspace**: `psx-port.toml`; `glossary/` in Rust as now;
   `scenarios/`, the recordings, `fixes/`, `decomp/`, `names/` and `notes/`
   unchanged; `game/` with a Makefile and C sources; optionally
   `tools/import/` in any language.
2. **Recording** does not change: `scenario record` runs the original on
   psx-core and the binding reads each tick's quantities.
3. **The game** is ordinary C: the state a struct, a tick a function, the
   original's numbers `int32_t` with helpers that follow the spec, tested
   against `psx port vectors` in `make test`.
4. **The protocol mode** (`--psx`): `main` reads a request a line, answers
   it, flushes stdout. A vendored JSON library such as cJSON is enough.
5. **`port check`** runs `build`, starts `run`, and drives each recording
   through `ProcessPort`: launch, then tick and observe each tick. The
   verdicts are today's. Held-out recordings run the same way. Coverage,
   review and the decompile tools work on the original and do not change.
6. **psx web's replay** asks for screens; a port that draws writes them.
7. **Playing**: without `--psx` the same program is the game, with its own
   window and pad (SDL, raylib, ...). The add-ons stay Rust; ports in
   other languages bring their own engine.
8. **Assets**: the manifest's `import` writes modern formats (glTF, PNG)
   from the player's disc, which the game loads at run time.

### The hard parts

- **Speed.** One round trip per tick is fine at 60 Hz for recordings of a
  few thousand ticks. If not, the protocol batches: run these N pads and
  observe after each.
- **Determinism.** Only the pad drives the port: no clock, no unseeded
  randomness. The rule is as now, but psx no longer sees the code. A cheap
  check is running each recording twice and comparing the two.
- **Robustness.** Timeouts, crashes, and the protocol version.
- **The port key.** A hash of the git tree, or of the files the manifest
  names, instead of the cargo workspace.

### What it gains

- **The port never receives the expected values**, held-out or not: it is
  sent pads and asked for values. Nothing in the port can lean on a
  recording, by construction. The held-out recordings stay readable in the
  workspace (the agent runs as the same user, so no storage hides them);
  the guide's rule against reading them stays the safeguard. `port check`
  can also report a held-out failure by tick and quantity without the
  expected value.
- **Unusual ports become testable**, since anything that keeps the
  contract can be checked against the recordings:
  - a recompiled decompilation, which connects to the annotated decompile
    (docs/decomp-plan.md);
  - a patched emulator;
  - the original itself on psx-core. That is a self-test of the harness:
    the original playing its own recordings always passes.

### Phases

About phase 1 and phase 2 of this plan together, mostly on psx's side.

- **(a) One runner, Rust only.** `ProcessPort` in psx, `serve(Game)` in
  psx-scenario, observations read as values by glossary name; `port check`
  and web replay drive Rust ports through the protocol, which replaces the
  `cargo test --test scenarios` route and its report file. Ports on the
  base template need no change: `serve` answers `--psx` inside
  `psx_scenario::replay::main`, which their `main` already calls, so a
  `port update` is enough. The in-repo Croc example's reports stay byte
  for byte the same.
  Done: `psx_scenario::protocol` holds `serve` and `Process`/`ProcessPort`
  (one child for a whole check, a fresh game per launch, a child that dies
  or takes over two minutes on a reply started again; panics replied and
  raised again on psx's side, so failures read as before). `port check`
  builds with `cargo build`, starts `cargo run -- --psx` and writes the
  runner's report itself; `$PSX_SCENARIO_REPORT` is gone. The full
  add-on's `Shown` starts its renderer at the first screen asked for, so
  a check draws nothing. The Croc example's report is byte for byte the
  same, in about a second.
- **(b) Any executable.** The manifest; `port check` and web replay running
  its commands; the arithmetic spec and `psx port vectors`; a tiny C port
  (one quantity) in CI as the conformance test.
  Done: `psx-port.toml` names `build` (optional), `run`, `import` and
  `import_test`; psx appends `DISC OUT` or `index DISC ARCHIVE...` to
  `import` rather than filling placeholders, so one command serves both.
  Without the file the commands are cargo's, and the base template seeds
  the file with them. The check reads the glossary's quantities alone; the
  binding is recording's. The guide says nothing of the port's
  language beyond the Rust templates being there to use: topic `setup`
  holds the manifest and the protocol, topic `arithmetic` the integer
  rules, the GTE's pinned points and the libgte-style helpers. `psx port vectors` (and the `port_vectors`
  tool) writes `fixed.jsonl` (about 15,000 cases over eleven formats,
  thirteen pairs and the angle helpers), `gte.jsonl` (every command with
  each `sf` and `lm`, at register level, and the helpers) and the two
  tables. examples/c-port is a sled in C with two quantities, its
  arithmetic tested against the vectors and checked by `port check` in
  psx's tests (crates/psx/tests/c_port.rs); its recording is synthetic.
- **(c) The wording and the key.** The port key without cargo; the guide,
  the tool descriptions and port next's items no longer assume cargo.
  Done: psx web's port key is the git tree of the workspace as it is
  (written from an index of its own, so committed, uncommitted and new
  files count and ignored ones do not), the project's notes left out; the
  replay needs `psx-port.toml` (or a cargo game). The overview says "a
  modern program" and points the template's other languages to topic
  languages; check's and port next's messages lose the `cargo test` hint,
  which the Rust template's AGENTS.md keeps.
- **(d) Move the test ports.** The psx-bevy ports under
  `external_agent_tests` (GT2, Croc and Tomb Raider Chronicles) own their
  `psx-bevy/`, released before phase 1, with its own runner
  (`psx-bevy/src/scenario.rs`); psx cannot reach into it, and (a) removes
  the route it answers. We move them ourselves: their scenario test and
  `main` go through the shared runner (psx-bevy's `AppPort`, behind
  `serve`), with a `--psx` branch in `main`. Done when each one's
  `port check` verdicts are the same as before the move. Done between
  agent runs, not during one.
  Left to the user, who moves them by hand (perhaps off Bevy as well);
  until then they keep their psx-bevy runner and `port check` cannot
  run them.

Start with the contract and the C example alone; templates for other
languages can come later if ports ask for them.

# Master plan

Everything left to do on psx, in one order. It sequences the work on
input and timing, designed on 2026-11-03, with what is left of
[decomp-plan.md](decomp-plan.md).

One commit per step, `cargo test --workspace` passing after each. Each
step says what it changes and the fact that shows it done. When a step is
done, add a line under it (`*Done (date):* …`) with what was measured.

## Where things stand (2026-11-03)

- **Recordings** ([recordings-plan.md](recordings-plan.md)): done,
  through step 25 (analog sticks in recordings).
- **Names** ([decomp-plan.md](decomp-plan.md), phase 1): steps 1 to 4
  done. Names files are read into one model, the build is detected and
  Ghidra set up for it, decompiles apply the names, and every view and
  address argument uses them. `code annotate` is gone.
- **The recorded pad**: `f919bde` let a binding answer an `INPUT`
  question with the game's own input word, which recordings then keep in
  place of the controller's buttons; `91dafa9` made that all or nothing
  and named the source in each recording (`"pad": "game"` or
  `"controller"`). The sticks still come from the controller. Track A
  replaced this, and A5 removed `INPUT` (2026-11-05).
- **Track A without the discs** (2026-11-03): A1 to A4 and A6 are
  built and tested on hand-written test programs (each step's *Done*
  line says what); their real-game checks, A5, A7's decision and the
  proof in A8 need a BIOS and the discs. Recordings are now made
  latched and steady unless `--stock`.
- **[inventory-timeline-plan.md](inventory-timeline-plan.md)**: status
  unknown; some of it is in the code (`code inventory --unpacked`). E3
  asks its author to mark what is done.
- **[PLAN.md](../PLAN.md)**: historical, by its own note.

## The problem

Recordings compare the port with the original tick by tick. The port is
written to a simple model: one tick at a time at a steady rate, its input
sampled once at the start of each tick, and only the features ported so
far running. The original is not:

- **Input arrives by field.** The pad is polled every field, from the
  vblank interrupt, into a buffer; the game reads the buffer at some
  point in its tick. Which poll a tick saw depends on where the read
  falls against the field, and that differs by the game's phase: a race
  and a menu read a field apart. At 30 or 20 Hz a tick spans two or three
  polls, and a game that does not copy its pad once per tick can read two
  different polls in one tick.
- **Ticks stretch.** When the original slows down a tick spans more
  fields, and a game that scales movement by the fields elapsed moves
  further.
- **Every system runs at once**, while a port early on has one feature.

Each shows up in a recording as a difference that is not in the rules
being ported.

## The approach

psx runs the original on its own core, so it **adjusts the original to
the port's model** where they meet the machine, and nowhere else:

| Adjustment | What changes | What it removes |
|---|---|---|
| `latch` | The pad buffer changes only at tick boundaries, to the tick's input | Input timing, all of it |
| `steady` | The CPU and GPU run fast enough that no tick overruns its fields | Slowdown |
| `off` | Named functions return at once | Systems the port does not have yet |

With them the original runs one tick at a time, at a steady rate, with
input that changes only between ticks: the port's model. Recordings then
hold only the game's rules, and the port replays them with nothing but
its normal tick and the recorded input.

## Principles

- **Adjust boundaries, never rules.** An adjustment changes what reaches
  the game from outside (input, time, the hardware), never the game's
  own code or data. `off` and input fixes (next) are the exceptions,
  and recordings say so.
- **Input reaches the game every tick.** A port is told each tick's
  buttons and nothing else, so it never copies the console's input
  quirks. Where the original's own driver delays input for the
  console's sake (GT2's pad library skips its poll while the serial
  port is busy, and a release then lands a tick late), the agent
  replaces that function in the running game so the tick's input
  arrives every tick. psx finds such ticks without knowing the game
  (A9); the guide states the rule, never the fix.
- **Measure.** The same input recorded stock and adjusted, compared:
  what differs is the adjustment's cost for that game, or a rule that
  depends on timing which the port needs to know.
- **Name what was done.** A recording lists the adjustments it was made
  with, and the determinism check runs with them.
- **The stock game is one flag away** (`--stock`), and the disc is never
  modified: adjustments are made while the game runs, so overlays cannot
  undo them.
- **The port never emulates.** What the original's timing decides
  reaches the port as recorded input, not as behaviour it imitates.
- **The port is deterministic too.** Replay compares two programs; both
  must give the same result for the same input every time. The port
  seeds its own random numbers and no rule depends on the order a query
  returns entities.
- **Sony's code is never redistributed.** The PsyQ SDK and decompiled
  versions of it are copyrighted. psx and the template may consult them
  and test against them where the person supplies them, but never ship
  them, as the game decompilation projects that use them do not.

## What exists to build on

- **Pad and serial port**: `psx-core`'s `pad.rs` builds a poll's reply
  (the ID byte `41` or `73`, `5A`, two button bytes, and four stick bytes
  in analog mode); `sio0.rs` is the serial port.
- **Observation**: each step's `StepInfo` (`cpu.rs`) has its data access
  (`mem`: address, value, width, load or store) and, for an I/O load, the
  register loaded (`io_load`).
- **Timing**: the CPU counts cycles and hands them to the bus
  (`Cpu::cycles`); video timing makes fields and vblank from them
  (`video.rs`). The GPU's FIFO holds DMA back (`dma.rs`).
- **Ticks**: the glossary's binding names the tick store that ends each
  tick; `scenario record` (`crates/psx/src/scenario.rs`) ends ticks
  there, keeps each tick's `field`, and can already be driven by tick
  (the determinism check's second route).
- **Names** bind to code by content, so a scenario can name a function
  to turn off and have it found in an overlay wherever it loads. `code
  symbols` identifies the PsyQ library's functions, `VSync` among them.
- **The template's tick**: rules run in `FixedUpdate` in
  `GameSystems::Game` at the game's `TickRate`; `begin_tick`
  (`psx-bevy/src/shell.rs`) samples input into `ControllerState` once per
  tick; replays inject a `TickInput` (`psx-bevy/src/testing.rs`) and run
  exactly one tick per update. Two flaws: replayed input is OR-ed with
  live devices, and `frames.rs` copies the injection (dropping the mouse)
  instead of sharing it with the scenario runner.

## Decisions

- **The pad buffer is found from the library first, by data flow
  always.** A game that uses Sony's pad routines tells psx where its
  buffer is: the buffers it passes to the BIOS's `InitPAD` or the PsyQ
  library's `PadInitDirect`, or the library's own buffer for `PadInit`
  and `PadRead`, at the place the decompiled library
  ([psy-q-decomp](https://github.com/sozud/psy-q-decomp)) shows it,
  for the library version `code symbols` identifies. Data flow, below,
  finds the buffer for every game and confirms the library's answer.
  Where a game has a known GameShark joker code, its address is a third,
  independent check: a joker code is built on the game's pad word.
- **Data flow.** A load from the serial
  port's data register marks its destination register with the reply
  byte it returned (which byte of the exchange). Moves, and the shifts
  and masks a pad library assembles a halfword with, carry the mark;
  other writes clear it; a store of a marked register marks the RAM bytes
  it writes. The bytes marked with the button and stick bytes of the last
  complete poll (a `42` read command answered on port 1) are the buffer.
  This finds the PsyQ library's, the BIOS's (`InitPAD`, `StartPAD`) and a
  game's own handler's buffer alike, with no scripted presses. Memory
  card exchanges (`81`) and config mode (`43` to `4F`) are ignored.
- **Lag frames.** A tick in which no game code reads the pad is a *lag
  frame*, the term TAS tools use for a frame where the game did not poll
  input; its input has no effect. psx reports them by that name, and the
  port gets the released pad on them.
- **The latch.** At each tick boundary psx writes the next tick's input
  into the buffer's button and stick bytes and sets the emulated
  controller to it; until the next boundary it drops stores to those
  bytes made in an exception handler. The pad's ID, its connected status
  and the exchanges that switch analog mode, configure the pad or drive
  its motors are left alone. A game with no buffer (one that reads the
  serial port from its main loop) is latched by setting the controller
  only at boundaries.
- **A latched recording's pad is the input written.** Nothing is
  observed; rows keep their shape and meaning ("the tick's input"), so
  the recording format does not change.
- **Two modes, to start.** Once `latch` and `steady` exist, `scenario
  record`, `scenario play` and `run` use both by default, and `--stock`
  uses neither. `off` is the only adjustment with arguments: the
  scenario's `[off]` table. A recording names what it was made with in
  `producer.details.adjust` (`["latch", "steady"]`, and what was off); no
  field means stock. Choosing adjustments one by one per scenario comes
  when a scenario needs it.
- **States stay stock.** The states a scenario starts from are made
  without adjustments, so they can be shared; adjustments start where the
  recording starts.
- **The measurement is a plain comparison.** `scenario adjust-check ID`
  records the scenario's recordings again, stock and adjusted, driven by
  the same ticks, and reports for each quantity the first tick it
  differs, with both values. The agent judges whether a difference is a
  harmless shift (a press taking effect a tick earlier) or a rule.
- **`INPUT` is retired.** It is used once, as a cross-check while `latch`
  is measured on the race game, then removed from `psx-capture` and psx,
  with the `"pad"` detail it added. A game with no buffer needs no word
  from its binding: `latch` sets the controller at boundaries.

## Track A: input and timing (the critical path)

A1. **Find the pad buffer.** The serial port says which reply byte each
    data-register load returned; a `PadFlow` observer fed each step's
    `StepInfo` keeps the marks, tracks exceptions (entry to `rfe`), and
    reports the buffer: its RAM bytes, and which reply byte each holds.
    For a game whose pad routines `code symbols` identifies, the
    library's buffer (decided above) is reported beside it, and the two
    must agree; a joker code given for the game is checked the same way.

    *Done when* on Croc, GT2 and TRC from a state in play the buffer is
    found within 2 fields and stable across 600, each byte matches the
    poll's reply every field, the library's buffer agrees for each game
    that uses Sony's pad routines, and a test EXE that branches on the
    reply in a register without storing it reports no buffer.

    *Done (2026-11-03), on test programs only:* `psx_core::padflow`
    (with each JOY_DATA read placed in its exchange, and completed polls
    logged, by `Sio0`) and `run --pad-reads`. Hand-written programs: a
    handler storing the button bytes, one assembling them into a halfword
    and inverting it, and a main loop polling into registers (no buffer;
    its serial loads are its reads). Each buffer byte also keeps its mask
    (0 for a copy, 0xFF inverted), which `latch` needs. **Still to do:**
    the real-game check (Croc, GT2, TRC, with a BIOS and the discs), and
    the library cross-check and joker codes (with the PsyQ signatures).

    *Done (2026-11-05), on the discs:* `run --from-state --frames 600
    --pad-reads` with the same ten presses on each (Croc `level_1_1`,
    GT2 `b1_start`, TRC `level-probe`). Each buffer is found from the
    first poll: Croc `0x8007C199` (id) and `0x8007C19A`/`B`
    (buttons); GT2 `0x801C97A8` to `AB` (id, ready, buttons); TRC
    `0x801FEE70` to `73`, with copies at `0x800A6018`/`9`. Write
    watches on the button bytes over the 600 fields: every byte is the
    held buttons, active-low, from the field the press starts (GT2: the
    next poll), never anything else. Croc and TRC poll every field; GT2
    two fields in three (409 polls in 600), and Mednafen from power-on
    with the same replay does the same (411, same pattern), so that is
    the game's own driver, not a lost interrupt. The game reads
    the buffer every second field in Croc and GT2 (301 fields without a
    read), every field in TRC. GT2's race word `0x800A95B4` is not
    reported as a copy, and cannot be: the driver remaps the buttons
    through a 16-entry table one bit at a time (`0x80083A4C`, branch on
    the bit then `or`), into the pressed/held words at `0x800A9590` to
    `9C` that the race reads. That is control flow, not an inversion,
    so carrying the mark through `nor`/`xori` would not help it; `latch`
    still works, because it latches the buffer the remap reads. **Still
    to do:** the library cross-check and joker codes (PsyQ signatures).

A2. **Naming adjustments, and the measurement.** `--stock`,
    `producer.details.adjust`, the determinism check running with the
    recording's adjustments, and `scenario adjust-check`. A recording
    naming an adjustment psx does not know is refused when checked.

    *Done when* a do-nothing test adjustment is named in a recording,
    rerun by `scenario check`, and measures the same on Croc's scenarios,
    and a test adjustment that moves Croc one unit on one tick is
    reported at that tick.

    *Done (2026-11-03), on test programs only:* `psx::adjust`,
    `--stock` on `scenario record` and `scenario play`,
    `producer.details.adjust`, the determinism check, replays and
    continued recordings running with each recording's own, and
    `scenario adjust-check` (MCP `scenario_adjust_check`), which keeps
    its result in `.psx/scenarios/<id>.adjust.json` rather than in the
    recordings, which are committed data. Tests: a do-nothing adjustment,
    and one adding 1 to a word when tick 3 ends, reported at tick 4.
    **Still to do:** "measures the same on Croc's scenarios" (needs the
    disc); `--adjust` on `run` is not built.

    *Done (2026-11-05), on the disc:* `scenario::tests::adjust_check_on_a_game`
    (ignored; `PSX_TEST_WORKSPACE`, `PSX_TEST_SCENARIO`,
    `PSX_TEST_NUDGE=ADDR:QUANTITY`) runs `adjust-check`'s measurement
    with the test adjustments. On all five of Croc's scenarios (42
    recordings, 12 min): `test_nothing` measures the same, and adding 1
    to Croc's x (`0x800D8D8C`, his object's `+0x370`) when tick 3 ends
    is first reported as `croc_x` at tick 4 in every recording.

A3. **`latch`, and its check.** The latch as decided, with recording
    driven by tick. Its risk is a binding whose tick store comes after the
    game's pad read in its loop: the latch then lands a tick late. With
    `latch`, `scenario record` watches the buffer's reads by code outside
    a handler, and refuses a recording in which one falls between the
    tick boundary and the game's wait for the next field (its call to the
    PsyQ library's `VSync`, or where it spins on vblank), naming the
    read's function. What `VSync` waits on, and the library's other
    waits, are taken from the decompiled library, not assumed. A
    recording reports its lag frames.

    *Done when* on Croc and GT2 every read of the buffer during a tick
    sees that tick's input; a test EXE reading the buffer twice across a
    vblank sees the same bytes both times with `latch` and different
    bytes without; and a test binding whose tick store follows the pad
    read is refused with the read's function named.

    *Done (2026-11-03), on test programs only, with one change:* the
    latch as decided (input held to the tick's end; the buffer written
    through each byte's mask and put back after the handler's stores; the
    controller alone where no buffer is found or a byte has no mask).
    On a test game that polls between two reads of its buffer in one
    tick, stock ticks sometimes read two pads and latched ones never do,
    through a copied buffer and an inverted one. Lag frames are reported,
    and so are ticks ending inside an exception handler. **The change:**
    the refusal above is not built. A read between the tick boundary and
    the game's wait belongs to the next tick by the binding's own
    definition, and with `latch` it gets that tick's input, so the check
    would refuse sound bindings (a tick store at the top of the loop).
    The hazards that remain are reported instead: lag frames (a tick
    store on a render loop that outpaces the logic) and ticks ending in a
    handler (a tick store in vblank's). **Still to do:** the real-game
    check on Croc and GT2.

    *Done (2026-11-05), on the discs:* a latched recording now also
    reports `buffer_reads`, the game's loads (outside a handler) of a
    latched byte, and `stale_reads`, those that read anything but the
    tick's latch (absent when none). 600 fields with ten presses, latched
    and steady, in Croc's `ground_1_1` and GT2's `b1_launch` (probe
    recordings, since removed): no lag frames, no tick ending in a
    handler, 2 fields per tick throughout. Croc: 12,225 loads of its
    button bytes in 300 ticks, none stale. GT2's game code never loads
    its buffer (0 reads): its vblank handler remaps it into words that
    OR together every poll since the race last copied them (`0x800838B4`,
    copied by `0x80083998`), and the race packs its pad word
    `0x800A95B4` from that copy. That word, which GT2's recordings keep
    as the pad, follows the latch: a press latched at the end of tick k
    is in tick k+1's word, always (53 of 53 in the 8 `measure_latch`
    recordings). A release is in tick k+1's word too, except when the
    first field of tick k+1 has no poll (the driver skips about 3
    fields in 10): that field's handler still accumulates, with the
    pad it decoded at its last poll, so the release shows in tick
    k+2's (10 of 47 releases; in lsd_test exactly the ticks whose field
    is 3 mod 10). Stock, the same input reaches the word at the same
    tick or one earlier, never later. The skipped polls are the game's
    driver (Mednafen skips the same), not psx's, and a port told only
    each tick's buttons cannot tell when they happen. It should not have
    to: A9 has the game patched so they never happen.

A4. **Replayed input is the only input.** An exclusive mode on
    `InjectedInput` in the template: the tick's pad, sticks and mode are
    the injected ones; live devices and the held-tap memory are ignored.
    The scenario runner and `--frames` share one `apply(&TickInput)`. The
    developer MCP's injected input still adds to live input. The runner
    also replays each recording twice in fresh apps and fails a port
    whose observed values differ between the two runs, naming the first
    tick and quantity: the port's own determinism, checked where it is
    cheap. `AGENTS.md` gains the rules behind it (seeded random numbers,
    no dependence on query order).

    *Done when* a test holding a live key while replaying gets only the
    recording's buttons, both replay drivers use one input path, and a
    test rule that picks an entity by query order is reported as
    nondeterministic.

    *Done (2026-11-03):* `InjectedInput::exclusive`, set by
    `testing::apply`, which `run_input` and `--frames` share (so `--frames`
    now replays the mouse too); the scenario runner replays each recording
    a second time and reports where the two runs part; `AGENTS.md` gains
    the determinism rules. 78 psx-bevy tests and the workspace's pass.

A5. **Measure `latch`, then retire `INPUT`.** `adjust-check` on every
    scenario of Croc and of the game `f919bde` was made for (its race and
    its menu), and each latched tick's pad compared with that game's
    `INPUT` answer. Then remove `INPUT` (the question in `psx-capture`'s
    `abi.rs`, `Glossary::input`, `Plugin::input` and its parsing,
    `scenario record`'s game pad and the `"pad"` detail) from psx and
    the template's vendored copy; that game's binding drops its `input`.

    *Done when* the results are recorded here with any difference
    explained, that game's port passes its recordings made with `latch`
    with no input delay fitted, and nothing named `INPUT` is left.

    *Done (2026-11-05), but for the port's run (A8):* `adjust-check`
    on all of Croc's 42 recordings and GT2's 24, which gained
    `--adjust latch|steady` to measure one alone. With both, every
    recording differs from stock. **Croc:** `steady` alone changes
    nothing (42 of 42 the same), so all its differences are the latch's
    input timing, from the first presses (ticks 3 to 6 in most). **GT2:**
    the latch alone gives the same first differences as both in 6 of 8
    scenarios (b1_brake 61, b1_chase 62, b1_launch 2, b1_steer 61,
    fr_test 25, mt_test 415). In the other two `steady` adds to it:
    b1_course's yaw first differs at tick 171 with both and 241 with the
    latch alone, and lsd_test's values at 41 against 39. That is A3's
    skipped polls again: from lsd_test's state, 300 fields of the same
    input poll 205 times stock and 207 steady, so steady moves which
    fields skip (the card's transfer ends elsewhere against vblank) and
    so which releases land late. A9 removes them. (`steady` alone, with
    stock input, also differs in three GT2 scenarios, from the same
    cause: input crossing a tick boundary when the tick ends earlier in
    its field.) No difference was found that is not input timing, but
    GT2 is a running simulation where a press a tick sooner changes
    every later value, so "a value stock never reaches" cannot be told
    from a shift by comparing values; A8's port runs are the test.
    `INPUT` is gone from psx-capture, psx, the template, the croc
    example and GT2's binding (its measured bits are in GT2's
    `notes/pad-words.md`); question 5 is marked retired, not reused.

A6. **`steady`.** A clock multiplier on the CPU (its cycles count for a
    fraction of the bus's, 4× to start) and the GPU's drawing taking no
    time against the FIFO, so a tick's work fits its fields. Timers, the
    CD, the SPU and video timing keep the real clock. `run` reports
    fields per tick as a histogram. Measured with `adjust-check` as
    `latch` was. Emulators already offer this to players (DuckStation's
    CPU overclock), and it is known to break some games, which is why
    it is measured per game rather than trusted.

    *Done when* a Croc or GT2 stretch that slows down stock runs every
    tick in its normal fields with `steady`, audio and video timing are
    unchanged, and the measurement is recorded here (a game that does
    more per tick when it has time shows there).

    *Done (2026-11-03), on test programs only:* `Cpu::overclock` in
    psx-core (instruction issue and fetch cost a quarter of their clocks;
    loads, multiplies, divides and the GTE keep theirs; off, timing is
    unchanged and psx-core's tests pass), the GPU untimed, and every
    recording's `fields_per_tick`. On a test game whose work takes a
    field and a half before it waits for vblank, every stock tick takes
    two fields and every steady tick one. **Still to do:** the real-game
    check (a stretch of Croc or GT2 that slows down, with audio and video
    timing compared), and `run` reporting fields per tick (it has no
    ticks without a binding).

    *Done (2026-11-05), on the disc:* Croc and GT2 barely slow down.
    Every stock recording in both workspaces is 2 fields a tick, except
    one 3-field tick (fields 587 to 590) in Croc's 1-1 play-throughs
    `play_running` and `play_jumping`. `start_1_1/play_running`
    recorded again from its input: stock reproduces that tick; latched
    and steady, all 706 ticks take 2 fields (29,694 buffer loads, none
    stale, no lag frames). Croc does not do more per tick when it has
    time: stock tick t and steady tick t+1 (the latch's tick) hold the
    same quantities through the slow tick up to tick 321, where stock,
    now a field behind, gets a replayed press a tick later. Timing, with
    `run --adjust steady` against stock over the same 1415 fields and
    input: the same CPU cycles (4,821,932,199 against …201), the same
    28.4358 s of audio, the same sound effects keyed on (same samples
    and counts, each at most a field earlier or later, from where the
    game's work now ends within its tick); only a silent 32-byte sample
    the sound library keys to reset voices comes 8 times more. Neither
    game gives a long slowdown to measure here; a busier stretch (GT2
    with a full grid, a later Croc level) would test it harder.

A7. **Fields elapsed, only if needed.** If A6 leaves ticks that overrun,
    or a game changes its tick rate on purpose (30 Hz in play, 60 Hz in
    menus), the template's `TickInput` gains `fields` and rules read it
    from a `TickTiming` resource (the game's `FIELDS_PER_TICK` live, the
    recording's fields in replay), never from `Res<Time>`. Otherwise
    this step is closed with the reason.

    *Done when* either a template test replays a two-field tick with a
    rule scaling by `fields`, or this file says why no game needs it.

    *Closed (2026-11-05):* no game in use needs it. A6 found one tick in
    all of Croc's and GT2's stock recordings that took 3 fields, and
    `steady` runs it in 2; neither game's ticks change rate on purpose
    where a port is compared (both glossaries' ticks are the play loop's;
    menus have no ticks in a recording). A8's re-recordings, all made
    with `steady`, recheck it from their `fields_per_tick`. A game that
    ticks at two rates, or overruns under `steady`, reopens it.

A8. **The proof, and the docs.** Re-record Croc's and the race game's
    scenarios adjusted, run both ports, and record the verdicts before
    and after and the workarounds removed. Update `docs/scenarios.md`,
    the guide overview's Recordings section, `port-design.md` and the
    template's `AGENTS.md` (and a line in `game/AGENTS.md`: check whether
    the original keeps a tap shorter than a tick, which live play does).

    *Done when* the numbers are recorded here and the docs describe
    adjusted recordings as the default.

    *Partly done (2026-11-03):* `docs/scenarios.md` and the guide's run
    topic ("Adjusted recordings") describe them as the default. The
    guide's overview is at its size limit (a test holds each topic under
    16 KiB), so it does not mention them yet: trim it, then add a line.
    `port-design.md` and the tap line in `game/AGENTS.md` are still to
    do, and the proof needs the discs.
    *Since (2026-11-04):* `port-design.md` has the tick as the boundary,
    `game/AGENTS.md` the tap line, and the overview one line (its sticks
    passage shortened to fit); `run --adjust steady` exists (`latch` is
    refused there, needing a recording's ticks). The proof needs the discs.
    *Done (2026-11-05), on the discs:* every tracked recording of both
    workspaces (Croc's 42, GT2's 8) recorded again from its own input,
    latched and steady: no lag frames, no stale buffer reads, and every
    tick 2 fields (6,884 in Croc, 3,718 in GT2). The ports, stock
    recordings → adjusted: **Croc** passes 41 of 42 → 40 of 42.
    `onto_the_rim` fails as before, a tick later (91 → 92, the same
    values: the latch's shift). `play_running` (in `start_1_1` and
    `croc_animations`) is new: without the stock run's one 3-field
    tick, a later press lands a tick sooner and Croc's route parts by
    about 440 units by tick 664; at tick 672 the original climbs
    (`croc_y` −202) where the port stays at −225, ground the old route
    never reached. That is the port's, not the adjustment's. **GT2**
    fails the same 93 (now 94) lines, all before input matters (tick 2,
    tick 60, or the port's own panic in `wheels.rs`), so its
    `INPUT_DELAY` of one tick cannot be judged yet and stays; its
    agent removes it once the port gets far enough to show it, with
    A9's late releases in mind. Neither game needed a workaround
    removed in psx.

A9. **Late input, found and fixed.** The serial port knows each field's
    pad exchanges. A recording (and `adjust-check`) reports the fields
    with no exchange between fields that had one, and the ticks whose
    latched change could not reach the game in the next tick because of
    them: a finding, with no guess at the cause. The guide states the
    rule (input reaches the game every tick; replace the function that
    delays it) and nothing game-specific. The fix is the agent's
    replacement of a game function, through C1's call redirection, and
    recordings list it as they list `off`.

    *Done when* GT2's skipped polls are reported on its recordings
    without psx knowing GT2, a replacement made as an agent would make it
    (in its pad library's interrupt routine, or the race's
    accumulation, keeping the skip that protects the memory card's
    transfer but handing over the latched buttons) has every release
    land in the next tick (47 of 47 in `measure_latch`), and the
    recordings name it.

    *Partly done (2026-11-05), the finding:* a latched recording reports
    `late_input`: the ticks in which a latched change waited a whole
    field with no poll into the buffer and no game code read the buffer
    before the tick ended (a read sees the latch), and the count of those
    fields; `adjust-check` repeats it per recording. It does not look at
    the serial port's exchanges, as planned above, but at what the plan
    cares about: whether the game took the tick's input in. A test
    program that polls every other tick is reported in exactly its
    unpolled ticks, and not at all when it reads its buffer before the
    tick ends. On the discs, from the 8 `measure_latch` inputs re-recorded
    latched and steady: GT2 reports 23 ticks, one field each (b1_course
    1, lsd_test 17, mt_test 5, the other five none). 10 of them are
    releases, exactly A3's 10 of 47 late releases; the other 13 are
    presses, which GT2's handler ORs into its words at the next poll, so
    they reach the race word on time: psx cannot tell that without
    knowing the game, and reports them as the same finding. Croc, 12
    recordings: none. Its own driver skipped a poll once (`play_running`
    tick 463), but its game reads the buffer itself later in that tick
    and sees the latch, so it is not reported. The fix, and the 47 of 47,
    wait for C1's call redirection.

## Track B: the library's rules, exactly

Most of Sony's library is hardware plumbing (graphics, the CD, sound,
the memory card) that the port replaces and never copies. A few of its
functions decide game rules, and a port matching recordings must
compute them exactly: the GTE math library's sine and cosine tables,
`ratan2` and square roots, its fixed-point helpers, and the C library's
`rand`. The template's `psx_bevy::num` and `psx_gte` compute "as the
compiled C does"; this track proves it against the original functions.

B1. **The library's functions, checked.** For each library math function
    a game in use calls (from `code symbols`), a psx test runs the
    original function in psx-core over a sweep of inputs (every angle;
    a spread of values and the edges for the rest) and writes the
    results as a table, once per SDK version, from the person's own
    disc. The template's tests compare `num` and `gte` against those
    tables. Each template function cites its library counterpart, and
    the decompiled source where it helped.

    *Done when* `rsin`, `rcos`, `ratan2`, `SquareRoot0` and `rand` (as
    Croc and GT2 call them) match their tables exactly, or the
    differences are fixed, and the tables are generated from the disc,
    not committed.

## Track C: systems the port does not have yet

C1. **`off`.** A scenario's `[off]` table names functions by their
    `names/` names with a return value (`enemy_update = 0`); when the CPU
    reaches one's entry, psx returns to the caller with it. This is the
    smallest case of the decomp plan's call redirection (D10), built here
    first and shared with it. A name not in the running code is left
    alone, as names are.

    *Done when* a Croc scenario with the enemies' update off records
    Croc's movement with no enemy moving, `adjust-check` reports only the
    enemies' quantities and what touched them, and a misspelt name gets
    the names file's spelling suggestion.

    *Done (2026-11-04), on test programs only:* `[off]` in the scenario
    format (and its schema; the template's vendored copy too), resolved
    by `[MODULE:]name` or hex, refused at prepare with a spelling that
    differs only in case or underscores; the recording keeps each
    function's name, address and value and how often it was called;
    `--stock` keeps `off`. On a test program whose function returns 7,
    the recording holds the value given. Since moved into psx-core
    (`Machine::redirects`), which `code check` and later whole runs share.

    *Done (2026-11-05), on Tomb Raider Chronicles:* Croc cannot take
    it: its objects are bytecode scripts run by one dispatch loop
    (0x8002C790), with no per-kind update to turn off, and returning
    early from an opcode handler leaves the loop spinning. A game that
    runs its objects as scripts needs the interpreter's own switch (a
    script's flag, say), which is D13's code replacement, not a return
    value. On TRC, the Old Mill's two attackers (object 89, items 2
    and 3) have their control routine in their object entry: 0x801D543C
    in the GAMEWAD.OBJ overlay, named `attacker_control` in a scratch
    copy of the port, whose glossary gained the attackers' x, z and
    animation. The port's own route (2400 fields from
    `old-mill-navigation-start`) recorded with `attacker_control = 0`:
    neither attacker moves (stock, both run at Lara from tick 838), it
    was called 726 times, and Lara's quantities and the pad are the
    same tick for tick. `adjust-check --adjust off` (new: `off` alone,
    and each recording's `off_calls` in the report) differs in the six
    attacker quantities only, from tick 837, with `attacker_control`
    called 728 times. `Attacker_Control` and `attackercontrol` are
    refused with `attacker_control?`; `attacker_ctrl` with no
    suggestion. The A9 fix (GT2's pad routine made to poll every field)
    needs code replacement too, not a returned value.

C2. **Turning systems back on.** `port next` gives a Restore item for a
    scenario with something off once the port has the feature it belongs
    to (from the catalogue), and the feature is not done until its
    scenarios pass with nothing off that it owns.

    *Done when* the item appears on scratch Croc once an enemy feature
    exists.

    *Done (2026-11-04), on fixtures:* a function turned off belongs to
    the feature whose entry lists it in `code`; Restore comes once that
    feature matches (its failing recordings first), the Done stage waits
    for it, and `port status` shows each feature's code turned off.

## Track D: the original's code (decomp plan, in parallel)

Independent of tracks A to C except that C1 builds the first piece of
D10's call redirection and B1 runs library functions the way D10 runs
the game's. Details, decisions and "done when" for each are in
[decomp-plan.md](decomp-plan.md), with one addition to D10: besides the
calls taken from recordings, each function is also checked from
randomized starting states (arguments and the memory they point to
varied within their types), to reach branches no recording does, as
[cnc-ddraw's difftest](https://github.com/jake-bliss/cnc-ddraw/pull/3)
does. Library functions the agent's C calls are never rewritten: the C
calls them by name.

| Step | What |
|---|---|
| D5 | What to name next: unnamed callees, `DAT_` globals and stray offsets, by how often they appear |
| D6 | Coverage on the C, and the gaps grouped |
| D7 | The disc's own names: debug files left on the disc |
| D8 | The flow (`port init` runs the scan) and a measure |
| D9 | The header and the build: `names.h` and the linker script. *Done 2026-11-04 on test names; the command comes with D11* |
| D10 | One call, compared, in psx-core. *Done 2026-11-04 (`psx_core::callcheck`); GCC-built C checked against hand-written code end to end* |
| D11 | Checks over recordings: `code check`. *Partly done 2026-11-04: from a state, not yet from recordings* |
| D12 | The decompile shows the C |
| D13 | Replacement for whole runs: `--decomp` |
| D14 | The person sees it: psx web's Code view |
| D15 | Measure it on scratch Croc |

## Track E: housekeeping

E1. **The cloud environment's setup script** installs `libudev-dev` and
    `libasound2-dev`, without which `psx` does not build in a new cloud
    session (gilrs and cpal need them through pkg-config), and
    `gcc-mipsel-linux-gnu` for the agent's C (D9 on; its tests skip
    without it).

E2. **The other agent pulls `psx-web`** and knows: a binding whose
    `INPUT` answers for some ticks and not others is now refused, and A5
    will remove `INPUT` in favour of `latch`. *Since (2026-11-05):*
    `INPUT` is gone; a binding that still has `fn input` no longer
    builds against the template, and drops it.

E3. **The inventory timeline plan's status** is marked step by step by
    whoever knows it, so this plan can place what is left of it.

## Order

1. **E1 and E2**: minutes, and they unblock others.
2. **A1 to A5**: the buffer, the measurement, `latch`, the template's
   replay input, and the proof on the race game. This ends the
   input-timing problem: five steps.
3. **A6 to A8**: `steady`, fields elapsed if needed, the proof and docs.
4. **B1** before a port's rules need exact library math: soon for any
   3D game.
5. **C1 and C2** when ports start needing scenarios with systems off.
   A9 follows C1's call redirection: GT2's late releases until then
   are known and listed.
6. **Track D** throughout, in parallel, by another session or agent.

## If needed later

Each of these was designed and then left out because nothing needs it
yet. The first scenario or game that does brings it in.

- **Recording by read**, for a game `latch` does not suit (A3 refuses
  its tick point, or A5 shows its rules change when latched). Each tick's
  pad is the poll its first read by game code outside a handler saw. A
  tick whose reads saw different polls keeps a note per read site
  (function), which the port can model with a second, later pad for the
  systems that read later (empty in live play); polls skipped between
  reads with different buttons, a tap the game never read, are noted, not
  replayed; a tick with no read replays as released. Rows would gain
  fields, so the recording format would go to version 2.
- **`quick_disc`**: data reads complete in a fixed time (seeks at once,
  no random part), audio and movie sectors keeping real timing, for a
  scenario that crosses a load. DuckStation's read and seek speedups
  are the precedent, with its warnings: faster seeks crash some games
  that expect the drive's timing, so this too is measured per game.
- **`seed`**: the game's random state (a `names/` global, or the PsyQ
  `rand` state) written at the recording's start, for a determinism check
  that fails on random outcomes.
- **Choosing adjustments per scenario**: an `adjust` list in the
  scenario's `.toml`, for a scenario that needs a set other than the
  default.
- **Live tap memory as a setting**, for a game whose original drops taps
  shorter than a tick and where that is felt in play.

## Not in this plan

- Port 2, multitaps and other devices (the mouse, light guns, the
  NeGcon): one buffer per device, found the same way.
- Settling the vblank handler's other work at tick boundaries (timers
  counted per vblank, a sequencer's tempo, animation counters).
- Feeding the original's random numbers to the port as tick input.
- Adjustments for presentation (widescreen, precision, filtering).
- Everything decomp-plan.md lists as outside it.

## Related work

A survey on 2026-11-03 found the pieces of this plan established
separately, and the combination (a native port checked tick by tick
against an adjusted original) not established anywhere:

- **TAS tools** (BizHawk and others) record input per frame and detect
  frames where the game did not poll it (lag frames). They replay into
  the same emulator, so they never map input to a port's ticks.
- **Cheat devices**: GameShark joker codes are built on each game's pad
  word in RAM, found by hand per game; a cross-check for A1.
- **Emulators for players** (DuckStation) overclock the CPU and speed up
  CD reads and seeks, with known breakage; the precedent for `steady`
  and `quick_disc`. Nothing found latches the pad buffer at a game's
  tick boundary.
- **Deterministic game loops** (a fixed timestep, one input snapshot per
  tick, replays compared to catch divergence, as League of Legends
  does): the model the template already follows.
- **Ports checked against the original in an emulator**:
  [Rrecreate](https://github.com/MrMethor/Rrecreate) (an AI agent ports
  DOS games, compared tick by tick with the original in a deterministic
  emulator; early, its comparison not yet built) is the closest project,
  worth comparing notes with on input timing. sf2ww runs an arcade port
  in lockstep with MAME.
- **Checking rewritten functions**: re3 replaced a game's functions one
  by one; the Crash Bandicoot 2 project runs the original in an
  emulator with decompiled functions swapped in (D13); cnc-ddraw's
  difftest compares a function and its C rewrite from the same state,
  with mutant controls and randomized states (D10).
- **Sony's library**: [psy-q-decomp](https://github.com/sozud/psy-q-decomp)
  and PSY-Z are matching decompilations of PsyQ (B1, A1, A3);
  PSn00bSDK is an open SDK written from scratch, which shows the
  hardware but not how Sony's library behaved; lab313ru's signatures
  are what `code symbols` already uses to find library code.

## Open questions

- **Is the tick store always before the pad read?** A3 checks, using
  the game's wait for the next field to tell where a tick's loop ends. A
  game that waits without `VSync` and without a recognisable spin may
  need its binding to name the wait.
- **Does the data flow survive every pad library?** A handler that
  inverts the buttons (`nor`, `xori`) before storing loses the mark; A1
  can carry it through those if a game needs it.
- **How fast is fast enough for `steady`?** Fixed (4×) first; adaptive
  if overruns remain.

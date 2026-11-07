# Plan: probes, the values inside a tick, compared

A port is judged at the tick, on the glossary's quantities: game-terms
state both programs have whatever their shape. That is the right
contract, but a poor microscope. When `car_speed` parts at tick 217, the
fault is somewhere in a long chain of values the tick computed on the
way, and port_check cannot say which.

This plan lets an agent compare those values for a while, by name, on
both sides, without touching the glossary, the recordings or what a
feature is judged on.

## Why

Gran Turismo 2's agent, chasing a steering divergence (b1_steer), added
64 `dbg_*` fields to its glossary: the car's fields by offset
(`dbg_c_f62c`, documented "Diagnostic c_f62c / Unit: raw") and eleven per
wheel (`dbg_w0_load`, `dbg_w0_grip`, ...), and a scenario,
`diagnostic_steer`, comparing 68 of them. It reverted them all without a
commit. Each round of it means:

- adding every value in three places (the glossary, the binding, the
  port's `Observed`);
- rebuilding the binding and recording every scenario again (a glossary
  change invalidates recordings);
- taking it all out again, or committing a glossary in raw units by
  mistake, against the rule that it is in game terms.

It did not avoid checked C: it has nine functions in `decomp/`, the tyre
model's among them. Checked C proves the agent's reading of the
original; it says nothing of where the port's Rust parts from it. Its
`notes/psx-feedback.md` names the missing piece: replay a recording with
its own adjustments while sampling what it likes per tick, "without
temporarily expanding the glossary and re-recording every scenario".
`run` samples per tick but refuses `latch` and `fix`; only recording
honours a recording's adjustments, and recording reads only the glossary.

## The design

Four pieces:

1. **Probes in the binding.** The `Glossary` trait gains an optional
   method, Rust like the rest of the binding, so a probe can follow a
   pointer, walk a list or pick the active car:

   ```rust
   fn probes(&self, m: &Machine, p: &mut Probes) {
       let car = m.u32(0x800A_2F18);
       for w in 0..4 {
           p.probe(format!("wheel_load_{w}"), m.i16(car + 0x6A4 + w * 0x40));
       }
   }
   ```

   Names are free-form strings, outside `Quantities`: not in
   `glossary/src/lib.rs`, so not held to its rules, not in the port's
   `Observed`, not in recordings. Adding or removing a probe rebuilds the
   binding and nothing else. A binding without the method has no probes.

2. **The original, in run.** run's sample takes probe names beside
   glossary names, read where the binding's tick ends, so probes serve an
   investigation before there is a port to compare. And run replays a
   recording as it was recorded (its chain, its adjustments, its pad),
   so what it samples is what the recording saw: latch and fix, which run
   refuses today, come with the recording.

3. **Probes in the port.** The port reports values from wherever it
   computes them:

   ```rust
   psx_scenario::probe!("wheel_load_2", load);
   ```

   On the protocol, a tick's reply carries them when the launch asked for
   probes: `{"probes":{"wheel_load_2":655}}`; otherwise the reply is
   `{}` as now. `probe!` costs one check of a thread-local flag when probes
   are off, so it can stay in the code while a bug is chased. A port in
   another language just writes the object.

4. **The diff.** Probes are matched by name: for each, the first tick the
   two sides differ, with the values a few ticks either side; names only
   one side has are listed (usually a typo). Nothing is written to
   `.psx/verdicts/`, and no stage or port_next item reads it.

A name given more than once in a tick holds a list in order, compared
element by element; a different length is reported as such (a loop that
ran a different number of times).

## Decisions

- **Rust, not addresses in a file.** A TOML list of `ADDR:FMT` reaches the
  easy values only, the same wall the glossary's TOML hit before the
  binding was Rust. The binding already says where the original keeps
  things; probes are more of the same.
- **Values, not functions.** Comparing the port's functions with the
  original's (checked C's captured calls as the port's test vectors) was
  weighed and dropped: it makes the original's call boundaries part of
  what the port is tested on, so the review's restructuring either breaks
  the tests or is refused to keep them. A probe is attached to a value; a
  value survives a refactor and the probe moves with it.
- **Disposable.** Probes are debugging output. Nothing checks them,
  requires them or counts them; the review can ask for any restructuring
  and the porter can delete probes, at no cost. Whether the binding's
  `probes` is committed is the agent's choice.
- **The tick stays the contract.** A port is still judged only by its
  recordings' quantities, in game terms.
- **No new command.** The original's side is run; the diff is an option
  of an existing command (step 5 picks which).

## Steps

### 1. Probes in the binding

`psx_capture::Probes` (named integers and bools, as quantities are
encoded; repeated names kept as lists) and
`Glossary::probes` with an empty default. `export_glossary!` exports it;
psx loads it when present, so bindings built before stay valid.

*Done when* a test binding's probes are read at each tick of a run, and a
binding without them loads as before.

*Done (2026-11-07), the read per tick left to step 2:* `Probes::probe`
takes an integer or a bool (a fixed-point value as its integer; the
PlayStation has no floats), a repeated name making a list.
`abi::PROBES` (6) asks for them; a binding built before answers
`UNKNOWN`, which `Plugin::probes` takes as none, so ABI_VERSION stays 6.

### 2. Probes in run's sample

sample's entries take probe names as they take glossary names, by the
binding's tick (`sample_per: tick`, the tick column as now); a name both
the glossary and the probes give is the glossary's. Values come as the
binding gives them, a repeated name as its list.

*Done when* a run from a state samples a probe and a quantity per tick,
and a probe reading the same memory as the quantity gives the same
values.

*Done (2026-11-07):* a sample name the glossary does not have is a
probe; the report says each column's source, and names the binding
never gave under probes_never_given, with the names it did give.
Checked with a test binding; on a game, in step 7's trial.

### 3. run from a recording

run takes a recording (`scenario/name`): its scenario's chain, then the
recording's pad from its first tick, under the recording's adjustments
(latch, steady, off, and fix with the build the recording kept), for the
recording's length unless fields says less. Without a recording, run
refuses latch and fix as now.

*Done when* sampling a latched, fixed recording's quantities in run gives
the recorded values on every tick.

This is step 2 of docs/run-plan.md, which puts run, recording and the
handoff on one loop first.

*Done (2026-11-07)* as docs/run-plan.md's step 2: run's recording option.

### 4. Probes in the port

`psx_scenario::probe!` and its recorder; `launch` takes `"probes":true`;
`serve` puts each tick's probes in its reply; `Process` collects them. The
protocol section of `docs/scenarios.md` gains the field. The C example
writes one.

*Done when* the Rust and C examples' probes arrive per tick through
`Process`, and a port that never writes any still replies `{}`.

### 5. The diff

run from the recording (step 3) on the original's side, the port
over the same recording on the other, matched by name. The result:
per probe, the first tick it differs and the values around it; the names
one side lacks; the recording's first failing quantity, for reference.
Where it lives is decided here: an option of `scenario_show` (which
already reads a recording per tick) or of `port_check` (which already runs
the port over a recording). Neither writes a verdict.

*Done when* a planted mistake in an example port's intermediate value is
reported at its tick before the quantity it feeds parts.

### 6. The guide

A paragraph in the overview's Fix, general like the rest: the first tick
that differs is where; probes show which value; they are not judged and
come out when done. The binding's probes go where the guide describes the
binding (topic setup); sample's probe names and run from a recording go
in topic run.

*Done when* the guide says it in under 150 words and the overview stays
under its size limit.

### 7. A trial on Gran Turismo 2

Its agent's steer divergence, with probes in place of `dbg_*` fields.

*Done when* what it did with 64 glossary fields it can do without
changing the glossary or recording again, and its feedback file says
whether that was enough.

## Not in this plan

- **Probes at an instruction.** Reading a value when a PC runs (a register
  inside a loop, a scratch value overwritten before the tick ends) reaches
  what tick-end probes cannot. Checked C already sees inside a call; this
  waits until a trial shows tick-end probes fall short.
- **Function vectors.** See Decisions.
- **Resyncing the port to the original each tick**, so a fault does not
  compound and every wrong tick shows, not just the first. It needs the
  port launchable from the original's whole state, which the glossary
  does not hold.

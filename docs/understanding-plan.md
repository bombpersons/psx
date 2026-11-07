# Plan: fixing by understanding

A port is judged on its recordings' quantities, in game terms, and that
stays the contract. This plan changes what the agent is shown around it,
so the work it does is understanding the original's rules, not moving a
number:

1. A failing recording is reported as failing, with the quantities that
   differ and where to look, not as "matches up to tick N" with the
   values it missed by.
2. Features' dependencies come from what their recordings run as well as
   from what the catalogue says.
3. Features that depend on each other are worked together, as a group.
4. The catalogue changes as the agent learns the game: features merge
   and split without losing what the person confirmed.
5. Probes (docs/probes-plan.md) stop at the original's side.

Written 2026-11-07, after the GT2 debugger trial (docs/gt2-debugger-trial.md).

## Why

**A tick that moves is a score.** The GT2 trial's agent described its
progress as moving b1_course/offroad's divergence from tick 229 to 241,
and tick 229's speed error "from 104 to -3". This time the change under it
was a rule found (the port's grip table was fixed to surface 0), but the
report rewards any change that pushes the tick on, understood or not, and
the values invite tuning constants until they match. A recording that
fails means the rule is not yet understood, or the feature needs another
that is not done; neither is a distance to close.

**Probes would make internals a target.** docs/probes-plan.md's port side
and diff compare the port's intermediate values with the original's by
name. An agent shown `wheel_load_2` off by 15 tunes until it matches, and
the port is pushed to keep the original's units, steps and loops to have
something to compare, against the review's question (would one write a
new game like this). The plan dropped comparing functions for that reason
(its Decisions, "Values, not functions"); probes at a PC, which the trial
seemed to call for, would bring it back. The trial's fix came from
reading the original (the surface bytes in the debugger, the checked
`update_tyre_loads.c`), not from a comparison: the hand comparison only
confirmed it.

**Checked C already proves an understanding.** code_check runs the C
against the original's own calls from the recordings, and port_check with
decomp= replays a recording with the C in the original's place
(diverges_with_c). That is a rule written as code and shown right on
that recording: the thing to port.

**Declared dependencies miss the real ones.** b1_course/offroad is
F-0010's (courses), and the port parted from it in F-0007's code (tyres
and surfaces), while the catalogue says F-0007 depends on F-0010. Each
needs the other. `depends` is the agent's guess when it catalogues; the
recordings' coverage shows what each feature's recordings run.

**port next names one feature where work spans several.** Interdependent
features have come up often: "work on F-0010" when the fix is in F-0007,
which waits on F-0010.

**The catalogue is cut once and then held.** catalogue/README.md says a
feature stands alone "when someone could want it without its
neighbours", with "the friction curve" as too fine. GT2's agent cut the
car into gearbox, drive types, differentials, tyres and body: each a
capability a player could name, none recordable without the others.
Taking a feature out of a confirmed goal needs the person to confirm
again, so a merge reads as losing features.

## The design

### Failures: which quantities, and where to look

port_check's result for a failing recording gives the quantities that
differ, not the tick or the values (expected, got). The per-tick values
psx web plots (`RecordingReport::values`) stay in the person's view and
leave the agent's results. The tick is not hidden: the agent can find it
with the debugger or a run; it is not reported, so there is no count to
push.

Beside them, a pointer, worked out by psx from the recording around the
first difference:

- the writers of the differing quantities: the addresses the binding
  reads for each, watched on the recording's replay, by function (as the
  debugger's watch lists writers);
- code the original ran there for the first time in the recording: a
  function first called, a branch way first taken;
- the feature each belongs to (the catalogue's code), and its checked C:
  whether the functions have C whose check holds, share_in_c over that
  stretch, and whether the recording replays with it.

The pointer says what it found as evidence, not a verdict:

- the code is another feature's, one not done: the features depend on
  each other (they join a group, below);
- the code is this feature's (or its group's) and has no checked C: the
  rule is not understood yet;
- checked C covers it and the recording replays with it, and the port
  still differs: the original's rule is known there. The port may not
  follow the C, or what is around it differs: code on the path with no
  C, the data the C reads (GT2's grip table), the port's launch from the
  scenario's parameters, the order the tick calls things in.

The pointer names the recording, functions and features; the agent
starts the debugger itself (debug_start with the recording) and finds
the moment, by a pc stop at a function, a watch on the quantity or the
code.

### Dependencies: declared and seen

A feature's dependencies are its `depends` and the features whose code its
recordings run (each recording's coverage against the catalogue's
`code`). Declared ones still order the work before there are recordings
and make the goal's closure. A dependency seen and not declared is a
catalogue item: add it, or say why the code it reaches is not that
feature's (shared library code, a function catalogued in the wrong
feature).

### Work by group

Features joined in a cycle of dependencies, either kind, form a group.
port next gives a group's items as one: the features, their recordings
together, and port_check over all of them. Groups come in dependency
order. Each feature is still judged and finished on its own recordings;
the group is the unit of work, not of judgement.

### The catalogue changes as the agent learns

catalogue/README.md's rule becomes one the recordings can check: a
feature stands alone when its recordings can show it without its
neighbours. Two features whose recordings each run the other's code are
one of:

- separate features that share code: a recording of one could avoid the
  other's code (a character controller's jump and climb, one state
  machine, each recordable alone); the coupling is how the recordings
  were made;
- one feature: no recording can show one without the other (GT2's car
  handling, likely). Merged, it keeps a recording per part, so the
  checks lose nothing; only finishing the parts apart goes, which those
  never could.

The agent decides which, and port next asks when a group forms ("these
features' recordings run each other's code: merge them, or say why they
stay apart"). The README says refining the catalogue is expected at any
stage.

A merged feature lists the ids it `replaces`; each part of a split names
the id it came `from`. Confirmation follows them, so neither counts as the
goal shrinking. Recordings (work and held_out, the person's plays) and
wont_reach confirmations move with the recordings and code they belong
to; the new features are judged again by port_check.

### Probes: the original's side only

docs/probes-plan.md's steps 4 to 7 (probes in the port, the diff, the
guide's paragraph, the trial) are dropped, its Decisions saying why. The
binding's probes (steps 1 and 2, done) go too, so the port has nothing to
match against: names paths (below) read the original's memory as well.

Names paths become how memory is named wherever psx reads it: sample,
until, watch, debug_show value and debug_go's write and read take
`car_0->mass_scale` and the like, by the names/ types, beside
`ADDR:FMT`.

## Decisions

- **The quantities are the only target.** Everything else psx shows is
  for understanding the original.
- **Reported, not hidden.** The first tick and the values stay findable;
  only the results stop carrying them. Hiding would leak anyway.
- **Evidence, not a verdict.** Checked C on a recording shows the rule is
  known there; it does not show which side of the port is wrong.
- **The agent starts the debugger.** The pointer is a place in code and
  features, not a moment; finding the moment is reading the code.
- **Seen dependencies add to declared ones**; they do not replace them,
  since nothing is seen before the recordings.
- **Groups are computed; merges are the agent's.** A group says what is
  worked together; whether it is one feature is a judgement in game
  terms.

## Steps

### 1. Probes out, names paths in

Drop probes-plan steps 4 to 7 (its Decisions record why); take out
`Glossary::probes`, `abi::PROBES`, sample's probe names and
`probes_never_given`, keeping ABI compatibility with bindings that export
them (psx ignores the export). Names paths in sample, until, watch,
debug_show value and debug_go write/read.

*Done when* `car_0->field` samples, stops and shows as its address and
type would, on Croc and GT2, and a binding built with probes still loads.

**Done** (2026-11-07). `names_bind::NamePath` resolves a typed global's
`.field`, `->field` and `[N]` (a null pointer or one outside memory
leads nowhere); sample, until and debug_show value read it anew each
row, watch, watch_reads, debug_go write/read and bytes where it leads
when they start; a bare name is the glossary's quantity, else a typed
global. On Croc, `croc->x` (a smoke names file, `Obj *` at 0x8007528C)
sampled equal to croc_x, stopped on its store and held in until; on GT2
a struct global at the car sampled as its addresses (mass_scale 5165)
and stopped on the accelerator's store in map_pedals. GT2's own
`Car *car_0` is declared a pointer where the car itself is (the word is
0 all race): its agent's names to correct. Croc's binding rebuilt
against the probes-era psx-capture with a `fn probes` loaded and
sampled.

### 2. Failures without the tick

port_check's and scenario_check's results for the agent give a failing
recording's differing quantities, without first ticks or values; psx web
keeps its plots. The guide's Fix (overview) and topics that speak of the
first tick say what a failure means.

*Done when* no agent-facing result of port_check, scenario_check or
port_next carries a recording's first failing tick or a missed value,
and psx web plots a failure as before.

### 3. The pointer

Per failing recording, from its replay: the binding's reads per
quantity (the addresses each reads, recorded by the machine the binding
reads), their writers near the first difference, code first run there
(coverage and call moments), the features they belong to and their
checked C (with decomp= over that recording: diverges_with_c,
share_in_c). Bounded in time; one recording per feature per check.

*Done when* on GT2 b1_course/offroad at 0180540 the pointer names the
surface and grip functions and F-0007, and after the trial's fix
(404dbb7) the collision response's code and its feature.

### 4. Dependencies seen

Per feature, the features whose code its recordings run; port next's
catalogue item for one not declared; port status shows both kinds.

*Done when* GT2's catalogue shows F-0010's recordings depending on F-0007
and port next asks for it.

### 5. Groups

Cycles of dependencies as groups; port next's items by group, in
dependency order; port_check over a group's recordings; the review and
progress name groups where they name features today.

*Done when* GT2's handling features come as one group, a feature outside
any cycle comes alone as now, and every port next test passes with
groups of one.

### 6. Merging and splitting

`replaces` and `from` in catalogue entries; confirmation through them;
verdicts (.psx/verdicts/<id>/), reviews' feature lists, progress.jsonl
and port next's history read through them; a catalogue item when a group
forms. catalogue/README.md's rule and that refining is expected.

*Done when* merging two confirmed features into one keeps the goal
confirmed, keeps their held_out recordings, and port next judges the new
feature; and a split likewise.

### 7. A trial on Gran Turismo 2

A fresh session from port_next, with the handling features as a group:
whether it merges them, how it uses the pointer, and its feedback file.

*Done when* its feedback says whether the pointer led it to the rule
without the tick, and whether it worked on a number at any point.

## Not in this plan

- **Resyncing the port to the original each tick**, so every wrong tick
  shows: it needs the port launched from the original's whole state, and
  it would report ticks.
- **Checking the port's functions against checked C's vectors**: the
  probes plan's reason holds.

# Plan: fixing by understanding

A port is judged on its recordings' quantities, in game terms, and that
stays the contract. This plan changes what the agent is shown around it,
so the work it does is understanding the original's rules, not moving a
number:

1. A failing recording is reported as failing, with the quantities that
   differ and where to look, not as "matches up to tick N" with the
   values it missed by.
2. A feature is what the player could name and the recordings that
   show it; its code is the agent's current reading, revised freely, and
   every function the goal's recordings run is some feature's or marked
   not ported, so code nobody claimed is work psx can point at.
3. Features have no dependencies: the pointer says which feature's code
   a failure is in, and the work goes there.
4. Features merge and split without losing what the person confirmed,
   and progress is measured in ways that do not change with the cut.
5. Probes (docs/probes-plan.md) stop at the original's side.
6. The agent sees the port's screen beside the original's, to
   understand what drawing does; what it shows that can be checked
   becomes quantities.

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
needs the other. `depends` is the agent's guess when it catalogues.

**What runs is no dependency either.** Taken from the recordings'
coverage instead, GT2's dependencies are everything: each of F-0001 to
F-0011's recordings runs the code of nearly every other (the whole car's
physics runs every tick, whatever the recording does). In a game whose
systems run every frame, running together says nothing about needing.

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

**A feature's code is held as first written.** Adding a function to
`code` brings every branch of it to Cover, or wont_reach entries the
person must confirm; removing one turns its wont_reach entries into
problems, confirmed again. A helper two features share owes its branches
twice if both list it. The README says what `code` is, not that it is
revised, and Locate asks only while it is empty. So `code` stays the
first guess, though the agent understands the game better later: the
pointer at 404dbb7 found the collision response in no feature's code.

**Code no feature lists is invisible.** Locate asks for a feature's code
once; after that, a function the recordings run that no entry lists is
seen only if the pointer happens on it. Library code and the drivers
(CD, sound, the GPU's command lists) are no feature's either, and
nothing says so, so an agent cannot tell the code it set aside from the
code it missed.

**Progress counts features as cut.** Features exist to say how much of
the port is done and to point the agent at what is not. Counting
features done makes the first depend on the catalogue's cut: GT2's car
as five features or one changes the count, not the port.

**The person cannot judge engineering.** What is a feature's code, what
is library code, what depends on what: the person cannot tell whether
the agent has it right, so a confirmation of them only stops the agent
revising. The person can judge the game: what they want, a reason in
game terms why no recording can reach a branch, the held-out plays.

**Nothing shows the agent its port's screen.** psx web draws the port's
frames beside the original's for the person; no result gives them to
the agent, and the guide never says to look. The reviewer's report on
how the port looks as a game is written from the code alone. The
quantities say nothing of the camera, animation or HUD, so a port can
pass every recording and draw the game wrong.

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

- the code is another feature's, one not done: the work is there (port
  next's Fix names that feature beside the failing one);
- the code is this feature's and has no checked C: the
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

### A feature: what the player could name, and its recordings

A feature is a capability in game terms (catalogue/README.md's rule: a
feature stands alone when its recordings can show it without its
neighbours) with the recordings that show it. It is the unit the person
names in the goal, plays held-out recordings for and calls done.
Everything else in an entry is the agent's reading of the original,
revised as it learns, with nothing for the person to confirm.

### Code: claimed by a feature, or not ported

A feature's `code` is the agent's current reading of where its rules
are: a function added when the pointer or a watch shows the feature's
quantities come from it, one taken out when it turns out to be
another's. A function may be several features' (a state machine jump
and climb share); its branches are covered once, by any of their
recordings.

Code that is no feature's is listed in `catalogue/not_ported.toml`, by
function or address range, each with why: library code (memcpy, the
libgte helpers), the CD and sound drivers, the GPU's command building,
debug code. The port has its own of those, or none. It is the agent's
judgement, not confirmed: the pointer says when code listed there
writes a differing quantity ("marked not ported, and it writes
car_speed"), and the person's held-out plays fail where a rule was set
aside.

The goal's code is every function the goal's recordings run (work and
held-out, their coverage against the inventory). Each is claimed (some
feature's code) or not ported; one that is neither is work port next
gives (Locate), those writing a compared quantity (the pointer's
writers) first. A feature is reviewed only once the code its own
recordings run is all claimed or not ported.

Revising costs no more than it must. Code taken out of a feature takes
its wont_reach entries with it, and that is not a change to confirm;
code moved to another feature takes its confirmed entries along. Code
added needs its branches covered (that shows it understood). wont_reach
stays the person's to confirm: its reasons are in game terms.

### No dependencies: the pointer directs

`depends` goes, and nothing takes its place. Order mattered for which
feature to work on first; before recordings, Locate and Record wait on
no other feature, and after, a failing recording's pointer says whose
code its differing quantities come from. port next's Fix names those
features beside the failing one, so "work on F-0010" says the fix is in
F-0007's code. A goal's closure is its features; the agent proposes the
whole list the goal needs and the person confirms it. Existing
workspaces: port update moves each feature only `depends` brought into
the closure into goal.toml's features, so the confirmed list keeps it.

### Merging and splitting

Two features whose recordings each run the other's code are one of:

- separate features that share code: a recording of one could avoid the
  other's code (a character controller's jump and climb, one state
  machine, each recordable alone); the coupling is how the recordings
  were made;
- one feature: no recording can show one without the other (GT2's car
  handling, likely). Merged, it keeps a recording per part, so the
  checks lose nothing; only finishing the parts apart goes, which those
  never could.

The agent decides which, from the pointer and the recordings it can
make. The README says refining the catalogue is expected at any stage.

A merged feature lists the ids it `replaces`; each part of a split names
the id it came `from`. Confirmation follows them, so neither counts as the
goal shrinking. Recordings (work and held_out, the person's plays) and
wont_reach confirmations move with the recordings and code they belong
to; the new features are judged again by port_check.

### The cut, against the goal

The first cut is a guess, made before the agent knows the game; nothing
so far asks it to cut again. Two things do:

- **The reviewer.** The Review brief also gives the goal's description
  and the catalogue, and asks: do the features together cover what the
  description asks for, its breadth included (GT2's "any car on any
  track" against eleven features whose recordings are nearly all the
  Vitz on the licence course); can each feature's recordings show it
  without its neighbours; which should merge or split, and what is
  missing. These are findings like any other, answered by recutting or
  kept with why.
- **Signs the cut is wrong**, found by psx and given to the agent where
  it works, and to the reviewer in the brief: a feature whose failures'
  pointer keeps finding other features' code (GT2's offroad finds
  F-0002, F-0003, F-0004 and F-0007), said in Fix's why as "the cut may
  not hold"; a feature with no recording of its own (each is also
  another's); a function claimed by several features whose recordings
  all run the others' code. They say where to look, not what to do.

The goal has held-out plays of its own, beside the features': once every
feature is reviewed, port next gives a Play item for the goal, and the
agent writes the request from the description, in game terms ("a 4WD car
on a dirt course, then an FR car on a street circuit"). The goal is done
when those pass too. A request narrower than what was asked is something
the person can see without judging code or a cut.

### Progress the cut does not change

Beside the features by stage, psx web's Stats and port status plain
(the person's) show three shares that no cut changes: the goal's
recordings passing (held-out ones once reviewed), the goal's code
claimed or not ported, and the claimed code's branches resolved. The
agent is given items, not the shares: a share is a number to push.

### Screens: for understanding, beside the quantities

The agent can see the original's frames and the port's at the same
ticks of a recording (the port's `--replay` screens, which psx web
already draws), as images it reads, and the guide says to compare them
once a feature draws: in Fix, when a drawing feature's recording
differs, and in the Review brief, before the reviewer writes how the
port looks. A port that does not draw yet says so.

Screens are not compared by psx. What drawing does that can be checked
becomes quantities in the glossary, recorded and compared exactly like
any other: the camera's position and angle, the animation playing and
its frame, the model or sprite shown, the HUD's numbers. Camera,
animation and HUD are features of their own, with their recordings. The
person judges the whole look in psx web, as now.

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
- **No dependencies.** Neither the catalogue's guess nor what runs
  together says what a feature needs; the pointer does, per failure.
- **The engineering is the agent's.** `code`, not_ported, merges and
  splits are its reading of the original, not confirmed: the person
  could not tell if they were wrong. The pointer and the held-out plays
  catch code set aside that holds a rule. The person confirms what they
  can judge: the goal, and wont_reach's reasons in game terms.
- **The cut is revised against the goal.** The reviewer reads the
  catalogue against the description, psx points at signs the cut is
  wrong, and the goal's own held-out plays show its breadth to the
  person; none decides the cut, the agent does.
- **Every function the goal runs is accounted for.** Claimed or not
  ported, so code nobody claimed is work, not a gap no one sees.
- **Screens are looked at, not scored.** A pixel difference would be a
  number to fit and would push the port's renderer toward the PS1's GPU;
  what drawing shows that matters is a quantity.

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

**Done** (2026-11-07). The runner's failures say "differs from the
recording in a, b" (or that the port has no q); each difference's
where and values go to the report's `differences`, which only psx
web's plot reads. port_next's Fix says what a difference means and
where to look, scenario_check's diverges_with_c gives differs_in. Kept
with their tick: a port's panic and its own non-determinism (port
faults, not differences), a replay that differs without the C too (the
capture's fault), and scenario_adjust_check's stock against adjusted.

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

**Done** (2026-11-07). `port/pointer.rs`: per failing feature, its first
work recording that differs is replayed on the original to the end of
the first differing tick. A quantity's bytes are those the binding read
whose change (inverted, or the low bit where that leads a pointer out of
memory) changes its value; writers are the stores into them in the 4
ticks before (else each byte's last writer), first_run the words and
branch ways run first there, by inventory function, each with its
features and C; with_c replays it with the checked C. evidence reads
whose code each shown function is. Kept in the verdict, shown in
port_check's rows and port next's Fix; one replay per recording, 300 s
for all. On GT2 at 0180540, offroad names sum_wheel_speeds (F-0007,
checked, replays with it), update_wheel_surface_samples and
wheel_tyre_force (no feature); at 404dbb7 its first first_run function,
called first, is 0x800340A4, the trial's collision response
(0x80034134), in no feature's code. The whole check took 95 s. Script
code (Croc's) shows as the interpreter's functions.

### 4. Dependencies out

`depends` no longer read (the README, the guide and port add drop it;
an entry that still has it loads); no dependency cycle problem; the
closure is the goal's features, in id order; port confirm and port
status without depends and required_by; port update moves features only
`depends` brought in into goal.toml; port next's Fix names the features
whose code the pointer finds beside the failing one.

*Done when* GT2's catalogue, after port update, keeps its closure and
confirmation, has no dependency problems, and its offroad Fix at 0180540
names F-0007.

**Done** (2026-11-07). `depends` still loads and nothing reads it: no
dependency or cycle problems, the closure is the goal's features by id,
port confirm, port status and psx web drop depends, required_by and
in_goal. port update lists in goal.toml the features `depends` brought
in, keeping the confirmation (`goal_features_added`). Fix's why adds
"the pointer finds code of F-x there" for other features' code it names;
the evidence says the rule may be there, not that they depend on each
other. On GT2 (whose goal already lists F-0001 to F-0011) port update
adds nothing and the confirmation covers; offroad's pointer at 0180540
names F-0002, F-0003, F-0007 and F-0004, sum_wheel_speeds listed by
three features: step 5's shared code.

### 5. Code claimed or not ported

`catalogue/not_ported.toml` (function or range = why); the goal's code
(what its recordings run) each claimed or not ported, else port next's
Locate for it, the pointer's writers first; reviewed waits on its own
recordings' code being accounted for; the pointer's evidence for not
ported code that writes a differing quantity; a function several
features list covered once; wont_reach entries leaving with code taken
out without a confirmation and following code moved; catalogue/README.md
and the guide say `code` is revised as the agent learns.

*Done when* on GT2 at 404dbb7 port next asks where 0x800340A4 belongs, a
range in not_ported answers for library code, and moving a function
with a confirmed wont_reach entry to another feature, or taking it out,
asks nothing of the person.

**Done** (2026-11-07). catalogue/not_ported.toml (entry or range = why;
a key that is neither, no reason, or an entry no function starts at is a
problem). Each feature's recordings' functions in no entry's code and not
listed are its `unclaimed` (port status); reviewed and finished wait on
none, and port next's Locate, after Answer for a matching feature, lists
them by name, the pointer's first. A wont_reach confirmation is the
key and reason under any feature, so it follows moved code; code taken
out takes its entries (the problem says so); another feature's
wont_reach answers by address or range for a function both list. The
pointer's evidence names listed code on the path ("listed in
catalogue/not_ported.toml … the rule may be there"). On GT2 at 404dbb7
about 370 functions are unclaimed per feature, 0x800340A4 among F-0001,
F-0006, F-0007, F-0008 and F-0010's, 2 s for port status; a range and an
entry in not_ported take theirs out. At 0180540, update_wheel_surface_samples
listed there shows in offroad's evidence with its reason (wheel_tyre_force
was not on the path at this check). Moving a confirmed wont_reach is
tested, not tried on GT2 (it has none). The guide's overview trimmed to
stay under 16 KiB.

### 6. Merging, splitting and progress

`replaces` and `from` in catalogue entries; confirmation through them;
verdicts (.psx/verdicts/<id>/), reviews' feature lists, progress.jsonl
and port next's history read through them. The three shares in psx
web's Stats and port status plain. The Review brief's questions on the
cut against the goal's description, with the signs of a wrong cut (the
pointer finding other features' code, a feature with no recording of
its own, code claimed by several features that run each other's); Fix's
why carries the first. The goal's held_out in goal.toml, port next's
Play item for it once every feature is reviewed, and Done waiting on it.

*Done when* merging two confirmed features into one keeps the goal
confirmed, keeps their held_out recordings, and port next judges the new
feature; a split likewise; the shares are the same before and after a
merge; on GT2's workspace the Review brief lists its signs (offroad's
pointer, features sharing recordings); and a goal with every feature
reviewed gets a Play item for the goal.

### 7. Screens

An argument on an existing command (the tool budget is full) giving the
original's and the port's frames at chosen ticks of a recording, or a
few spread over it, as PNGs for read; the port's come from its
`--replay` screens, as psx web's do, and a port that does not draw says
so. The guide's Fix and the Review brief say to compare them once a
feature draws; catalogue/README.md and the glossary's guidance say what
drawing shows becomes quantities (camera, animation, HUD).

*Done when* on the Croc example a recording's frames come back as both
games' screens at the same ticks, a port that does not draw says so, and
the Review brief asks the reviewer to look.

### 8. A trial on Gran Turismo 2

A fresh session from port_next, on the handling features, on the
simulation disc as the workspace is (the person moves the goal to the
arcade disc afterwards, which suits "any car on any track" better):
whether it merges them, revises their code and not_ported, how it uses
the pointer and the review's findings on the cut, and its feedback file.

*Done when* its feedback says whether the pointer led it to the rule
without the tick, whether it worked on a number at any point,
whether it changed a feature's code when it learnt more, and whether it
recut the features against the goal.

## Not in this plan

- **Resyncing the port to the original each tick**, so every wrong tick
  shows: it needs the port launched from the original's whole state, and
  it would report ticks.
- **Checking the port's functions against checked C's vectors**: the
  probes plan's reason holds.

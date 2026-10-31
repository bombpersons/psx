# Design: coverage of scripted rules

Built in psx (step 23 of [recordings-plan.md](recordings-plan.md)); not
yet tried on a game. It extends [port-design.md](port-design.md)'s
coverage (section 5) to games whose rules live in a script run by an
interpreter, so a feature made of script code can be located, covered and
counted done the way one made of machine code is.

## 1. The problem

Coverage today is machine code: each recording keeps which conditional
branches the original ran and which way (`<name>.coverage.json`), and a
feature's `code` names the functions whose branches must all be resolved.
That is enough when the rules are code. In Croc they are not:

- Croc's controller is a bytecode program run by an object interpreter
  (dispatch at 0x8002CFF8, opcode table 0x8006B424). The program decides
  when to jump, when a press becomes a stomp (`y - floor > 983`), the
  stomp's sub-states and timers; the opcodes it calls (MOVE, TURN, JUMP)
  do the arithmetic.
- The interpreter's `if` opcode is one native branch shared by every
  `if` in every object's program. Coverage sees that branch taken both
  ways somewhere and calls it covered; which script `if` went which way
  is lost.
- So F-0003 (the stomp) lists three opcode handlers as its code and
  reached "reviewed" without coverage ever seeing its rules; a recording
  that never pressed CROSS below the stomp height would not have been
  missed. Locate had the same blind spot: `coverage diff` between a run
  that stomps and one that does not shows the same interpreter code in
  both.

Games use every kind of scripting: bytecode with jump offsets, stack
machines, tree-walking evaluators, state tables, compiled event lists.
psx cannot know each format, and the agent should not have to teach it
one.

## 2. The idea: the script's bytes as coverage

What every interpreter has in common is that it reads its program. A
script `if` whose else-branch never ran is a stretch of program bytes the
CPU never loaded from. So the one thing psx needs is **where the program
is**; it then keeps, per recording, which bytes of it the game read, as
it keeps which code words it ran. Unread stretches are the script's
uncovered blocks, in the same way an unexecuted basic block is code's.

That needs no knowledge of the format. The agent, who has to find the
program anyway to read the rules, says where it sits; psx counts reads.

Reads assume the program is bytes in one place, and they give blocks,
not branch ways. For the rest (section 4) the binding runs its own code
at points in the interpreter it chooses and tells psx the script position
and the way each decision went. However the interpreter works, the
binding's code is the part that knows; psx only counts.

## 3. Declaring programs: the binding

The glossary's binding already knows the PlayStation (where the tick ends,
where each quantity is). It gains one optional method naming the programs
running, read when each tick ends, as quantities are:

```rust
impl Glossary for Game {
    // ...
    /// Croc's controller: a bytecode program. The object's +0x04 holds its
    /// base; the length is the word before it (0x8002C790, notes/player-controller.md).
    fn programs(&self, p: &mut Programs, m: &Machine) {
        let croc = m.u32(0x800D_8A1C + 0x04);
        p.program("croc", croc, m.u32(croc - 4));
    }
}
```

- A program is a name, an address and a length. Its name is the stable
  key: coverage and the catalogue speak in `croc+0x1803`, never in RAM
  addresses, so a program loaded elsewhere in another scenario still has
  the same offsets.
- psx keeps each program's sha256 with the coverage (as it keeps the
  sha256 of the code run), so a recording whose `croc` holds different
  bytes is not mixed with the others; no program byte is kept, so the
  coverage is still committed.
- Read every tick, so a program that is loaded or moves mid-recording is
  followed. A tick's reads count against the programs as the tick before
  declared them.

## 4. What psx records

`scenario record` already sees every instruction the original runs (for
the machine code's coverage). It also keeps, under `programs` in the
recording's `<name>.coverage.json`:

1. **Program reads** (needs only `programs`): the bytes of each program
   any load read. Per program: name, length, sha256, and the stretches
   read as `[start, end)` offsets.
2. **Sites and ways** (optional): code in the binding, run where the
   binding asks, that says which script position is running and which way
   a script decision went. psx stores what it is told, keyed by
   `(program, position)`; how the interpreter works stays in the binding.

   Interpreters differ too much for a fixed declaration ("the load that
   reads each opcode") to fit them all:

   - one dispatch loop reading an opcode (Croc);
   - threaded code, where every handler ends with its own fetch, so there
     are dozens of dispatch points;
   - a helper such as `read_u16(&pc)` that reads opcodes and operands
     alike, so no one load means "an opcode";
   - tree walkers, whose program is nodes scattered on a heap, linked by
     pointers, with no contiguous bytes and no opcode at all;
   - state tables, where a row is the position, and event lists, where a
     time is;
   - the script position held only in a register, or in a field of the
     current object, with a handler interrupting a script and resuming it
     (Croc's event handlers);
   - several interpreters in one game (objects, cutscenes, AI).

   So the binding registers hooks, as `tick` registers the store that ends
   a tick, and runs ordinary Rust in them with the CPU's registers as well
   as RAM:

   ```rust
   fn hooks(&self, h: &mut Hooks) {
       h.at(0x8002_CFF8)                  // Croc's dispatch (illustrative)
           .branch(0x8002_8190);          // the `if` handler's deciding branch (illustrative)
   }

   fn hook(&self, at: Hook, m: &Machine, s: &mut Script) {
       // The object running (in s0 here, illustrative), its script position
       // at +0x00 and its program's base at +0x04 (0x8002C790).
       let obj = m.reg(S0);
       let position = m.u32(obj) - m.u32(obj + 0x04);
       match at {
           Hook::At(_) => s.site("croc", position),
           Hook::Branch { taken, .. } => s.way("croc", position, taken as u32, 2),
       }
   }
   ```

   - `at(pc)` runs once the instruction at `pc` has run; `branch(pc)`
     runs once a conditional branch at `pc` has gone, after its delay
     slot, with the way it went. Registers are as they are then: a delay
     slot that moves the script position on has already moved it.
   - `c.site(program, position)` marks a position run. `c.way(program,
     position, way, of)` counts one way of a decision at that position;
     `of` is how many ways it has, so a `switch` with five cases is one
     decision with five ways, and a never-taken case is a gap.
   - The binding computes the position each time from what the hook sees,
     with no state between hooks, so interrupts, nesting and several
     objects running the same program need nothing special.
   - A position is whatever the binding can name stably across runs: an
     offset into a program it declared, a node's offset in the disc file
     it was loaded from, a table's row. psx compares positions, never
     interprets them.
   - Hooks only run at the pcs registered, so the cost is a call per
     dispatched opcode, not per instruction.

   Sites say what ran, for `coverage diff` (Locate) and for the agent's
   own reading; psx cannot know the sites that never ran, so they are not
   gaps. The gaps are unread stretches and decision ways never taken. A
   program can use reads alone, hooks alone (a tree walker with nothing to
   read as bytes), or both.

   Finding the hook points is the agent's work with the tools psx already
   has: coverage of the interpreter, the hottest code reading the program,
   the branches inside its conditional handlers. psx still decodes
   nothing.

## 5. The catalogue, status and next

A feature's `code` names script stretches beside functions:

```toml
code = ["0x800286F4", "0x80028734", "croc+0x1803..0x18A0", "croc+0x2732..0x2B82"]

[wont_reach]
"croc+0x19D2..0x1A77" = "surface 4 bounce: the fixed JUMP 2300 off a type 4 floor"
"croc+0x1890" = "decision: never false on a flat floor (wall contact)"
```

- A stretch must lie in a program a recording of the feature declared,
  as an address must be a function of the inventory.
- **Cover** counts, within the feature's stretches: each stretch read or
  never read (when the binding declared where the program is) and each
  decision with the ways its recordings took (with hooks).
  Each gap is a `wont_reach` key in the same `name+offset` form,
  confirmed by the person as today.
- **Locate**: `code coverage diff` diffs program reads (or sites) as it
  diffs code, so a stomp run against a jump-only run from the same state
  gives `croc+0x2732..0x2B82` directly, the stretch the stomp's rules are
  in. That is the bigger win for Croc: the stomp could have been located
  by psx instead of by hand-decoding the program.
- **Status** shows a script gap as `croc+0x18A4..0x18C0 never read`, or
  `croc+0x1803 if: never taken`. The agent maps it to a rule with its own
  decoder (Croc's `notes/player-controller-script/dis.py`); psx never
  needs one.
- The guide's code topic gains a short section: when the rules are
  interpreted, find the program and declare it in the binding; hooks
  when the read gaps are too coarse or the program is not bytes in one
  place. Nothing specific to a format or a game.

## 6. Croc, under this design

- `programs`: one line, as above (the agent had already found obj+0x04 and
  the length word). Hooks: the dispatch and the `if` and `switch`
  handlers' deciding branches, as in section 4 (the registers there are
  illustrative; the agent reads them from the dispatch's code).
- F-0002 and F-0003 name their script stretches: the jump/stomp sub at
  `croc+0x1803`, the stomp state at `croc+0x2732`, the walk-off snap at
  `croc+0x1923`. Their gaps would have asked for, among others, a CROSS
  press below 983 while falling (the buffered jump) and a stomp ending
  over a drop (stomp again, `L104 = 1`), both recordable on the flat floor.
- Things the flat floor cannot give (surface 4 bounce, damage, the other
  `G0` states) become `wont_reach` stretches with reasons, as MOVE's
  analog branches were.

## 7. Limits and noise

- **Reads that are not execution.** A loader copying the program in, a
  checksum or a scan for labels reads every byte. Recordings start from a
  state after the level loaded, so a copy at load time does not count;
  anything that reads the whole program each tick shows up as no gap at
  all, which is the sign to rely on decisions from hooks instead.
- **Padding and tables.** Alignment bytes and unused table entries are
  never read, so they are gaps, given `wont_reach` as padding. Decisions
  have no such noise.
- **Blocks, not ways.** With reads only, an `if` whose skipped arm is
  empty, or whose other arm is reached by another path, looks covered:
  the classic gap between block and branch coverage. Ways from hooks
  close it.
- **Programs that are not bytes in one place.** A tree on the heap, a
  program streamed from the disc a piece at a time, one built at run
  time: reads cannot cover them, hooks can, as long as the binding can
  give each position a name that stays the same across runs.
- **Hooks the binding gets wrong.** A position computed from the wrong
  register counts the wrong site, and coverage trusts it. The check is
  the one the binding's quantities already get: a hook that names
  positions outside the feature's stretches, or the same position for
  every dispatch, shows up at once in `code coverage`. psx can also
  refuse a program whose sites, with reads declared too, fall on bytes no
  load read.

## 8. Alternatives considered

- **psx decodes the script.** The agent writes a full decoder psx loads,
  giving every instruction and branch of the program, so psx knows the
  ways that exist even where no run went near them. Exact, but it means
  decoding the whole format before any coverage, where hooks need only
  where the position is and which branches decide. Hooks are already
  code in the binding, so a decoder is a binding that goes further, not a
  different mechanism; nothing stops an agent writing one.
- **Fixed declarations** (the first draft): the binding names "the load
  that reads each opcode" and "the branches that decide", and psx takes
  the position from what the load read. It fits one dispatch loop and
  nothing else (threaded code, helpers reading operands, tree walkers,
  positions in registers).
- **Split every native branch by script position**: every branch inside
  the interpreter counted per script position. That multiplies native
  noise (each of MOVE's branches once per script line that calls MOVE);
  hooks split only the branches the binding names.
- **Coverage of the port instead.** Replay the recordings under
  `cargo llvm-cov` and require every branch of the port's code be taken.
  This is format-free and cheap, and it does show port rules no
  recording tests. But it cannot show rules of the original the port
  never implemented, which is what original-side coverage is for. It is
  worth having beside this design, not instead of it (a separate step).
- **Coverage by quantities.** Count a feature covered when its recordings
  span the values involved (heights, speeds, states). Says nothing about
  which rule a value went through.

## 9. Work

Built:

1. `scenario record` keeps each program's reads, and calls the binding at
   its hooks (after an instruction, after a branch has gone) with the
   machine as it is; tested on a toy interpreter.
2. psx-capture: `Programs`, `Hooks`, `Hook` and `Script`, and the optional
   `programs`, `hooks` and `hook` methods (boundary version 5).
3. `programs` in a recording's kept coverage; `code coverage` and
   `coverage diff` report program stretches, sites and ways for
   recordings (traced runs from `run` keep none).
4. port: `code` and `wont_reach` take `name+offset` positions and
   stretches; Cover and status count them as section 5 says.
5. The guide (code topic, overview's catalogue lines) and port-design.md.

Next: try it on Croc's workspace: declare `croc`, add the script
stretches to F-0002 and F-0003, and see what Cover asks for. Then the
example.

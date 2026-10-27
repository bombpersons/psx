# Plan: a function is what ran inside its calls

The inventory on the timeline (docs/inventory-timeline-plan.md, piece 2)
takes a function's start from the trace and its body from its bytes: from
the start forward to its last return. That reading is a guess, and guesses
about one function's body can cut another's. GT2's boot run still shows
two cases:
- 0x80015FF8, a state machine that saves where it got to and resumes there
  later by `jr s0` and branches. A guess carried from another moment by
  matching bytes splits it, and 26 words that ran are in no image.
- 0x80047EA0 has two extents in two moments.

Every rule tried so far inferred where functions end from bytes or from
how the CPU reached a word, and each broke on some case. No rule about
bytes can be right for hand-written assembly: tails shared by several
routines, several entry points, code falling into the next routine. The
CPU has no notion of a function. It runs words, jumps, branches and saves
a return address on a call.

So the trace records what it can see: which call each word ran inside.
The inventory builds a function's body from that, and reads bytes only for
code that never ran.

## How other tools do it

- Ghidra, IDA and Binary Ninja all make a function an entry and a set of
  blocks, not a range.
- On shared code they differ. Ghidra allows no overlap: each address is in
  at most one function, and its Shared Return Calls analysis makes a tail
  jumped into from several functions its own function, the jumps tail
  calls. IDA gives a shared tail one owner and further parents. Binary
  Ninja lets functions overlap.
- All of them read boundaries from bytes, by heuristics. Nucleus, a
  compiler-agnostic detector in Binary Ninja, falls from 97% to 33% F1
  on Chromium x86 with random padding. iCi, which finds calls at run time,
  still needs a heuristic for tail calls made by a jump.

psx takes Ghidra's model: each word is in one function. The difference is
the evidence. Which calls each word ran inside is recorded, not read from
bytes; bytes are read only for code that never ran.

## The design

### The trace records each word's call

The trace keeps a stack of the calls running (piece 2 uses it for the
moments a call ran in). When a word runs, the call on top is the call it
ran inside: its owner. Each call target in calls.json gains `body`: the
words run while a call to it was on top, as ranges per copy
(`[copy, first, end]`). Code runs in long straight stretches, so ranges
stay few.

Three things need the stack to be right:
- **Exceptions.** An interrupt or exception is a frame of its own: its
  owner is the exception vector (0x80000080), entered by the CPU, a fact
  like a call. `rfe` (in `jr k0`'s delay slot) ends it. Without this the
  handler would be owned by whatever it interrupted. Callbacks the handler
  calls are calls inside it, as now (GT2's 0x8007EA78 and 0x8007EA98,
  called from the kernel at 0x1C18).
- **Returns the stack does not hold.** A `jr ra` to no address on the
  stack (a thread switch, a game swapping stacks, longjmp to a frame
  dropped from the stack) puts an unknown frame on top. Words run under
  it have no owner. A call made under it is known again; a return that
  the stack does hold, below it, drops it.
- **The stack's depth.** At most 1024 frames are kept (`MAX_RETURNS`). A
  call that pushes the oldest out leaves it unknown when the stack
  unwinds to it.

Words with no owner are counted in the trace report
(`trace.unowned_words`), with the frames lost to returns the stack did not
hold.

The trace also notes, for each word entered by a jump (`j`, or `jr` other
than a return), whether `sp` was back to its value at the owning call:
the frame torn down, as before a compiled tail call. That is a calling
convention, not a CPU fact. It only labels guesses ("looks like a tail
call"); it never decides one.

### A function is an entry and its chunks

A function is its entry and one or more chunks, each a range of words.
In one image each word is in at most one function. A function's sha256 is
over its chunks' bytes in order. A function with one chunk keeps today's
row: entry, end, blocks, sha256. One with more lists `chunks`.

### Bodies from what ran

At each moment, for each call running or run then:
1. Its words that ran in this moment's copies, inside this image, are its
   body. These are facts.
2. Words that ran inside calls to more than one target are shared: each
   stretch of them is its own function, entered where the CPU jumped into
   it (its words that do not continue the code before them). The calls
   that reached it are its callers, by jump. GT2's 0x8007EAAC, run inside
   calls to 0x8007EA78 and 0x8007EA98, is one.
3. Code a call reached by a jump that ran inside no other call stays its
   chunk, labelled entered by a jump (and frame torn down, if so): a
   candidate entry. A call landing there later makes it a function; it is
   then shared by step 2. 0x80015FF8's resume points stay its own.
4. A chunk never crosses into another image. Code in another image (the
   BIOS shell, another overlay) entered by a jump is a function of that
   image, entered by a jump. The BIOS's A0/B0/C0 functions, reached by
   `jr t2` from game stubs, are BIOS functions, not chunks of the stubs.

What did not run is read from bytes, as now, and is a guess:
- the blocks of a function its runs skipped, reached by its branches;
- its tail after the last word that ran, to its return;
- functions no run reached: calls read from bytes in live functions,
  pointers, prologues.
A guess never takes a word that ran. A guessed start on one (a `jal` read
from bytes landing inside code that ran) is kept as a candidate entry of
the function that owns it.

Words that ran with no owner (an unknown frame) are found as now: where a
stretch starts, a guess.

### What goes

- Guesses carried between moments by matching bytes (`same_code`), for
  code that ran. Kept for code that did not: it names the same bytes.
- Seeding a function where a stretch of executed code starts, and the
  `fresh` set that limits it, except for words with no owner.
- The protection by continuing words: no guess takes a word that ran.
  Continuing words stay in the trace; the extent reading of unrun code
  still uses them.

### Coverage and the ledger

A word is in one function, found through its chunk, so coverage stays a
lookup: per image, chunks sorted by address. executed_outside_functions
keeps its meaning. A function's blocks and totals cover all its chunks.
The ledger's units are functions as before; a spec routine's address is
matched to the function whose chunk holds it.

## Pieces, in order

Each is committed and reviewed on its own, with its check.

1. **Trace: owners.** Owners per word as `body` ranges in calls.json,
   exception frames, unknown frames, `trace.unowned_words`, the
   torn-down frame mark. Check: tests for a call and its body, a call
   inside a call, a tail jump, a tail shared by two calls, an interrupt
   in a call, a return the stack does not hold, a stack deeper than it
   keeps. On GT2's boot run, measure before going on: words with no
   owner, words with more than one, frames lost, calls.json's size.
   Stop and decide if words with no owner are more than a few.
2. **Inventory: bodies from what ran.** Chunks, shared code split out,
   candidate entries, the image rule, guesses only on what did not run;
   the parts above removed. Check: GT2's boot run gives 0 words in no
   image, 0 executed outside functions, no function with two extents;
   0x80015FF8 whole, 0x8007EAAC its own function, the 9 BIOS call stubs
   and 5 pointer stubs still found. The function list is compared with
   piece 2's and every difference explained.
3. **Coverage and ledger by chunk.** Check: coverage's totals on GT2 match
   the inventory's; the ledger's tests with a function of two chunks.
4. **Guides.** run (calls.json's body), protocol (what a function is,
   shared code, candidate entries).
5. **Ghidra.** `ghidra_annotate` gives Ghidra each function's chunks as its
   body and marks candidate entries, so the workbench shows what the
   trace proved.

Then the second game, as for the timeline.

## Open questions

- How often the stack loses its place in GT2 and in a game that uses the
  BIOS's threads (ChangeTh). If often, the trace may need a stack per
  thread, switched where the BIOS switches them.
- Whether a handler that leaves without `rfe` (one that restarts the
  game, say) needs more than the unknown frame.
- How large calls.json grows. It is 10.6 MB pretty-printed on GT2's boot
  run already; bodies may need writing compactly, or to a binary file
  beside executed.bin.

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
ran inside: its owner. owners.json lists each owner with its `body`: the
words run while it was on top, as ranges per copy (`[copy, first, end]`).
Code runs in long straight stretches, so ranges stay few.

Three things need the stack to be right:
- **Exceptions.** An interrupt or exception is a frame of its own: its
  owner is the exception vector (0x80000080), entered by the CPU, a fact
  like a call. `rfe` (in `jr k0`'s delay slot) ends it. Without this the
  handler would be owned by whatever it interrupted. Callbacks the handler
  calls are calls inside it, as now (GT2's 0x8007EA78 and 0x8007EA98,
  called from the kernel at 0x1C18).
- **Returns the stack does not hold.** A `jr ra` to no address on the
  stack (longjmp, a thread switch, a game swapping stacks) puts a
  landing on top, owned by where it landed: no call the stack knows.
  A call made under it is known again; a return that the stack does
  hold, below it, drops it; another landing replaces it. The trace
  keeps what the run says of where it landed, R: the owners R ran
  inside after a call two words before returned there through the
  stack (in the landing's copy of the page, or the nearest before it),
  `sp` then, and `sp` at each landing. Whose code resumed at R is the
  inventory's guess from that, and facts override it.
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
   stretch of them is split where a call's entry is; a piece starting at
   one is that call's, any other is its own function, entered at its
   first word (the word before it is not in it, so each call that ran it
   came in there or by a jump; branches are not recorded as jumps, so
   which word a jump entered cannot pick the entry). The calls that
   reached it are its callers, by jump. GT2's 0x8007EAAC, run inside
   calls to 0x8007EA78 and 0x8007EA98, is one.
3. Code a call reached by a jump that ran inside no other call stays its
   chunk, labelled entered by a jump (and frame torn down, if so): a
   candidate entry. A call landing there later makes it a function; it is
   then shared by step 2. 0x80015FF8's resume points stay its own.
4. A chunk never crosses into another image. Code in another image (the
   BIOS shell, another overlay) entered by a jump is a function of that
   image, entered by a jump. The BIOS's A0/B0/C0 functions, reached by
   `jr t2` from game stubs, are BIOS functions, not chunks of the stubs.

A call's body is built in a moment only when the moment holds every word
it ran with the bytes it ran: the same code loaded again at the same
place (a later copy with the same bytes, after other code was there) is
the same code, so what ran in either load is the body in both, and a load
nothing ran in gets it too. A call with any of its words changed is not
built there.

What did not run is read from bytes, as now, and is a guess:
- the code between words that ran, read straight through as an extent
  is (a switch's cases are reached by a table, not by branches);
- the blocks of a function its runs skipped, reached by its branches;
- its tail after the last word that ran, to its return;
- functions no run reached: calls read from bytes in live functions,
  pointers, prologues.
A guess never takes a word that ran, and never starts on or takes a word
the moment holds as it ran inside a call that is not built there (the
call is found where it is whole). A guessed start on a word that ran in
a body (a `jal` read from bytes landing inside code that ran) is kept as
a candidate entry of it; one on a word the body did not run starts a
function there, and the body is read up to it. Between a body's own words
that ran, words that ran inside another call are the body's: this code
ran on both sides of them (a `nop` of other code once loaded there).

Words run under a landing whose R ran inside one owner X after returns
through the stack are guessed to be X's (`setjmp` returns twice, as
compilers know it: the code after it is its caller's). A call landing
there, or the words also running inside a call the stack held, override
that. `sp` does not decide it: a game may resume on another stack.
Words under any other landing are found as now: where a stretch starts,
a guess.

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

1. **Trace: owners.** Owners per word as `body` ranges in owners.json,
   exception frames, landings with what the run says of them, the
   torn-down frame mark. Check: tests for a call and its body, a call
   inside a call, a tail jump, a tail shared by two calls, an interrupt
   in a call, a return the stack does not hold, a stack deeper than it
   keeps. On GT2's boot run, measure before going on: words with no
   owner, words with more than one, frames lost, calls.json's size.
   Stop and decide if words with no owner are more than a few.
   Done. GT2's boot run: 602,844 words run inside calls, 1,807 inside
   exceptions, 229 before any call (the BIOS's reset code); 1,711 of
   509,604 inside more than one owner (mostly the BIOS's A0/B0/C0
   dispatchers, jumped to by the game's stubs); no frame dropped, no
   exception left elsewhere. 30,471 returns the stack did not hold put
   23,241 words under landings, nearly all PsyQ's interrupt dispatcher:
   it calls `setjmp` (A0:13h) and gives the buffer to the BIOS as its
   exit from exceptions (B0:19h); each interrupt, the BIOS's handler
   loads `ra`, `sp` and the saved registers from it and `jr ra`s back
   after the call. All 23,241 have one owner R ran inside after a
   return through the stack; 20 resumed with the same `sp` (the
   dispatcher sets the buffer's `sp` to its own stack). owners.json is
   1.9 MB; calls.json is unchanged.
2. **Inventory: bodies from what ran.** Chunks, shared code split out,
   candidate entries, the image rule, guesses only on what did not run;
   the parts above removed. Check: GT2's boot run gives 0 words in no
   image, 0 executed outside functions, no function with two extents;
   0x80015FF8 whole, 0x8007EAAC its own function, the 9 BIOS call stubs
   and 5 pointer stubs still found. The function list is compared with
   piece 2's and every difference explained.
   Done. A traced run's end is one more moment: its bodies are the calls
   whose words RAM holds as they ran, as in every moment (carrying
   bodies found in moments into it, as before, fed its own guesses back
   in as facts). Bodies are carried only into a run without a trace.
   GT2's boot run: 1749 functions (1096 executable, 653 in 44 overlay
   images) and 692 in the BIOS shell; 0 words in no image, 0 executed
   outside functions; 7 functions of more than one chunk, 5 of shared
   code, 3 with words run under a landing, 4 with candidate entries.
   0x80015FF8 is one body (0x80015FF8 to 0x80016270), 0x8007EAAC is its
   own, shared by 0x8007EA78 and 0x8007EA98, and every executable
   function piece 2 found is still found. Two entries have two extents,
   both other code at the same entry: 0x80016F88 at frame 3021, when the
   overlay's next page was not loaded yet (other code from frame 1761
   holds 0x80017000), and 0x80030D64 in the BIOS shell, read to the end
   of its page as in piece 2.
   Against piece 2 (1092 executable, 696 overlay rows, 722 in the shell):
   - The executable gains 4 shared code functions (0x80011B08,
     0x80011B28, 0x80068364, 0x8007C32C) and 5 bodies change with them;
     0x8007C310's ends where it ran: 0x8007C318 is the next rung of a
     ladder of entries (`b; li a0, n`) no call reached, now in no
     function.
   - 50 functions piece 2 guessed (carried by matching bytes, or seeded
     where executed code started) are inside a body of what ran: 28
     start on a word that ran inside that body's call, 3 on a word that
     ran inside another call, 19 in code the body did not run. Of those,
     4 BIOS shell stubs (0x8005A9A0, 0x8005A9B0, 0x8005A9D0, 0x8005A9E0)
     came from a call piece 2 read in a half-loaded moment: at frame 3661
     the overlay's 0x800319A8 was read on into the next page, which still
     held the shell, and the shell's `jal`s there were followed. Without
     that call they are read as the tail of the stub before (after its
     `jr t2`, as piece 2's extent reads too).
   - The other differences in overlays and the shell are guesses moved by
     the bodies beside them: bodies end where they ran or where their
     bytes say, not where a guess carried from another moment started,
     so the guesses after them start, end or are carried differently.
   - 44 overlay images, not 47: piece 2's images at frames 5597, 5640
     and 5721 held only second extents of functions now found once (code
     loaded again, 0x80015FF8 among them), or a guess now in a body. An
     image is named by the first function found in it, so four more are
     named differently.
   Coverage of functions of more than one chunk waits for piece 3.
3. **Coverage and ledger by chunk.** Check: coverage's totals on GT2 match
   the inventory's; the ledger's tests with a function of two chunks.
4. **Guides.** run (calls.json's body), protocol (what a function is,
   shared code, candidate entries).
5. **Ghidra.** `ghidra_annotate` gives Ghidra each function's chunks as its
   body and marks candidate entries, so the workbench shows what the
   trace proved.

Then the second game, as for the timeline.

## Open questions

- How often the stack loses its place in GT2 and in a game that switches
  threads: the BIOS's (ChangeTh: a syscall whose handler leaves by `rfe`
  to the new thread's saved pc) or its own (a routine called by `jal`
  that swaps `sp` and `ra` and returns to the other context). If often,
  the trace parks stacks instead of dropping them, knowing nothing of
  the BIOS:
  - An exception that leaves to a pc other than the one it was entered
    from, or a return to an address the stack does not hold, parks the
    call stack under where it would resume: the pc the exception was
    entered from (the word after a syscall) and `sp` then, or the return
    address on top of the stack and `sp` at that call.
  - An exception return or `jr` that lands on a parked (address, `sp`)
    swaps that stack back in.
  - No match, or more than one, is an unknown frame, as now. `sp` is in
    the key because threads waiting in the same routine resume at the
    same address but each has its own stack; it only matches, it never
    decides a body. Parked stacks are capped; the trace report counts
    resumes and misses.
  Ghidra has nothing for this: its analysis is static, and its debugger
  takes threads from the OS, which a PlayStation does not have.
- Whether a handler that leaves without `rfe` (one that restarts the
  game, say) needs more than the unknown frame.
- How large calls.json grows. It is 10.6 MB pretty-printed on GT2's boot
  run already; bodies may need writing compactly, or to a binary file
  beside executed.bin.

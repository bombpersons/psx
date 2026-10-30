# Plan: the inventory as a timeline of code

The inventory lists every function a game has; coverage says which of them
the corpus ran. Both have to be right for any PlayStation game, including
ones that load code over other code and ones whose code is compressed on
the disc. This plan replaces how they tell one piece of code from another.
Everything here is general to PlayStation games. Proven on Gran Turismo 2
(compressed executable, overlays loaded over each other) and one game
without compression before it is called done.

## Why

An executed word is known today by its address and instruction only, not
by which code was in RAM when it ran. A common word (`jr ra; nop`, a stack
prologue) matches several pieces of code, so each part of the inventory
and coverage guesses which one it was:
- snapshots are RAM at a run's end, so code replaced before then is
  searched for in disc files (lost code, placements);
- code no file matches comes from code pages laid over each other
  (`code_as_it_ran`), whose functions are filtered to those holding lost
  code (`only`), with the executable check skipped;
- images at one place that differ only in data are variants, or note
  same_range_as;
- coverage credits a word to an image only for a stretch of four words,
  the whole run of executed words, or whole blocks, or where the words
  beside it are other images' (`beside`);
- a traced call target is matched to an image by any instruction run at
  its address, so code loaded over a function's start is split where the
  old entry falls in it.

Each rule closed the gap the one before left, and each is a guess the
trace could have answered. GT2's boot run shows it: 71 words executed
outside functions, 69 of them loop tails cut off by calls into boot code
seeding functions in the code later loaded there (212 false functions);
the other two a `jr ra; nop` stub the inventory gives to the BIOS shell
(which holds the same words there) and coverage gives to the game.

## The design

### The trace keeps each code page's history

RAM is 4 KiB pages. The first time code in a page runs, the trace copies
the page and numbers the copy. Every CPU or DMA store to a page marks it
dirty; when code next runs in a dirty page, the trace compares it with its
last copy: changed, a new copy; not, the mark is cleared. A page nothing
writes after its code runs costs nothing more.

- `code.bin` holds each copy: id, page, the frame it was taken. A page's
  first copy is whole, a later one the words changed since the copy before
  it (most changes are a few variables kept beside code).
- `executed.bin` holds (copy, address, count): each executed word against
  the copy it ran from. The instruction is the copy's.
- `calls.json` holds each call as (caller's copy, site) → (target's copy,
  target).

At any frame, the copy each page held is RAM's code then. That is the
timeline.

### The inventory reads the timeline

At each moment the timeline changes, the inventory finds functions as it
does now (calls, executed code, prologues, pointers), seeded only from that
moment's copies. A function crossing into the next page reads the copy
that page held at the same moment.

A function is its start address and the sha256 of its bytes. The same
function in many moments, runs or snapshots is listed once; other code
loaded at the same address is another function. That rule replaces
variants and same_range_as.

A snapshot without a trace (inventory `--ram` on an untraced run) is one
more moment: RAM at its end, with no executed words.

### Naming is separate

Once found, each function and its neighbours are named by matching their
bytes against the executables, the disc's files, unpacked files (below)
and the BIOS ROM (the shell). Code nothing matches is still listed, named
by where and when it ran: "code at 80030000, frame 3661". Matching affects
names only, never what is found or counted.

### Coverage is a lookup

Each executed (copy, address) is in one function's bytes: coverage counts
it there. The stretch, blocks and beside rules and the tie-breaks go.
executed_outside_functions stays: it now means a function boundary is
wrong, and nothing else.

### What stays a guess

Function boundaries (static analysis, now checked by what ran), and names
for code no file matches. The words that ran inside each call (owners.json)
make the body of code that ran a fact too.

## Compressed code

A game that stores code compressed has an unpacker that expands it into
RAM. The trace sees the code as it ran, so the timeline finds and counts
it without unpacking anything. Two things still need the files unpacked:
names, and the functions no run reached. An overlay the corpus never
loaded is in no code page, and completeness counts every function.

The decompressor is the game's own (each game packs differently): the
investigator writes it in the project's tools/, and the port needs it
anyway, since a packed file is a format that must round-trip. psx takes
what it produces.

- **Finding the unpacker** is a recipe in guide topic protocol with tools
  psx has: `run --watch` on a code page gives the pc that wrote it just
  before it ran; `run --trace-cd` with `loads` gives which file's sectors
  were read into the buffer it reads from. Check against the code pages:
  an unpacked file must equal the code that ran, byte for byte. If the
  recipe needs a missing step (for example, which buffer a writer read
  from), that step is added to psx.
- **Unpacked files**: protocol.toml `[disc] unpacked = "DIR"`, a folder of
  files named for the disc file they unpack (`RACE.OVL`, or
  `ARCHIVE.BIN#3` for an archive entry). The inventory matches against
  them for names and analyses each one whole, so functions no run reached
  are listed to classify or record.
- **Files that need it**: the ledger lists disc files that look packed
  (high entropy, matching no code that ran) and have no unpacked file, as
  investigator work: the code there cannot be read yet.

## Pieces, in order

Each is committed and reviewed on its own; each has its check.

1. **Trace: page history.** Dirty marking on CPU and DMA stores, copies
   on change, the new `code.bin`, `executed.bin` and `calls.json`.
   Check: a test where code is loaded over code in one page and both run;
   on GT2's boot run, every executed word is in exactly one copy, and the
   copies' count against today's 160 pages (the cap is set from it).
   Done: 1919 copies of 127 pages in 850 KB (today's 160 pages: 658 KB);
   every executed word in its copy, none without one. Until piece 2 the
   inventory reduces the copies to the pages kept before, and gives the
   same inventory.
2. **Inventory on the timeline.** Moments, functions by address and
   hash, naming after. Old runs are refused with "record again", not read
   another way: keeping both keeps the layers. Lost code, placements,
   `code_as_it_ran`, `only`, variants and same_range_as are removed, and
   so is the reduction of copies to pages (`as_ran`). Copies whose changes
   all fall outside the moment's functions give the same functions.
   Check: GT2's boot run gives 0 words in no image, and its function list
   is compared with today's and every difference explained (212 false
   functions from calls into replaced code are expected to go).
   Done, with what the trace proves kept apart from what is guessed:
   - Facts: a call landing (trace_call), the executable's entry, and
     a word the trace saw continue the code before it: run straight on
     from the word before (outside a jump's delay slot; after a
     conditional branch's, taken or not, the CPU is at the word after
     the slot or the target), or returned to by a call two words
     before. No function starts on such a word.
   - Each call records the moments it ran in, from the call until it
     returned (the trace's return stack). A moment's live functions are
     those calls; calls and tail jumps are followed only from them, so
     code still in RAM after it stopped running seeds nothing.
   - Guesses: everything else (calls and tail jumps read from bytes,
     pointers, prologues, where a stretch of executed code starts, and
     the code a function did not run). A guess may split any function,
     traced or not, except on a word the trace saw continue. Protecting
     a traced function's whole extent instead lost 9 BIOS call stubs
     the executable calls with `jal`: they lay in a traced stub's
     unrun tail.
   - The run's end is one more moment, live with the calls running then.
   GT2's boot run (6600 frames): 1795 functions (1092 executable, 703 in
   47 overlay images), and 722 in the BIOS shell, now listed; 0 executed
   outside functions; 932 exercised. Against the inventory before (1849:
   1120 executable, 729 in 12 overlay images): the executable lost 28
   functions and none was added. 24 were calls into other code loaded
   over the executable (the word run there is not the executable's),
   4 guesses now inside traced functions; 13 functions end elsewhere as
   a result. Overlay differences are not all explained yet.
   Open: 2 stretches (26 words) of 0x80015FF8, a state machine that
   resumes itself by `jr s0` and branches, are in no image: a guess
   carried by matching bytes from another moment splits it there, and
   what ran past the split is left out. One function has two extents in
   two moments (0x80047EA0). Both come from reading a function's body
   from bytes; recording which call each executed word ran inside would
   make the body of what ran a fact.
3. **Coverage by lookup.** Credit by (copy, address). Check: GT2's boot
   run gives 0 executed_outside_functions, or each one is a boundary
   error found and fixed in the analysis.
4. **Protocol.** The `corpus` item asks for runs recorded before piece 1
   again (it already does for runs without code.bin); guide topics
   protocol and run say what the files hold.
5. **Unpacked files.** `[disc] unpacked`, matched and analysed whole;
   the ledger's list of packed-looking files. Check: a hand-made packed
   test file; then GT2, once its decompressor exists.
6. **Finding the unpacker.** The recipe, tried on GT2 by an investigator
   session without help; what it lacked becomes psx work.

Then the second game: its corpus re-recorded, inventory and coverage
compared with what they gave before.

## Open questions

- How many copies a long run makes when any store can dirty a page.
  GT2's boot: 1919 in 6600 frames, 1716 of them three pages whose only
  changes are 1 to 4 variables beside code; kept as changed words, the
  cap is 64 MiB of copies. Check a long playthrough.
- Stale instruction cache: a word run from the cache that RAM no longer
  holds is credited to the latest earlier copy holding it
  (ran_from_cache in the trace report). GT2's boot: none.
- The BIOS shell is copied to RAM at power-on: it becomes copies like any
  other code, named from the ROM. Check bios_code's numbers match.

Samples, until and poke read memory as ADDR:FMT. FMT is u8 i8 x8 u16 i16
x16 u32 i32 x32, fxI.F (fx16.16, fx4.12), bits of a word, a byte string, or
a width 1/2/4.

A flag inside a word: u16[14] reads bit 14 of a 16-bit word as 0 or 1,
u8[4..7] bits 4 to 7 as a number. A poke writes the bits alone and keeps the
word's others; until compares the bits' value.

A name, a filename, any text: strN reads N bytes as one cell. Its cell is
text: printable ASCII as itself, any other byte as \xNN (as are \ , " @ &
and a space at either end), trailing zero bytes left off. Bytes after a
terminator are kept (car\x00\x12). A poke writes all N bytes (zeros after
the text); until takes = and != only.

The game's tick. A game's logic runs at its own rate: often one tick every
one or two fields, with extra ticks to catch up after a slow picture. The
glossary's binding names the store that ends a tick (docs/scenarios.md), and
scenario recordings are taken once per tick.

Finding the tick, for any game:
1. run find_tick over a few hundred frames of play (from a state in a level,
   with input). It counts every CPU store to main RAM by PC and address and
   groups the pairs stored in exactly the same fields, as many times; the
   report's tick_finder lists the groups with the most storing PCs first
   (tick_finder.json has every member). A game's logic stores its variables
   once a tick, so the first group whose fields_by_stores is not "1 in every
   field" is usually the logic's, and its fields_by_stores is the tick's
   rhythm ({"0": 300, "1": 300}: a tick every other field). A double-buffered
   game also shows a pair of groups each stored every fourth field: drawing.
2. Pick a store made once per pass of the game's loop. A counter (or step) is
   best; otherwise find the loop: those called once per pass have it as
   caller (trace's calls.json gives callers). Take a store on the path every
   pass takes (where the loop's branches rejoin), as the objects' update
   starts. Confirm it is in the group: reading the code alone misleads, a
   branch can skip a store.
3. Bind it as the glossary binding's tick (t.store(address, bytes).by(pc)).
   The tick store must be in main RAM.

Waiting on a state. until ADDR:FMT=VALUE stops the run at the first vblank
where the value holds (also != < <= > >=), so a scripted route waits for
the game rather than a fixed field, which an emulator update or a slow load
can move. VALUE is written in the format (3, -2, 0x80, 1.5 for fx); several
must all hold. Termination is until_met; the report's until gives run_frame
and the values. The run's files and state.bin (save_state) are from that
vblank, so a route is a chain: run until the menu is up, then from_state
with the press at frame 0, held as Driving menus (below) says. A condition
already true at the start stops at run_frame 0; frames caps the wait, and
until.met false means it never came.

## Finding things in RAM

- The player's object: search 32-bit values over a range for its position
  while it moves one way (ram_search width=4 with between and increased),
  then narrow with more snapshots. A narrow field (a 16-bit heading) gives
  thousands of candidates; a wide one moving steadily gives a few. The
  object's other fields are near it: ram_view the struct across snapshots.
- The tick: run find_tick (above).
- Input: the console's pad reaches the game through its own input word.
  Hold one button and diff against holding none for the word; a write
  watch on it gives the one routine that writes it, and xrefs on it the
  readers, which are where the game's behaviour starts: xrefs bits=MASK
  with one button's bit lists the code that tests that button. A bit no
  single button sets is made by the writer (a modifier held with another):
  read the writer.
- Arrays of objects: one element found, the others are a stride apart;
  xrefs on the first element's address often finds the table's base.
- An object at another address in each level (or each time it is made):
  find it in two places, then search for a word holding its address in
  both (ram_search exact in each snapshot; the address may be held without
  its 80 prefix). The glossary's binding reads it through that pointer.
## Driving menus

A game reads the pad once a tick, not once a field, and a menu usually acts
on a press's edge: held this tick, not held the tick before. A press shorter
than a tick can fall between two reads and never count, and two presses with
no release between them read as one. So hold each press for two ticks and
release for two before the next: 4 fields held and 4 released suit a game
ticking every field or every other (a menu may tick at another rate than
play). A game that ignores a press held that long is
not taking input yet (a fade, a load): wait on a state, not more fields.

The menu's cursor is a variable. Find it as any other: snapshots before and
after one press of DOWN, ram_search for a value that rose by one (narrow with
a second press, and UP to see it fall), with the menu's open flag or screen
if there is one. until ADDR:u8=3 then waits on
the item rather than counting presses, and a route reads which item it is on
without a screenshot after every step.

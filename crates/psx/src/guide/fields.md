A field map names the game's memory once, in samples.toml in the project
root (or the file run's map gives). Samples then take names, and samples.csv
and the report carry names only, so a recording can be shared without the
game's addresses. Every PlayStation game needs its own map; build it as you
find things.

  [tick]                 # optional: a store here ends one tick (below)
  write = "80087AA4:4"   # ADDR[:WIDTH], width in bytes, default 4
  pc = "8001DD64"        # optional: count stores from this PC only

  [globals]              # NAME = "ADDR:FMT"
  camera_x = "8008B258:i32"

  [structs.object]       # a layout: FIELD = "OFFSET:FMT", offsets in hex
  x = "34:i32"
  yaw = "42:i16"

  [subjects.player]      # one struct at a fixed address
  struct = "object"
  at = "8017C87C"

  [subjects.placement]   # an array of them, by index
  struct = "object"
  at = "8017C61C"
  stride = "4C"
  count = 256            # optional bound on the index

  [subjects.lara]        # where a pointer leads, read anew at each sample
  struct = "object"
  pointer = "8008C6B8"   # holds the subject's address
  offset = "0"           # optional, hex, added to it; stride and count work too

FMT is a sample format (u8 i16 x32 fx16.16 ...) or a width 1/2/4. Fields keep
the map's order, and several may share an offset.

A flag inside a word is a field of its own: u16[14] reads bit 14 of a 16-bit
word as 0 or 1, u8[4..7] bits 4 to 7 as a number. Name the flags rather than
the word (or beside it), so a case and its spec say active is 1, not that a
byte holds 64: where the original keeps the flag is its layout, which a port
need not copy. A poke writes the bits alone and keeps the word's others;
until compares the bits' value.

  [structs.object]
  flags = "28:x16"
  active = "28:u16[14]"

sample then takes:
- camera_x, a global, as the column camera_x;
- player, every field, as player.x, player.yaw, ...;
- player.x, one field;
- placement[3] or placement[3].yaw, an element of an array.
Bare hex (8008B258) and ADDR:FMT[:NAME] still sample an address directly.

The game's tick. A game's logic runs at its own rate: often one tick every
one or two fields, with extra ticks to catch up after a slow picture. A
recording means little without knowing which fields a tick ran in. With
[tick] in the map, samples.csv gains a tick column after frame: the ticks
completed since the run start, at each row's vblank. The ticks between rows
r-1 and r ran during field r-1, and read the pad as held in that field.
The report's samples.tick gives the total and fields_by_ticks_run (how many
fields ran 0, 1, 2 ... ticks): steady play is one count, catch-up shows more.

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
   best; otherwise find the loop: profile lists functions by calls, and those
   called once per pass have it as caller (trace's calls.json gives callers).
   Take a store on the path every pass takes (where the loop's branches
   rejoin, or the head of a function called there), as the objects' update
   starts. Confirm it is in the group: reading the code alone misleads, a
   branch can skip a store.
3. Put it in the map as [tick] (write = its write, pc = its pc) and sample the
   subject with it. samples.tick.fields_by_ticks_run should match the group;
   catch-up shows as fields running 2 ticks over a slow scene.
4. samples.tick.changed_without_tick lists columns that changed on a row whose
   tick did not. Empty is exact. Rows there mean the column is updated outside
   the tick, or the marker is at the wrong point of the loop (a pass that
   crosses a vblank puts the update and the marker in different fields): move
   the marker to the point closest after the subject's update starts, and
   note any rows that remain in the case.
The tick counter must be in main RAM.

Waiting on a state. until NAME=VALUE stops the run at the first vblank
where the value holds (also != < <= > >=), so a scripted route waits for
the game rather than a fixed field, which an emulator update or a slow load
can move. NAME is one column (a global, SUBJECT.FIELD, or ADDR:FMT) and
VALUE is written in its format (3, -2, 0x80, 1.5 for fx); several must all
hold. Termination is until_met; the report's until gives run_frame and the
values. The run's files and state.bin (save_state) are from that vblank, so
a route is a chain: run until the menu is up, then from_state with the
press at frame 0, held as Driving menus (below) says. A condition
already true at the start stops at run_frame 0; frames caps the wait, and
until.met false means it never came.

## Finding things in RAM

- The player's object: search 32-bit values over a range for its position
  while it moves one way (ram_search width=4 with between and increased),
  then narrow with more snapshots. A narrow field (a 16-bit heading) gives
  thousands of candidates; a wide one moving steadily gives a few. The
  object's other fields are near it: ram_view the struct across snapshots,
  then name it in the field map.
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
  its 80 prefix), and name it with pointer rather than at, so one map
  serves the whole game. A pointer read as null or outside memory leaves
  the cell empty and an until condition on it false; a case compares an
  empty cell as absent, so a port's run matches it with an empty cell.
- A map right for one level only (its objects at fixed addresses): say
  where it is right, so another level's memory is not read under its
  names. [map] holds lists conditions in until's syntax, e.g.
  holds = ["level=1"] with level a global; a sample row where one fails
  is counted in samples.map_not_holding, and case record refuses a start
  state where they fail and warns of later rows.

## Driving menus

A game reads the pad once a tick, not once a field, and a menu usually acts
on a press's edge: held this tick, not held the tick before. A press shorter
than a tick can fall between two reads and never count, and two presses with
no release between them read as one. So hold each press for two ticks and
release for two before the next: with the tick in the map, a run's
samples.tick gives the fields a tick takes (a menu may tick at another rate
than play); without it, 4 fields held and 4 released suit a game ticking
every field or every other. A game that ignores a press held that long is
not taking input yet (a fade, a load): wait on a state, not more fields.

The menu's cursor is a variable. Find it as any other: snapshots before and
after one press of DOWN, ram_search for a value that rose by one (narrow with
a second press, and UP to see it fall), then name it in the field map, with
the menu's open flag or screen if there is one. until cursor=3 then waits on
the item rather than counting presses, and a route reads which item it is on
without a screenshot after every step.

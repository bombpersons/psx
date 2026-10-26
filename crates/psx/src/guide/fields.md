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

FMT is a sample format (u8 i16 x32 fx16.16 ...) or a width 1/2/4. Fields keep
the map's order. sample then takes:
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
press at frame 0, held long enough for the game to take it. A condition
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
  readers, which are where the game's behaviour starts.
- Arrays of objects: one element found, the others are a stride apart;
  xrefs on the first element's address often finds the table's base.

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
1. Find a value the game logic changes every tick (a timer or a position
   while moving), with ram_search over a few runs.
2. watch it for a few hundred frames. The PC that stores it is in the logic;
   follow it up the callers (xrefs, ghidra) to the loop that runs the logic
   one or more times per frame.
3. In that loop, find a store made once per pass: a pass counter, or a
   field accumulator the loop subtracts from. That address, and the PC if
   other code writes it too, is [tick].
4. Check it: run with the map and sample the value from step 1. It should
   change only on rows where tick changed, and over a slow scene some fields
   should run two ticks (fields_by_ticks_run).
The tick counter must be in main RAM.

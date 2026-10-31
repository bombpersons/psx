# Held-out plays to ask for

port_next's next item is Play (person) for F-0001, then F-0002 and F-0003.
Each play starts at scenario start_1_1 (Croc at the start of 1-1) and
becomes a held_out recording of its feature. The port has only a flat floor
so far (y = -225), so each play must keep Croc where the original's floor is
flat too.

## Where the floor is flat

The first round of plays (removed in a037ee9) went off it.
Reconstructing every recording's floor height under Croc (croc_y while
croc_on_ground, all 17 recordings) gives a flat strip about 1,500 units
wide, x ≈ 11,550-13,050. It runs along the stone path from about z ≈ 9,600
to z ≈ 16,400. Croc starts at x 12,288, z 12,288, facing +z, so:

- **Ahead** (+z, up the stone path, the crystals on it): flat for about
  4,100 units, roughly 4½ s of running.
- **Behind** (-z): flat for about 2,700 units, but narrower, x ≈ 12,200-12,850.
- **Left and right**: the ground rises about 700-750 units to either side of
  the start. +x is Croc's left at the start (R1 sidestepped him to the
  screen's right and x fell, 12,288 → 11,769 in 60 fields).

The first plays left the flat ground at:

| play | tick | x | z |
|---|---|---|---|
| play_running | 71 | 13,017 | 16,068 |
| play_jumping | 104 | 11,498 | 14,377 |
| play_stomping | 84 | 13,094 | 12,314 |

These positions are, in order:

- ahead and to the left, onto the grass bank below the orange earth;
- to the right;
- just to the left of the start.

Screenshots of the original one tick earlier show the same thing each time.
Croc is on the green grass just beside the stone path, where the grass starts
to slope up toward the brown rock walls.

On screen: **stay on the grey stone path, or at most a step off it onto the
grass**, between the start and the far end of the path's crystals. Don't walk
toward the rock walls, the orange earth, the well on the left or the
?-box/stone ring off to the side. If Croc rises at all, that play has left
the flat ground. A jump has to land on the path too.

## Requests

| feature | recording | request |
|---|---|---|
| F-0001 Croc's running | start_1_1/play_running | Run Croc around freely for 20-40 s: start and stop, turn while running and while standing, back up, sidestep with L1/R1 while moving, change direction quickly. Stay on the grey stone path (at most a step off it onto the grass), between the start and the far end of the crystals; turn back before the grass slopes up toward the rock walls. Touch no walls or objects, and don't jump. |
| F-0002 Croc's jump | start_1_1/play_jumping | Jump freely for 20-40 s: short taps and held jumps, from standing and while running, steering with left/right and turning with L1/R1 in the air, and pressing jump again just before landing. Take off and land on the grey stone path (at most a step off it), between the start and the far end of the crystals, away from the sloping grass by the rock walls. Don't press jump a second time high in a jump (that stomps). |
| F-0003 Croc's stomp | start_1_1/play_stomping | Jump and stomp (press jump again in the air) a few times, from standing and running, at different heights, and move off as soon as Croc can after each. Stay on the grey stone path (at most a step off it), between the start and the far end of the crystals; the grass rises toward the rock walls just beside it, even close to the start. |

Each is run as
`psx scenario play start_1_1 <name> --replace --request "..."`, then named
under the feature's recordings.held_out. The MCP server in the agent's
session is an older build whose scenario_play cannot open its window, so the
person runs these from the terminal.

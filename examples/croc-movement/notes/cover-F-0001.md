# Coverage gaps in Croc's movement opcodes (F-0001 MOVE/TURN, F-0002 rise/fall)

Source: `.psx/coverage-all.json` (code_coverage over the 8 recordings
`scenarios/start_1_1/*.jsonl`), functions MOVE 0x80043234, TURN 0x8004425C,
fall 0x8002D810, rise 0x8002D8D4. Conditions read from `code_disasm` of `/CROC.EXE`
(gp = 0x80074B38). 130 branch pcs have a missed way: MOVE 103, TURN 25, fall 1, rise 1.

- 10 are **reachable** with digital input on the flat floor from `level_1_1`. All 10 are
  verified: run `.psx/runs/1793415860007-2699507-native-135` (from_state `level_1_1`,
  trace, 200 fields), coverage `.psx/coverage-verify-f0001.json`.
- 120 are **unreachable** on a flat static floor with digital controls (the wont_reach
  blocks below).

Names used below: MOVESTATE = gp+0x694 (0x800751CC), w = turn rate gp+0x698 (0x800751D0),
`a2` = TURN's direction code, `s2` = MOVE's strafe code, `s1` = MOVE's "slippery" flag
(surface +0x3C == 1 on ground, or == 0x0F), pad = 0x80075424, surface = obj+0x3C,
wall bits = obj+0x76, on-ground = obj+0x178 bit 2.

## Reachable

Frame numbers are run fields from the `level_1_1` state. One tick is 2 fields. The run
below uses this single input sequence, which takes every row:

```json
[{"frame":0,"buttons":["R1"],"until":6},
 {"frame":20,"buttons":["L1"],"until":26},
 {"frame":60,"buttons":["UP","RIGHT"]},
 {"frame":84,"buttons":["UP"]},
 {"frame":92,"buttons":["UP","LEFT"]},
 {"frame":96,"buttons":["UP"],"until":104},
 {"frame":130,"buttons":["CROSS"],"until":134},
 {"frame":136,"buttons":["R1"]},
 {"frame":142,"buttons":["L1"],"until":148}]
```

| pc | way | function | condition | input that reaches it | verified |
|---|---|---|---|---|---|
| 0x80043710 | taken | MOVE | on ground, L1 held, last MOVESTATE == 0: the moving sidestep (s2 = +3) is skipped | L1 alone while Croc stands still (fields 20..26, 3 ticks). The standing sidestep is root motion only | yes |
| 0x8004373C | taken | MOVE | on ground, R1 held, MOVESTATE == 0: s2 = -3 is skipped | R1 alone while Croc stands still (fields 0..6, 3 ticks) | yes |
| 0x800446A0 | taken | TURN | a2 <= 0 (turning right or right decay) while MOVESTATE is not 0 or 3 (walking or running) | UP+RIGHT held together from standing (fields 60..84, 12 ticks) | yes |
| 0x80044714 | ran (both ways) | TURN | moving, a2 <= 0: a2 == -2 (RIGHT held) or a2 == -1 (RIGHT released, w < -0.4) | UP+RIGHT 12 ticks, then UP alone (fields 84..92) | yes |
| 0x80044728 | ran (both ways) | TURN | moving, RIGHT: w > -11.0, so w = -11.0 (first tick), else accelerate | UP+RIGHT held for at least 2 ticks | yes |
| 0x80044748 | ran (both ways) | TURN | moving, RIGHT: w > -52.0, so w += w>>2, else cap w = -52 (0xFFCC0000) | UP+RIGHT held for at least 9 ticks (w reached -52 at tick 8; samples) | yes |
| 0x8004477C | ran (both ways) | TURN | moving, a2 == -1: w < -3.0, so w >>= 3, else w = 0 | release RIGHT and keep UP after a fast right turn (w -52 to -6.5 to -0.8 to 0, fields 84..90) | yes |
| 0x800446A8 | taken | TURN | moving, a2 > 0 but not 2: a2 == +1 (LEFT released while w >= 0.4) | UP+LEFT 2 ticks (fields 92..96), then UP alone | yes |
| 0x80044700 | ran (both ways) | TURN | moving, a2 == +1: w > 3.0, so w >>= 3, else w = 0 | same as 0x800446A8 (w 11, 13.75, then 1.7, then 0) | yes |
| 0x8004454C | taken | TURN | airborne, L1 held while a2 == -1 (w < -0.4 from an R1 turn), so L1 does not set a2 = +2 and w only decays | CROSS jump (fields 130..134). In the air hold R1 for 3 ticks (fields 136..142), then L1 alone (fields 142..148). w -3, -7.5, -18.75, then -4.7 (>>2) | yes |

Notes for the port: in the air, L1/R1 (not LEFT/RIGHT) feed `a2`. Croc stands still in
the air (MOVESTATE 0), so an air turn uses the standing table (3, x2.5, cap 63; decay >>2).
Pressing the opposite turn button while |w| >= 0.4 only decays w. It does not reverse
the turn in the same tick.

## wont_reach

Reasons, grouped:
- **analog**: control mode 0x8007529C == 2 (camera-relative analog stick). The port has digital tank controls only.
- **room entry**: gp+0x678 (0x800751B0) is the movement-reset request. It is set by
  0x80042ED0, which the main loop 0x80012C6C calls (0x80013044, 0x8001582C) when a map is
  entered. It runs 0x800431AC, which arms entry sequences from the entry record's flags
  0x80075384 (written at 0x800253A8): bit 1 = a 30-tick auto-walk (gp+0x968), bit 2 =
  an auto-jump-in (gp+0x7E4, height 3600, v 261). Both sequences hold the input lock.
- **input lock**: 0x800752C8 != 0 (cutscenes and entry sequences).
- **surface**: special floor surface types obj+0x3C 1 (slippery), 8, 0x0E, 0x0F. The flat floor is type 0.
- **slide**: MOVESTATE 4/5 (sliding). Croc slides only when the floor under him moves fast or on a type 8 surface.
- **wall**: wall-contact bits obj+0x76 come from collision. The flat floor has no walls.
- **moving floor**: standing on an object (obj+0x3E bit 0 clear, platform index +0x79),
  or on a tile whose velocity (+0x24/+0x26) is not zero. The speed thresholds and the slide
  start depend on it. The flat floor is static.
- **ledge**: walking off a ledge during the first 5 ticks of UP. The 0x200000 walk-anim flag is still set while on-ground is cleared (script 0x1923, op 0x23).
- **dead**: the condition cannot hold for any input (s2 only takes the values 0, +-2, +-3; the anim divisor is always 60/64/128/140; ground plus surface 0x0E never reaches the check).
- **underwater**: level+0x6E == 1 (underwater level: fall speed v>>4).
- **surface 4 bounce**: the variable-jump bit 0x2000000 is cleared only before the fixed
  JUMP 2300 bounce off a surface type 4 floor (script 0x19D2..0x1A77). Every other jump
  starts with it set (landing handler, script 0x11B9).

### F-0001 (MOVE 0x80043234, TURN 0x8004425C)

```toml
# MOVE 0x80043234
"0x800432A8" = "room entry: movement-reset request gp+0x678, set only when a map is entered (0x80042ED0 from the main loop)"
"0x800432C0" = "room entry: auto-jump-in counter gp+0x7E4, armed by entry flag 0x80075384 bit 2"
"0x800432C8" = "room entry: auto-jump-in sequence (gp+0x7E4 != 0)"
"0x80043310" = "room entry: auto-jump-in sequence (gp+0x7E4 != 0)"
"0x80043338" = "room entry: auto-jump-in sequence (gp+0x7E4 != 0)"
"0x80043354" = "room entry: auto-walk counter gp+0x968, armed by entry flag 0x80075384 bit 1"
"0x80043368" = "room entry: auto-walk sequence (gp+0x968 != 0)"
"0x8004339C" = "input lock 0x800752C8 != 0 (cutscenes, entry sequences)"
"0x800433AC" = "surface: floor surface type 1 (slippery)"
"0x800433C0" = "surface: floor surface type 1 (slippery)"
"0x800433D8" = "surface: floor surface type 0x0F"
"0x800433F8" = "analog stick mode (0x8007529C == 2): the port has digital tank controls only"
"0x80043410" = "analog stick mode (0x8007529C == 2)"
"0x80043430" = "analog stick mode (0x8007529C == 2)"
"0x80043464" = "analog stick mode (0x8007529C == 2)"
"0x80043474" = "analog stick mode (0x8007529C == 2)"
"0x80043498" = "analog stick mode (0x8007529C == 2)"
"0x800434B8" = "analog stick mode (0x8007529C == 2)"
"0x800434D0" = "analog stick mode (0x8007529C == 2)"
"0x800434E4" = "analog stick mode (0x8007529C == 2)"
"0x8004350C" = "analog stick mode (0x8007529C == 2)"
"0x80043530" = "analog stick mode (0x8007529C == 2)"
"0x80043538" = "analog stick mode (0x8007529C == 2)"
"0x80043560" = "analog stick mode (0x8007529C == 2)"
"0x80043568" = "analog stick mode (0x8007529C == 2)"
"0x8004358C" = "analog stick mode (0x8007529C == 2)"
"0x80043594" = "analog stick mode (0x8007529C == 2)"
"0x800435B8" = "analog stick mode (0x8007529C == 2)"
"0x80043640" = "analog stick mode (0x8007529C == 2), while sliding"
"0x80043658" = "analog stick mode (0x8007529C == 2), while sliding"
"0x80043690" = "analog stick mode (0x8007529C == 2), while sliding"
"0x80043698" = "analog stick mode (0x8007529C == 2), while sliding"
"0x800436B4" = "analog stick mode (0x8007529C == 2), while sliding"
"0x800436BC" = "analog stick mode (0x8007529C == 2), while sliding"
"0x800436D4" = "slide: MOVESTATE 5 (slide on surface 8)"
"0x800436F0" = "slide: MOVESTATE 4 on the ground"
"0x80043794" = "slide: MOVESTATE 4/5 forward input"
"0x800437E8" = "surface: slippery floor (type 1 or 0x0F) while holding UP"
"0x80043824" = "surface: slippery floor (type 1 or 0x0F) while holding DOWN"
"0x80043848" = "slide: MOVESTATE 4/5 forward input"
"0x80043860" = "slide: MOVESTATE 4/5 forward input"
"0x800438A0" = "surface: slippery floor (type 1 or 0x0F), sidestep side speed"
"0x800438B4" = "dead: strafe code is only 0, +-2, +-3, so a positive code other than 3 is always 2"
"0x800438BC" = "surface: slippery floor (type 1 or 0x0F), air strafe side speed"
"0x800438D8" = "surface: slippery floor (type 1 or 0x0F), sidestep side speed"
"0x800438EC" = "dead: strafe code is only 0, +-2, +-3, so a negative code other than -3 is always -2"
"0x800438F4" = "surface: slippery floor (type 1 or 0x0F), air strafe side speed"
"0x80043914" = "wall: wall-contact bits obj+0x76 from collision"
"0x8004391C" = "wall: wall-contact bits obj+0x76 from collision"
"0x80043924" = "wall: wall-contact bits obj+0x76 from collision"
"0x8004393C" = "wall: wall-contact bits obj+0x76 from collision"
"0x80043944" = "wall: wall-contact bits obj+0x76 from collision"
"0x8004395C" = "wall: wall-contact bits obj+0x76 from collision"
"0x80043964" = "wall: wall-contact bits obj+0x76 from collision"
"0x8004397C" = "wall: wall-contact bits obj+0x76 from collision"
"0x80043984" = "wall: wall-contact bits obj+0x76 from collision"
"0x800439B0" = "ledge: walking off a ledge in the first 5 ticks of UP (walk-anim flag 0x200000 still set, not on ground)"
"0x800439C8" = "dead: compiler divide-by-zero guard, the anim divisor is always 60/64/128/140"
"0x80043A08" = "surface: airborne over floor surface type 0x0F"
"0x80043A1C" = "moving floor: standing on an object (obj+0x3E bit 0 clear), not on level geometry"
"0x80043AE0" = "moving floor: standing on an object (platform index obj+0x79)"
"0x80043AF4" = "moving floor: standing on an object (platform index obj+0x79)"
"0x80043B78" = "slide: MOVESTATE 4/5 while airborne"
"0x80043B88" = "slide: MOVESTATE 4/5 while airborne"
"0x80043BD4" = "surface: floor surface type 1 (slippery), floor speed threshold"
"0x80043BDC" = "surface: floor surface type 0x0F, floor speed threshold"
"0x80043BF0" = "surface: floor surface type 1 or 0x0F, floor speed threshold"
"0x80043C00" = "surface: floor surface type 8, floor speed threshold"
"0x80043C14" = "surface: floor surface type 8, floor speed threshold"
"0x80043C28" = "moving floor: floor/platform velocity squared > 0x10000 (static flat floor has 0)"
"0x80043C40" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043C54" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043C64" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043C78" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043C80" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043C94" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043CA8" = "moving floor: fast floor/platform velocity (second threshold)"
"0x80043CC8" = "slide: MOVESTATE 4/5 target velocity"
"0x80043CDC" = "slide: MOVESTATE 4/5 target velocity"
"0x80043D5C" = "slide: MOVESTATE 4/5 target velocity"
"0x80043D64" = "slide: MOVESTATE 4/5 target velocity"
"0x80043E20" = "slide: MOVESTATE 4/5 slide timer gp+0x644"
"0x80043E30" = "slide: MOVESTATE 4/5 slide timer gp+0x644"
"0x80043E88" = "surface: floor surface type 0x0E on the ground"
"0x80043E98" = "surface: floor surface type 0x0F (air formula on the ground)"
"0x80043EEC" = "moving floor: platform-relative speed r < 0 (r = 0 on a static floor)"
"0x80043F70" = "surface: airborne over floor surface type 0x0F"
"0x80043F80" = "dead: ground plus surface 0x0E never reaches this check (it leaves at 0x80043E88)"
"0x80043F90" = "moving floor: fast floor/platform starts a slide (MOVESTATE 4/5)"
"0x80043F98" = "moving floor: fast floor/platform starts a slide (MOVESTATE 4/5)"
"0x8004402C" = "surface: floor surface type 0x0E with no input"
"0x8004403C" = "moving floor: fast floor/platform with no input starts a slide (MOVESTATE 4/5)"
"0x80044044" = "moving floor: fast floor/platform starts a slide (MOVESTATE 4/5)"
"0x8004405C" = "moving floor: fast floor/platform starts a slide (MOVESTATE 4/5)"
"0x800440B0" = "slide: MOVESTATE 4/5 heading follows velocity"
"0x800440E8" = "slide: MOVESTATE 4/5 heading follows velocity"
"0x800440F8" = "slide: MOVESTATE 4/5 heading follows velocity"
"0x80044110" = "slide: MOVESTATE 4/5 heading follows velocity"
"0x8004412C" = "slide: MOVESTATE 4/5 heading follows velocity"
"0x80044168" = "slide: MOVESTATE 4/5 heading follows velocity"
"0x80044188" = "slide: MOVESTATE 4/5 heading follows velocity"
# TURN 0x8004425C
"0x80044278" = "analog stick mode (0x8007529C == 2): the port has digital tank controls only"
"0x8004428C" = "analog stick mode (0x8007529C == 2)"
"0x800442BC" = "analog stick mode (0x8007529C == 2)"
"0x800442CC" = "analog stick mode (0x8007529C == 2)"
"0x8004431C" = "analog stick mode (0x8007529C == 2), with the input lock"
"0x80044348" = "analog stick mode (0x8007529C == 2)"
"0x80044358" = "analog stick mode (0x8007529C == 2)"
"0x80044374" = "analog stick mode (0x8007529C == 2)"
"0x80044388" = "analog stick mode (0x8007529C == 2)"
"0x800443C4" = "analog stick mode (0x8007529C == 2)"
"0x800443E0" = "analog stick mode (0x8007529C == 2)"
"0x8004441C" = "analog stick mode (0x8007529C == 2)"
"0x80044464" = "analog stick mode (0x8007529C == 2), airborne"
"0x8004447C" = "analog stick mode (0x8007529C == 2), airborne"
"0x800444C4" = "slide: MOVESTATE 5 (slide on surface 8) ignores turn input"
"0x800444D8" = "input lock 0x800752C8 != 0 (cutscenes, entry sequences)"
"0x800444F4" = "slide: MOVESTATE 4 on the ground turns with L1/R1"
```

### F-0002 (fall 0x8002D810, rise 0x8002D8D4)

```toml
"0x8002D824" = "underwater level (level+0x6E == 1): falls at v>>4"
"0x8002D940" = "surface 4 bounce: only the fixed JUMP 2300 off a surface type 4 floor clears the variable-jump bit 0x2000000 (script 0x1A76)"
```

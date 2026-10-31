# Croc's player controller (PAL, SLES-00593): ground movement, turning, jump, stomp

Evidence: `/CROC.EXE` (load 0x80010000, gp = 0x80074B38) read with psx `code_disasm` /
`code_decompile`, and runs from the named state `level_1_1_idle` (Croc standing in 1-1,
object at 0x800D8A1C). Every rule below cites the instruction(s) that implement it.
"Verified" = a run reproduced the numbers; "read" = from code only; "unverified" = guess.

Units: positions are raw i32 world units (y up, standing on 1-1's start floor y = -225).
Angles are u16 0..0xFFF (4096 = full turn). sin/cos are PsyQ `rsin`/`rcos`
(0x800544FC / 0x800545C8, quarter-wave table at 0x8006D5D4, 4096 = 1.0); forward for
angle `a` is (+x, +z) = (sin a, cos a); LEFT increases the angle (verified, run below).

## 0. The big structural finding: Croc is a *script*, not a C controller

Each object has a bytecode program ("strat"); the C code is the interpreter plus a
library of opcodes. Croc's behaviour (states, jump, stomp, animation choice, timers)
is in Croc's script; the C opcodes do the arithmetic (movement, turning, gravity).

- Object update loop: 0x8002C790 (called once per logic tick from 0x80013C08).
- Dispatch: 0x8002CFF8..0x8002D030: `op = *obj->pc; handler = table[op]` with the
  table at **0x8006B424** (0xD0 entries); loop *while (obj+0x174 & 2) == 0* - an opcode
  sets bit 1 of +0x174 to end the object's tick (yield).
- obj+0x00 = script PC, obj+0x04 = script base (jump targets are base-relative u16),
  obj+0x08 = local-variable array (`L<n>`, 4 bytes each; Croc's at 0x8007C300),
  globals `G<n>` at 0x80086498 + 4n, obj+0x10 = parent object.
- Expression evaluator 0x8002F168: stack machine, values 16.16 fixed (`1` = 0x10000);
  properties (opcode 3 + u16) return raw quantities: pad words, MOVESTATE, y, floor...
  Opcodes and operand formats I decoded are in
  `notes/player-controller-script/dis.py` (disassembler), the decoded Croc program in
  `notes/player-controller-script/croc.dis` (offsets are script-relative; script base
  0x800D8E18 in this state, length 0x3BE4 stored at base-4). Seeds = PCs seen in
  traced runs (`seeds.txt`). Croc's program text is copied in after the object record;
  which disc file it comes from (CROCFILE.1 / STRAT.DIR?) is **unverified**.
- Statement opcodes used by the player paths (handler address):
  `15` if expr else +s16 (0x8002817C); `16` jmp s16 (0x800281E4); `1A` switch
  (0x8002844C); `1B` goto u16 (0x80028538); `1C` call u16 (0x8002AE80); `39` ret;
  `1E` **yield, resume at u16** (0x8002AED4: sets +0x174|=2); `36` L[u16]=expr
  (0x8002A9C0); `38` property[u16]=expr (0x8002AC0C); `5E` G[u16]=expr;
  `0B` set animation (0x80027CB0); `0C` stop anim; `22` **JUMP(height)** (0x80028688);
  `73` **stomp-fall** (0x800286F4); `76` clear jump/fall (0x80028734); `23` start fall
  (0x8002879C); `A0` **stop horizontal movement** (0x8004317C); `BF` **MOVE**
  (0x80043234); `C0` **TURN** (0x8004425C); `44`/`B0` var += / -= 1.0
  (0x8002BB5C/0x8002BC54); `65` anim speed +0x146 = expr>>8 (0x8002B0FC);
  `6A`/`6B` set/clear +0x174 bit 0x200000 (velocity-driven anim speed);
  `71`/`72` set/clear +0x174 bit 0x2000000 (variable-height jump enable).
- Engine-model consequence: the *VM, opcodes, physics opcodes and event dispatch* are
  engine; Croc's *script* (states, thresholds like 2300/983/131, timers) is data. A
  faithful port can either interpret the script or hand-translate it; either way the
  numbers below are where each constant lives.

## 1. Per-tick order (one logic tick = 2 PAL fields, 25 Hz)

Main loop 0x80012C6C, logic step 0x80013BB0..0x80013CE8 (accumulator 0x80075240).
1. Pad read **0x80015EA0** (called at 0x80013B60 every main-loop pass and at 0x80013BE0
   for extra steps): `pad = 0x800393A8()`; `pressed = pad & ~prev` -> 0x8007536C;
   `released = prev & ~pad` -> 0x80075368; `prev` at 0x80074B54. Verified: pressed bits
   show for exactly one tick (samples, run `...-native-51`).
2. **0x8002C790** objects:
   a. every object: copy angles/pos +0x368..+0x37C to +0x380.. (previous), then animation
      advance: `+0x144 += *(s16*)anim + (s16)+0x146`; for each whole 0x100: next frame,
      apply that frame's **root motion** (frame record anim[+0xC] + frame*0x18: s16
      dAngX/dAngY/dAngZ at +0/+2/+4, i32 dx,dy,dz at +8/+0xC/+0x10, `pos += (R*(d*4)+2)>>2`)
      and end the animation (clear +0x174 bit 8) when `frame+1 >= anim[+8]`.
      (0x8002C8xx..0x8002CA80, read.) Standing sidestep (L1) moves Croc purely by root
      motion: x +~50/tick with velocity 0 (verified, run `...-native-124`).
   b. per object, in list order: `+0x174 &= ~0x3002` (clears yield, LANDED 0x1000,
      APEX 0x2000) [mask 0xCFFD, 0x8002CCD0..0x8002CCFC]; **gravity/jump physics**: if +0x174&0x400 -> fall
      0x8002D810, else if &0x200 -> rise 0x8002D8D4 [0x8002CCF0..0x8002CD24];
      **event handlers** (table at obj+0x1A0, 0x1C-byte entries, type at +0xC, handler
      offset at +0x10; type 9 = LANDED flag 0x1000, type 8 = APEX flag 0x2000, type 7 =
      animation frame event; jump table 0x80010BC8, code 0x8002CD30..0x8002CFF4). A
      handler runs in the same tick, before the main script resumes: it pushes the pc and the entry's
      +0x18, sets +0x174 bit 0x20000000 (s2 = 0x20000000, set at 0x8002CCBC) and returns
      via opcode 0x34 (pops the pc, clears 0x20000000).
      Then the **script** until yield; then **collision** 0x8002D410 if +0x174&0x78000
      (Croc has 0x40000 -> 0x800362E4 -> 0x80031F64: floor height -> +0x38, surface type
      -> +0x3C (writers 0x800361A4 / 0x80036198), wall-contact bits -> +0x76, riding
      platforms).
3. Camera 0x8004162C after the objects (0x80013C58).

So inside one tick, Croc does: anim/root motion -> rise/fall -> landed/apex handlers ->
script (MOVE, jump/stomp test, TURN, ...) -> collision (new floor for *next* tick).

### Croc's script main loop (script offset 0x1EA8 .. yield at 0x368B)
- 0x1EB4..0x1F8C camera-distance presets (L2/R2 = pad 0x4000/0x8000, G31), hit/death.
- 0x21D8 `call 0x1923` (ground snap / walk-off-ledge, below) unless G0 in {13,8,9,11}.
- 0x21DB while falling: `L4 += 1`; L4==40 scream sound, L4==60 -> death (G0=12).
- 0x221B `switch G0` (player state, **G0 = global 0 at 0x80086498**, 16.16):
  1 normal (ground, jump and fall all live here) -> 0x2F9D; 4 stomp -> 0x2732;
  5 tail attack (0x2B82); 3 180-turn (0x2D18); 2 hurt; 12 dying; 13 victory;
  6,7,8,9,10,11 not traced (unverified meaning).
- State 1 body: `MOVE` (0x2F9D) ; `call 0x1803` (jump/stomp) ; `call 0xA9C` (TURN) ;
  SQUARE pressed (0x800) and L153==0 -> attack, G0=5 ; CIRCLE pressed (0x1000) ->
  180 turn, G0=3 ; animation selection by MOVESTATE (0x312D...).
- 0x361F..0x3645: if G0==1 && MOVESTATE==1 (walk) && on ground: set +0x174 bit
  0x200000 (walk anim rate follows speed) else `+0x146 = 0` and clear it.
- 0x3649..0x3687: count down L151, L153, L12, L10, L11, L152 (each -1.0 per tick if
  non-zero). `yield, resume 0x1EA8`.

## 2. Pad word 0x80075424 (built by 0x800393A8 from raw pad 0x8007C19A, active low)

Digital pad (id 0x4x, control mode 0x8007529C = 1) with button layout gp+0x884 =
0x800753BC = 0 (the value in 1-1):

| button | raw bit | game bits | meaning to the controller |
|---|---|---|---|
| UP | 0x0010 | 0x00010001 | 0x1 forward |
| DOWN | 0x0040 | 0x00040002 | 0x2 backward |
| LEFT | 0x0080 | 0x00020004 | 0x4 turn left (ground) / strafe left (air) |
| RIGHT | 0x0020 | 0x00080008 | 0x8 turn right / strafe right |
| CROSS | 0x4000 | 0x00800010 | 0x10 jump / stomp |
| SQUARE | 0x8000 | 0x00400800 | 0x800 tail attack |
| CIRCLE | 0x2000 | 0x00201000 | 0x1000 180 turn |
| TRIANGLE | 0x1000 | 0x00100000 | (high half only; free-look 0x40000000 in game mode 4) |
| L1 / R1 | 0x0400 / 0x0800 | 0x80 / 0x100 | sidestep (ground), turn (air) |
| L2 / R2 | 0x0100 / 0x0200 | 0x6000 / 0xA000 | camera presets (0x4000 / 0x8000) |
| START / SELECT | 0x0008 / 0x0001 | 0x200 / 0x400 | |

Code: 0x80039460..0x80039B6C (layout 1 and 2 swap CROSS/CIRCLE/SQUARE roles, read).
The high half (0x0001xxxx..) is a camera-oriented copy read by the camera code.
Observed words: UP 0x00010001, LEFT 0x00020004, RIGHT+UP 0x00090009, DOWN 0x00040002,
CROSS(+UP) 0x00810011 (verified, runs `...-native-51`, `...-native-52`).
0x800752C8 non-zero = input lock (expression property 0xC/0xD/0xE return 0, MOVE and
TURN ignore the pad). Control mode 0x8007529C: 0 none, 1 digital (tank controls),
2 analog (camera-relative stick: target heading = camera yaw 0x8006BAF6 + stick angle
0x800751E4, magnitude 0x800751DC vs dead zones 0x800752A8=0x3F walk / 0x800752AC=0x7B
run). Only digital mode is detailed here.

## 3. Ground movement: opcode MOVE 0x80043234 (digital mode)

State: **MOVESTATE** gp+0x694 = 0x800751CC (0 idle, 1 walk, 2 run, 3 backwards, 4 slide,
5 slide on surface 8), **walk counter** gp+0x768 = 0x800752A0, **velocity** vx gp+0x1D0 =
0x80074D08, vz gp+0x1D4 = 0x80074D0C (units: world units << 12), speed gp+0x1D8 =
0x80074D10.

Per call (pc += 1 at 0x80043298):
1. `slip = (surf(+0x3C)==1 && on ground) || surf==0x0F`; smoothing shift `k = slip ? 5 : 2`
   [0x800433A4..0x800433E8].
2. Strafe input `s` (uses last tick's MOVESTATE `st`) [0x800436CC..0x80043780]:
   if st==5 none; else if airborne or st==4: LEFT -> +2, RIGHT -> -2 (RIGHT wins);
   else (ground): L1 && st!=0 -> +3; R1 && st!=0 -> -3.
3. Forward input [0x80043784..0x8004383C]:
   - st 4: fwd = UP ? 2 : DOWN ? -4 : 0 (st 5: 0).
   - UP held: if counter==5 -> st=2 (run) else st=1 and counter++; fwd = slip ? 4 : **35**;
     anim divisor d = slip ? 128 : 140.
   - else counter=0; DOWN -> st=3, fwd = slip ? -2 : **-15**; neither -> st=0, fwd=0.
   So "walk" is just the first 5 ticks of UP (st 1); run is st 2; **both use the same
   target 35** - the difference is animation and turn rate (verified: counter 1..5 then
   st 2, run `...-native-124`).
4. Side speed `side` from s [0x80043888..]: +-3 -> slip ? +-2 : **+-12**;
   +-2 -> slip ? +-4 : **+-17**.
5. Wall bits +0x76 (from collision): bit1 kills fwd>0, bit2 fwd<0, bit4 side>0, bit8
   side<0 [0x80043370 block in decompile].
6. Walk anim rate: if +0x174&0x200000 && on ground:
   `+0x146 = ((speed << 8) / d) - 0x100` (speed = gp+0x1D8 from the previous call).
7. Platform carry: when on ground (or surf 0x0F), velocity of the object stood on
   (+0x79 index, +0x3E bit 1) is read; zero on static floor. (read, not traced.)
8. Target velocity (not slide):
   - fwd==0 && side==0: target = 0 (and on ground with fast platform -> slide st 4/5).
   - airborne (or surf 0x0E), not surf 0x0F:
     `tx = sin(a)*fwd + cos(a)*side; tz = cos(a)*fwd - sin(a)*side`.
   - on ground: `r = pz*cos(u-a) - px*sin(u-a); r = (r>>4)+(r>>5); if r<0 r>>=1;
     f = fwd*4096 - r; tx = (sin(a)*f>>12) + cos(a)*side; tz = (cos(a)*f>>12) - sin(a)*side`
     (with no platform r = 0, identical to the air formula).
9. Smoothing [0x8004406C]: `vx += tx - (vx >> k); vz += tz - (vz >> k)` (arithmetic
   shifts). Equilibrium |v| = 4*35*4096 -> **140 units/tick** running (verified: vz ->
   0x8A7F7 at angle 94, z += 138/tick).
10. Slide states 4/5 steer the heading toward the velocity (0x80044080..0x800441C8, read).
11. `speed = SquareRoot12((vx>>6)^2 + (vz>>6)^2) >> 12` (0x800546F8: GTE LZCS + table
    0x8006DDE8, PsyQ-SquareRoot12-like; name unverified) ;
    **`x += vx >> 12; z += vz >> 12`** [0x80044208, 0x80044224].
Stopping: release UP -> target 0, v decays by v>>2 per tick (x/z deltas 75%,...,
verified run `...-native-52` frames 81..99).
STOPMOVE (opcode A0, 0x8004317C) zeroes vx, vz, speed, MOVESTATE, counter, turn rate.
MOVE is only executed in G0==1 (and other states that call it; stomp does not).

## 4. Turning: opcode TURN 0x8004425C (digital mode), called after MOVE and the jump test

Turn rate `w` = gp+0x698 = **0x800751D0**, 16.16 angle units per tick.
1. `a2 = (w >= 0x6667) ? 1 : (w < -0x6666) ? -1 : 0` [0x8004448C..0x800444B8].
2. Input (skipped if MOVESTATE==5 or input lock 0x800752C8): on ground and MOVESTATE!=4:
   LEFT && a2!=-1 -> a2=2; RIGHT && a2!=1 -> a2=-2. Otherwise (airborne / slide): **L1 / R1**
   instead of LEFT/RIGHT [0x800444E0..0x80044578]. (Air: LEFT/RIGHT strafe, they do not
   turn - verified run `...-native-123`: angle stays 94 in the air with LEFT; L1 turns.)
3. If a2 != 0: `angle = (angle + (w >> 16)) & 0xFFF` (lh of the high half, 0x80044584) -
   the angle uses **last tick's w**, then w is updated:
   - MOVESTATE 0 or 3 (standing/backwards):
     a2=+2: w <= 0x2FFFF -> w = 0x30000; else w <= 0x3EFFFF -> `w = (w<<1) + (w>>1)` (x2.5);
     else w = 0x3F0000 (63). a2=-2 mirrored (-0x30000, cap -0x3F0000 = 0xFFC10000).
     a2=+-1 (released): |w| > 3.0 -> `w >>= 2` else w = 0.
   - other MOVESTATE (walk/run/slide):
     a2=+2: w <= 0xAFFFF -> w = 0xB0000 (11); else w <= 0x33FFFF -> `w += w>>2` (x1.25);
     else w = 0x340000 (52). a2=-2 mirrored (-0xB0000, cap 0xFFCC0000).
     a2=+-1: |w| > 3.0 -> `w >>= 3` else 0.
   (constants at 0x800445C8..0x80044790.) So turning from standing accelerates
   0,3,7,18,46,117->63 per tick; running 11,13,17,21,26,33,41,52 cap 52.
   Verified exactly: LEFT standing angle deltas 3,7,18,46,117,63,63...; RIGHT+UP from a
   +63 spin: +7 (63>>3), 0, -11,-14,-18,-22,-27,-34,-42,-53,-52 (run `...-native-52`).
   Reversing direction first has to decay through a2=+-1.
4. Script after TURN (0xA9D..0xAD7): when running (MOVESTATE 2) and |w| > 30.0 set the
   lean side L36; sub 0xAD8 eases the roll angle angZ (L34 x2 up to 128, /2 back).
   Cosmetic only (read).

## 5. Jump

Script, sub 0x1803 (called in state 1 right after MOVE):
- Condition [0x1803..0x18A0]: MOVESTATE != 5 and G0 != 4, and (CROSS **pressed** edge
  0x8007536C&0x10 or G69==1 (bounce pad) or **L152** (buffered jump)):
  - if not JUMPING/FALLING and L151 == 0: G69==1 -> JUMP 4600 (CROSS pressed) / 3450;
    else play CROCJMP1 and **JUMP 2300**; stop anim; **L103 = 1** (stomp allowed).
  - else if CROSS pressed: **L152 = 5** (jump buffer: a press within 5 ticks before
    landing jumps on the first tick it is allowed).
- JUMP opcode 0x22 (0x80028688): `+0x14C h = expr>>16` (2300), `+0x150 v = 0x105` (261),
  +0x174: clear 0x400, set 0x200 (JUMPING); +0x178 clear bit 1 (ON GROUND).
  y does not change this tick.
- Rise 0x8002D8D4 (each later tick, before the script):
  ```
  y += v; h -= v;
  if (h < 700)                         v = v - 8 - (v >> 2);
  if (h < 1400 && (+0x174 & 0x2000000) && !(pad 0x80075424 & 0x10))  v -= v >> 1;
  if (h < 4 || v < 4) { h = v = 0; +0x174 = (+0x174 & ~0x200) | 0x2400; }   // FALLING + APEX
  ```
  Bit 0x2000000 ("variable jump") is set by the landing handler (script 0x11B9) and is
  on in 1-1; cleared only for the lava bounce (0x1A76).
  Verified (y from -225): CROSS held: 36,297,558,819,1080,1341,1602,1790,1923,2015,2076
  (apex +2301); held 2 ticks: ...819,950,1016,1049,1066,1075,1080 ; held 6 ticks:
  ...1341,1472,1518,1532 (runs `...-native-111/112/113`). The release test happens every
  rise tick once h < 1400, so height is variable until the apex.
- Fall 0x8002D810 (each tick while +0x174&0x400, before the script):
  ```
  y -= v;   (v >> 4 instead when level+0x6E == 1, underwater - read)
  +0x178 &= ~2;  v = v + 8 + (v >> 2);  if (v > 0x240) v = 0x240;
  if (y <= floor(+0x38)) { y = floor; +0x174 = (+0x174 & ~0x400) | 0x1000; +0x178 |= 2; v = 0; }
  ```
  v starts at 0 after the apex: 1080,1080,1072,1054,1024,979,915,827,709,554,353,94,-225
  (v 8,18,30,45,64,88,118,155,201,259,331,421,534, **max 576**). Verified.
- Landing: flag 0x1000 fires event type 9 -> handler script 0x119E (same tick, before the
  main script): set 0x2000000, `y = floor`, G69=0, **L151 = 1** (no jump this tick;
  the press is buffered via L152), L4 = 0, **L103 = 0**, landing animation by MOVESTATE.
- Apex flag 0x2000 fires type 8 -> 0x1425: switch to the falling animation.
- Horizontal control in the air: MOVE runs every tick in the air with the same targets
  (UP 35, DOWN -15), smoothing k=2; LEFT/RIGHT give strafe +-17 instead of turning; L1/R1
  turn. With no input the momentum decays by v>>2 per tick.
- Walking off a ledge: script 0x1923: if `y - floor < 131` (0x83) then `y = floor`
  (ground snap, also up-steps; y <= -40960 -> death) else if not jumping/falling and G0
  not 7/4: G0=1 (unless 5/3/12), stop anim, L103=0, op 0x81, **op 0x23 start fall**
  (v=0, set 0x400, clear ground). So no stomp after walking off a ledge.

## 6. Stomp (G0 = 4)

- Trigger (script 0x18B9..0x18F0, same call as the jump test): **L103 && CROSS pressed
  edge && (y - floor) > 983 (0x3D7)** -> L152 = 0, **G0 = 4**, L104 = 0, L103 = 0.
  y is after this tick's rise/fall, floor is last tick's collision result.
  (A press lower than 983 is only buffered as a jump.)
- State 4 script 0x2732, one step per tick, L104 = sub-state:
  - L104 0 (first tick): opcode 76 clears JUMPING/FALLING and ON GROUND (no gravity:
    **hang**), random grunt, **STOPMOVE** (vx=vz=0), anim CROCSMP1 (15 frames, speed
    0x100 -> 1 frame/tick), L104 = 1. Croc does not call MOVE in state 4 (no x/z motion).
  - L104 1: wait until the anim ends, then opcode 73 (0x800286F4): set 0x400 FALLING,
    clear 0x200 and ground, **v = 0x180 (384)**; anim SMP2; L104 = 2; set +0x174 bit
    0x10000000; G1 = 2.
  - L104 2: while falling keep SMP2; when landed: L104 = 3, y = floor.
  - L104 3: CROCLAND sound, anim SMP3 (2 frames), L104 = 4.
  - L104 4: when the anim ends: L104 = 5, spawn StompWobbleCamera; if CROSS held and
    still > 131 above floor -> stop anim, L104 = 1 (stomp again) ; else G1 = 0.
  - L104 5..15: +1 per tick (op 44); at 16: anim SMP4, **G0 = 1**, clear 0x10000000,
    G1 = 0, **L153 = 5** (attack locked 5 ticks), L1 = 1.
- Verified timeline (run `...-native-111`, jump then CROSS again at y=950): y 950 ->
  1081 (one more rise tick before the state-4 script runs) -> 15 ticks at 1081 (anim
  frames 0..14) -> 697, 209, -225 (v 384, 488, 576) -> landed; L104 3,4,4,5..16 ->
  G0=1 at the 16th tick after landing; MOVE runs again on the tick after that.

## 7. Ground height / what a port needs for a flat floor

- floor = **obj+0x38** (i32), written by collision 0x80031F64 (store 0x800361A4) after
  the script each tick; surface type obj+0x3C (store 0x80036198); wall bits obj+0x76
  (cleared at the start of collision). Landing compares `y <= +0x38` in 0x8002D810;
  ground snap / walk-off uses `y - +0x38 < 131` (script 0x1923); stomp height uses
  `y - +0x38 > 983`.
- The real query walks the level's tile/placement tables (placements 0x18 bytes at
  level+0x54 with yaw at +2, tile records 0x68 bytes; per-probe points at obj+0x40,
  count obj+0x3F; 0x800306B0 per probe). For a flat test floor: floor = constant,
  surface = 0, wall bits = 0, no platform.
- Flags: obj+0x174: 0x2 yield (per tick), 0x8 animation playing, 0x200 jumping,
  0x400 falling, 0x1000 landed (event, 1 tick), 0x2000 apex (event, 1 tick),
  0x40000 run collision, 0x200000 walk-anim rate from speed, 0x2000000 variable jump,
  0x10000000 stomp falling. obj+0x178 bit 0x2 = on ground.
- Other object fields: +0x14C jump height budget, +0x150 vertical speed (rise or fall),
  +0x144/+0x146 anim accumulator / extra rate, +0x140 frame, +0x24 animation,
  +0x36A yaw, +0x370/+0x374/+0x378 x/y/z.
- Globals: G0 0x80086498 player state; MOVESTATE 0x800751CC; walk counter 0x800752A0;
  turn rate 0x800751D0; vx/vz 0x80074D08/0x80074D0C; speed 0x80074D10; pad 0x80075424,
  pressed 0x8007536C, released 0x80075368; input lock 0x800752C8; control mode
  0x8007529C. Script locals (Croc, 0x8007C300 + 4n): L103 may-stomp, L104 stomp step,
  L151 jump lockout, L152 jump buffer, L153 attack lockout, L4 fall ticks.

## Not verified / open

- Analog (mode 2) movement and turning; slide states 4/5 and slippery-surface numbers
  (read only); platform carry; L1/R1 sidestep while moving (read).
- Meanings of G0 states 2,6..11 and of opcodes 0x23/0x81/0xAA in landing paths beyond
  what is listed.
- The sqrt routine identity; the exact root-motion rotation (0x80055438/0x800548A8 GTE
  matrix code) - needed for sidestep and any anim with displacement.
- Which disc file holds Croc's script, and whether its bytes are identical per level.

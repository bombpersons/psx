# Case study: Croc's player movement, from RAM to a native Bevy demo

Game: *Croc: Legend of the Gobbos*, PAL (SLES-00593), level 1-1. Every result
below comes from the psx native core (`psx run --backend native`). The demo is
[`examples/croc-movement`](../examples/croc-movement/README.md).

Result: the walking, turning, sidestepping, jumping and falling rules on flat
ground are complete. An integer port of them (`examples/croc-movement/tools/reference_model.py`)
reproduces all 8 recorded scenarios (329 ticks) with zero error in position and
heading. The Bevy demo states the same rules in natural units. It stays within
55 units of position (a running step is 140) and 5/4096 of a turn, and it
leaves and reaches the floor on exactly the same ticks.

## Method

1. **Reaching the level.** `.psx/native/replay_start.json` presses Start from frame 3200 on. Running it to frame 5400 with
   `--save-state` leaves Croc standing at the start of 1-1, with nothing pressed (44 s):

   ```bash
   psx run --backend native --bios bios/scph7502.bin "media/Croc - Legend of the Gobbos.cue" \
       --replay .psx/native/replay_start.json --frames 5400 --save-state --out .psx/croc-study/boot
   ```

2. **Candidate variables.** Short runs from the state with nothing pressed, Up held and Right held, each for 10, 20, 30 and 40 frames.
   A Python diff of the `ram.bin` dumps kept the words that are constant when idle and change monotonically under Up.
   - The noise was GPU packets (packed XY words in 0x801Axxxx) and the ordering table.
   - Filtering out values above 2^20 left a short list. Three triples appeared: the camera (0x8006BAE4…), a copy at 0x8007F488/0x8007F490, and the player object at 0x800D8D8C.
   - The player object's triple starts at (12288, -225, 12288): round numbers at the level start.

3. **Writers.** `--watch ADDR[:W]` reports each store with its PC and its old and new values. It showed the following:
   - Positions and velocities change only on odd frames after the state, so Croc's logic runs at 25 Hz.
   - The velocity at 0x80074D08 is written by 0x80044084 and the position by 0x80044208, both inside the function at 0x80043234.
   - The heading is written by 0x8004459C, inside 0x8004425C.
   - Y is written by 0x8002D8E4 (rising), 0x8002D84C (falling) and 0x8002AC7C (a behaviour-script opcode).

4. **Rules from code.** For the code, three sources were combined:
   - `psx disasm` of all of CROC.EXE, converted to text (see "Toolkit friction").
   - Prologue/epilogue scanning to find where functions start.
   - A `psx run --trace --trace-from-frame N --trace-frames 2 --trace-range LO-HI` of one tick with buttons held.

   The trace's register and memory effects made the arithmetic readable without a decompiler.
   `psx symbols` named rsin/rcos/SquareRoot12/VSync; these are libgte/libapi, and the game code is unnamed.

5. **Checking.** A per-tick integer model was built and compared with watch-derived trajectories after each hypothesis. Every mismatch pointed at a missing rule:
   - the ground snap;
   - the sidestep reading the previous tick's gait;
   - L1/R1 turning in the air.

   Scenarios were cut where Croc leaves the flat start area, because slopes change speed and height. See "Not modelled".

## Frame rate

The main loop at 0x800145D8 compares the vblank counter (gp+0x890) with the value from the previous frame. If only one vblank has passed, it calls `VSync(0)` once more.
Each game frame is therefore at least two PAL vblanks, which gives 25 Hz. A slow frame takes longer, and the movement code uses fixed per-tick amounts.
The loop also accumulates elapsed vblanks × 128 into gp+0x708. That accumulator presumably serves animation and timers, but its consumers were not traced.
Player movement does not use it: every watched variable changed by the same amounts on every tick in every run.

The pad is sampled per tick as well: a jump starts on the first tick that sees Cross.

## RAM map (this state; the player object is heap-allocated)

`gp` = 0x80074B38. The player object is at 0x800D8A1C. Objects are 0x3F8 bytes, indexed from the array at gp+0x6B8.
Pointers to the player object are stored at 0x8007524C and 0x8007528C.

| Address | Width | Meaning | Evidence |
|---|---|---|---|
| 0x800D8D8C / 90 / 94 (obj+0x370/374/378) | s32 | Croc's position X, Y (up), Z. World units | Watch: written by 0x80044208 (X), 0x80044224 (Z) and the vertical functions (Y). Start (12288, -225, 12288) |
| 0x800D8D86 (obj+0x36A) | u16 | Heading, 4096 per turn. Forward = (sin h, cos h) in XZ. Larger turns left | Read with `lh 874(s4)` before `rsin`/`rcos` in 0x80043234. Written by 0x8004459C |
| 0x80074D08 / 0x80074D0C (gp+0x1D0/1D4) | s32 | Planar velocity X/Z × 4096 | Written by 0x80044084/0x8004409C. Added `>>12` to the position |
| 0x80074D10 (gp+0x1D8) | s32 | Speed = √((vx>>6)²+(vz>>6)²)>>12, via `SquareRoot12` | 0x800441E0 to 0x800441EC |
| 0x800751D0 (gp+0x698) | s32 16.16 | Turn rate. Its high half (`lh 0x800751D2`) is added to the heading | 0x80044584 to 0x8004459C |
| 0x800751CC (gp+0x694) | s32 | Gait: 0 idle, 1 first 5 ticks of Up, 2 walking, 3 Down; 4/5 not seen | Written by 0x800437C8/0x800437D4/0x80043820/0x80043834 |
| 0x800752A0 (gp+0x768) | s32 | Ticks Up has been held, saturating at 5 | 0x800437B4 to 0x800437E4 |
| 0x80075424 | u32 | Game pad bits: 0x1 Up, 0x2 Down, 0x4 Left, 0x8 Right, 0x10 Cross, 0x80 L1, 0x100 R1 | Tests in 0x80043234/0x8004425C/0x8002D948. Bits confirmed by replays (L1 sidesteps left, R1 right) |
| obj+0x150 (0x800D8B6C) | s32 | Vertical speed (rising or falling) | Watch: set to 261 by 0x800286B8 |
| obj+0x14C (0x800D8B68) | s32 | Remaining jump height budget | Watch: 2300 at take-off (0x800286B0), then −vy per tick (0x8002D8F8) |
| obj+0x38 | s32 | Floor height under Croc | Compared in 0x8002D88C. −225 at the start |
| obj+0x178 bit 1 | flag | On the ground | Set on landing at 0x8002D8C4, cleared by the fall at 0x8002D85C |
| obj+0x3C | u8 | Animation/state id (13 idle, 11 run, 4 airborne) | Observed only |
| 0x8007F488 / 0x8007F490 | s32 | Copy of X/Z, written by 0x8004162C (camera/collision pass). Lags on turns | Watch |
| 0x8006BAE4… | s32 ×3 | Camera position | Diff only |

## The rules

One tick runs these steps in this order: rise/fall, the player control function, turning, then the behaviour script.
The order comes from `--trace-range` call lines `->` within one frame: 0x8002D8D4 → 0x80043234 → 0x8004425C → script opcode 0x8002AC7C.

The consequences are:
- Walking uses the previous tick's heading.
- The landing flag is already updated when the control function runs.
- A jump pressed on tick *t* starts rising on tick *t+1*.

### Walking: control function 0x80043234

```
fwd  = Up ? 35 : Down ? -15 : 0            # 0x800437F8 / 0x80043828 (4 / -2, and sidestep 2/4, when s1 != 0: an unidentified condition, never seen)
side = airborne ? (Right ? -17 : Left ? 17 : 0)               # 0x800438C0 (Right wins)
     : gait_prev != 0 ? (R1 ? -12 : L1 ? 12 : 0) : 0           # 0x800438A4 (sidestep only while moving)
target = (fwd·sin h + side·cos h,  fwd·cos h − side·sin h) << 12      # rsin/rcos, 4096 = 1.0
v = v − (v >> 2) + target                  # 0x8004406C..0x8004409C
pos.xz += v >> 12                          # 0x800441F0..0x80044224
```

The steady state is v = 4·target. Speeds per tick are therefore 140 forward, 60 backwards, 48 sidestep and 68 air strafe, reached exponentially at 3/4 per tick.
The truncation `v >> 12` makes the real top speed 138 units/tick (measured: Z += 138, X += 20 at heading 94).

### Turning: 0x8004425C

```
coast = ω ≥ 0.4 ? +1 : ω < −0.4 ? −1 : 0           # 26215 / −26214 in 16.16
(L, R) = on_ground ? (Left, Right) : (L1, R1)
if L and coast != −1: dir = +2;  if R and coast != +1: dir = −2
if dir == 0 and coast == 0: done                      # a small leftover rate stays
heading += ω >> 16   (mod 4096)                       # before ω is updated
profile = gait in {idle, back} ? standing : walking
held:     |ω| < start ? start : |ω| < cap ? |ω|·growth : cap      (signed)
released: |ω| > 3 ? ω >> div : 0
standing: start 3, growth ×2.5 (2ω + ω>>1), cap 63, div >>2
walking:  start 11, growth ×1.25 (ω + ω>>2), cap 52, div >>3
```

The cap is checked before multiplying, so one tick overshoots. Holding Right from standing gives deltas of −3, −8, −19, −47, then **−118**, then −63 per tick.
A full turn on the spot takes about 65 ticks (2.6 s). There is another camera-relative branch at 0x80044294, taken when 0x8007529C == 2, which is probably an alternative control setting. It was not exercised.

### Jumping and falling: 0x80028688, 0x8002D8D4, 0x8002D810

- **Take-off.** The behaviour-script handler 0x80028688 sets vy = 261 and budget = 2300.
  - The 2300 comes from the script's argument through 0x8002F168. Another path sets budget 3600 (0x800432CC, gp+0x7E4 == 1, not seen).
  - The jump is edge-triggered. Holding Cross through a landing does not jump again, but a new press on the tick after landing does.
- **Rise (0x8002D8D4).** Each tick:

  ```
  y += vy; budget -= vy
  if budget < 700: vy = vy − 8 − (vy >> 2)
  if budget < 1400 and !Cross: vy -= vy >> 1
  if budget < 4 or vy < 4: start falling with vy = 0
  ```

  - Holding Cross gives 7 ticks at +261, then 188, 133, 92 and 61: an apex 2301 above the floor at tick 12.
  - A tap gives 4 ticks at +261, then halving to 131, 66, 33, 17, 9 and 5: an apex 1305 at tick 11.
  - Any hold of up to 9 frames gives the tap jump.
- **Fall (0x8002D810).** Each tick:

  ```
  y −= vy; vy = min(vy + 8 + (vy >> 2), 576)
  if y ≤ floor: y = floor, on the ground, vy = 0
  ```

  The deltas are 0, 8, 18, 30, 45, 64, 88, 118, 155, 201, 259, 331, 421, 534 and then 576.
- **Ground snap (behaviour script, opcode handler 0x8002AC7C sets Y).** A falling Croc whose height above the floor is under 131 is put on the floor.
  - Landing is then completed by the next fall tick.
  - The bytecode at 0x800DA73B reads `15 03 0300 03 0000 07 04 83000000 0F 12 38 03 0003…`. It is read as "if (field 3 − field 0) < 0x83 then set y = field 3", but the opcode semantics were not decoded.
  - Observations agree: snaps happened at 7, 19 and 29 units above the floor, and none at 223, 249, 319, 371 or 470.

### Measured trajectories (from the state, per tick; full data in `examples/croc-movement/tests/fixtures/`)

The "hold right 60 frames" case is Right for 30 ticks standing. The heading goes 94, 94, 91, 83, 64, 17, 3995, then −63 per tick to 2420.
After release it goes 2404, 2400, 2399. X and Z do not change: Right turns on the spot.

Up for 50 frames (25 ticks), then release:

| tick | 1 | 2 | 3 | 4 | 5 | 6 | 8 | 10 | 15 | 20 | 25 | 26 | 27 | 28 | 30 | 35 | 40+ |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Z | 12322 | 12382 | 12462 | 12556 | 12661 | 12774 | 13018 | 13276 | 13949 | 14635 | 15325 | 15428 | 15505 | 15563 | 15638 | 15710 | 15725–15727 |
| ΔZ | 34 | 60 | 80 | 94 | 105 | 113 | 124 | 130 | 136 | 138 | 138 | 103 | 77 | 58 | 32 | 7 | 1–0 |

Jump, Y per tick (floor −225):

| tick | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 15 | 17 | 19 | 21 | 22 | 23 | 24 | 25 | 26 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| tap (Cross 4 frames) | −225 | 36 | 297 | 558 | 819 | 950 | 1016 | 1049 | 1066 | 1075 | 1080 | 1080 | 1072 | 1024 | 915 | 709 | 353 | 94 | −225 | | | |
| held | −225 | 36 | 297 | 558 | 819 | 1080 | 1341 | 1602 | 1790 | 1923 | 2015 | 2076 | 2076 | 2050 | 1975 | 1823 | 1550 | 1349 | 1090 | 759 | 338 | −225 (snap from −196) |

## The demo and how it was kept idiomatic

`examples/croc-movement` was created with `psx template create`. It has the following parts:

- **State.** Components `Croc`, `Velocity`, `Facing { yaw, turn_rate }`, `Gait`, `Vertical { Grounded | Rising | Falling }` and `Ground`.
- **Systems.** Five systems in `GameSystems::Game`, chained in the original's order. They read `ControllerState`.
- **Units.** Lengths are Croc units, scaled into Bevy units by `units()`. Angles are radians from `turns_4096()`, and yaw is the Transform's rotation about Y. Rates are per 25 Hz tick: the logic rate is `FIELDS_PER_TICK = 2` PAL fields (`src/lib.rs`).
- **Arithmetic.** There is no fixed point, no >>12 velocities and no 16.16 turn rate.
  - Rules are written as what they do, e.g. "ease a quarter of the way to the target velocity" or "turn rate grows ×2.5 up to 63/4096 of a turn".
  - Each constant cites its address.
  - The original's integer truncations are not reproduced. They are the entire error budget of the comparison and are described in the test.
- **Integer port.** It lives outside the game, as `tools/reference_model.py`, because it is evidence of completeness rather than game code.

Verification: `cargo test -- --nocapture` feeds each fixture's per-tick buttons to the systems on a headless `App`. It then compares the result with the recorded trajectory:

| scenario | ticks | path (units) | max position error | final position error | max heading error (1/4096) |
|---|---|---|---|---|---|
| walk (Up 25 ticks, release) | 50 | 3473 | 30.2 | 30.2 | 0.00 |
| turn (Right 30 ticks) | 40 | 0 | 0.0 | 0.0 | 2.02 |
| jump_tap | 35 | 2610 | 28.2 | 0.0 | 0.00 |
| jump_hold | 35 | 4602 | 47.4 | 0.0 | 0.00 |
| back_and_turn (Down, then Right) | 30 | 912 | 19.8 | 19.8 | 2.02 |
| air_strafe (jump + Left, then Right) | 40 | 5057 | 50.4 | 25.9 | 0.00 |
| sidestep (Up + L1) | 14 | 1613 | 10.4 | 10.4 | 0.00 |
| combo (run-jump, run-turn, back, turn, jump, run-turn) | 75 | 10275 | 54.4 | 54.4 | 4.98 |

Floor contact matches on every tick of every scenario. The integer reference model has zero error on all of them.
`game_step` and `game_screenshot` through the dev MCP were checked for walking, turning while walking, and a held jump. The camera follows, and the blob shadow shows height.

## Not modelled / open

- **Slopes and walls.** On the path's slopes, Y follows the terrain. The speed also drops: on the first slope tick, the velocity was about 10 units/tick below the flat-ground rule.
  The collision/terrain pass (0x8004162C, and the script's Y writes) was not analysed.
- **Second Cross press in the air** (`CROSS@0-4;CROSS@10-14` and similar). Croc hangs at the height reached for about 16 ticks and then drops at 384 and then 488 per tick.
  This looks like a stomp. It is script-driven, and its trigger window was not pinned down: a press at frame 36 of a tap jump did nothing.
- **Pressing Cross during the last ticks of a fall** (frame 40 of a tap jump, landing at 45) buffers a jump that starts after landing. The buffer window is unknown.
- **Other branches.** The analog/camera-relative control branch, gait 4/5, the s1 "slow" condition (forward target 4, sidestep 2) and the 3600 jump were not exercised.
- **Hazards.** Falling below the floor (water) and damage bounces were not covered.
  In one run, walking left from (9800, 19700) dropped Croc to y = −716, bounced him at +261 per tick, and later reset him to the level start.

## Toolkit friction

Ordered roughly by how much time each one cost.

1. **No per-frame sampling of RAM variables.** Getting "position per frame" required `--watch` on each variable and then rebuilding values per frame from the write log.
   That only works because the game rewrites the variables every tick. Stores that write the same value (d=0) are logged too, which roughly doubles the noise.
   Suggestion: `psx run --sample ADDR:W[,ADDR:W…] [--sample-every N]`, writing `samples.csv` with one row per frame. Alternatively, `--ram-every N` could dump RAM snapshots.

   *Resolved:* `psx run --backend native --sample ADDR:FMT[:NAME]` (repeatable, `--sample-every N`) writes `samples.csv` (`run_frame,frame,<values>`, one row per vblank, read byte-exact from RAM).
   Walking forward from this study's state is one command: `--replay up.json --frames 60 --sample-every 2 --sample 800D8D8C:i32:x --sample 800D8D94:i32:z` gives z += 34, 60, 80, 94, 105, 113, 120, 124, 128, 130, 132, 134, 135, 136, 136, 137 ... per tick, reaching the measured steady state Z += 138, X += 20 on tick 20.
2. **No RAM search/diff command.** PLAN.md M3 lists `memory.search/diff`. Finding the variables took a hand-written Python diff over 12 `ram.bin` dumps.
   Suggestion: `psx ramdiff run1 run2 … --monotonic/--changed/--constant-in A`, with filters for value range and width.
   Better still would be a `psx search` session that narrows candidates across runs, the way cheat finders do.

   *Resolved:* `psx ram diff A B` (width, range, minimum delta, signed, clusters), `psx ram search RUN...` (per-pair predicates `changed/unchanged/increased/decreased/delta=N`, exact `--values`, `--between`, and `--save`/`--candidates` to narrow over rounds) and `psx ram view` (a struct side by side). Run `--poke` tests a hypothesis by writing memory at a frame.
3. **Frame numbering is inconsistent with `--from-state`.**
   - `--frames`, `--replay` frames and `--trace-from-frame` count from the state.
   - The `frame` in `writes.jsonl` and in `trace.txt` is absolute (5401…, counted from power-on, because the state was saved at 5400).
   - native-core.md says "frames count from the restored state", which is true only for the inputs.
   Suggestion: report both `frame` (run-relative) and `abs_frame`, or document it.

   *Resolved:* every native log keeps `frame` (absolute, so existing scripts still work) and adds `run_frame`; `trace.txt` has a header with the offset; the report has `frames.start`/`frames.end` and a `numbering` note. README "Frame numbers" and native-core.md document it.
4. **`writes.jsonl` uses decimal addresses, PCs and unsigned values.** Every analysis needed hex conversion and sign fixes for s32/s16 values.
   Suggestion: hex strings for `address`/`pc` (as `psx exe`/`disasm` already use), plus a signed value or a `--watch ADDR:W:s` type.

   *Resolved:* `writes.jsonl` lines add `address_hex`, `pc_hex`, `value_hex`/`old_hex` and `value_signed`/`old_signed` (by store width); `--watch-changes-only` drops the d=0 stores.
5. **`psx disasm` prints only JSON** (one object per instruction).
   - For a whole EXE that is 618k lines, and it had to be converted to text for grep.
   - There is no function view: function boundaries had to be found by scanning for `addiu sp, sp, -N`/`jr ra`.
   - Most game code is reached through jump tables and function-pointer tables (behaviour-script handlers at 0x8006B4xx, a switch at 0x80010928), so `jal` cross-references miss callers.
   Suggestions:
   - `--format text`;
   - `--function ADDR`, which disassembles the enclosing function using prologue heuristics;
   - an `xrefs ADDR` view that also scans data words equal to code addresses.

   *Resolved:* `psx disasm --text` (labels, `loc_` branch targets, resolved `lui`/`gp` addresses as `; load 0x…` notes), `--function ADDR` (any address inside; bounds from jal targets, prologues and the final `jr ra`), `--ram RUN` (code from a run's RAM), and `psx xrefs ADDR|LO-HI` (calls, jumps, branches, lui pairs, gp-relative accesses with `--gp`, and data words such as the handler table entry `0x8006B724: .word 0x8004425C`).
   `psx xrefs 800751CC --source CROC.EXE --gp 80074B38` lists the 32 accesses to the gait variable, including the four writers found by watches.
6. **Function ranges for `--trace-range` need hand-made bounds.** Only PsyQ library functions have symbols (20% of the code), so `--trace-range Function` only works for libraries.
   A `psx functions EXE` pass (prologue scan, `jal` targets and pointer-table targets) emitting a `.sym` file would make the trace and disasm commands far easier on game code.

   *Partly resolved:* `psx disasm --function ADDR` prints a function's bounds (`function.start/end`), which `--trace-range` takes as `LO-HI`. A whole-EXE `psx functions` → `.sym` pass is still open.
7. **Worktree workflow.** `bios/`, `media/`, `.psx/` and `external/` live only in the main checkout, so every run must `cd` there and use an absolute path to the worktree binary.
   Suggestion: a `PSX_DATA_DIR` or a `.psx/config` that resolves relative BIOS, media and `.psx` paths, independent of CWD.

   *Resolved:* `psx --root DIR` or `PSX_ROOT=DIR` (also for `psx mcp`): psx works in that directory, so relative paths and `.psx/` outputs resolve there.
8. **Stale help text.** `psx run --help` and the top-level command list say "Run … in a PCSX-Redux worker". For `--backend native` that is misleading: the native backend is now the one with `--trace`, save states and so on.

   *Resolved:* the `run`, `native` and `disasm` help and MCP descriptions describe both backends and point game work at `run --backend native`.
9. **Script-driven behaviour is opaque.** Croc's jump take-off, ground snap and stomp live in a bytecode VM (dispatchers at 0x8002AC0C and 0x8002F168, with opcode tables at 0x80010928/0x80010CE4).
   The trace shows the dispatch, but nothing in the toolkit helps decode a game's VM. An `--trace-range` on the dispatcher plus a "bytes read from range X" log would help.
   For example, `--watch-reads LO-HI` would have shown which script bytes each opcode consumed. Read watches do not exist at all; only write watches do.

   *Partly resolved:* `psx run --backend native --watch-reads LO-HI|ADDR[:W]` logs CPU loads to `reads.jsonl` (pc, address, value, frames) and lists the top reader PCs. Decoding a VM is still manual.
10. **Template nudges.**
    - **Fixed rate.** The template fixes `FixedUpdate` at the field rate (50/60 Hz) and suggests "count ticks with `GameClock.frame`" for half-rate games.
      Doing that makes `ControllerState::pressed` edges happen on 50 Hz ticks, so a press can land on a skipped tick and be lost. The demo sets the fixed rate to `region.hz() / 2` instead.
      Suggestion: the template README should recommend changing the fixed rate to the game's logic rate.
      *Resolved in the template:* `TickRate` (from `REGION` and `FIELDS_PER_TICK` in `lib.rs`) sets the logic rate apart from the display, and input is sampled every rendered frame and latched until the next tick, so a short tap is not lost.
    - **Dead code.** Removing `demo.rs` makes `ControllerState::released/direction` and `Region::hz` dead code, which gives clippy warnings in a fresh game. The demo silenced them with `#[allow(dead_code)]` on the template API.
      *Resolved:* the template is a library (`game`) plus a thin binary, so its public API is never dead code; a copy without `src/demo/` builds and passes clippy with no warnings.
    - **Shader warning.** Startup logs a WARN that `psx_material.wesl` has an unresolved import `bevy_pbr::render::view_transformations` (llvmpipe). Rendering is fine, but the warning looks like an error to an agent.
      *Resolved:* Bevy logs this when a WESL shader is compiled before a library it imports has finished loading (it then retries). The template now adds its shaders only once every imported module is loaded (`psx/shaders.rs`). The warning could not be reproduced on demand afterwards, so the fix is tested by unit tests of the import check and by clean startup logs, not by a before/after comparison.
    - **No positions over MCP.** The dev MCP has no way to read game state. `game_status` shows only frame and buttons, so trajectories cannot be compared through MCP.
      A generic `game_query` (e.g. the Transform of entities with a marker, or a registered debug resource) would make MCP-side numeric checks possible. The demo compares in a unit test instead.
      *Resolved:* `game_query` returns reflected components of entities by `Name` or marker, and resources, as JSON. The example declares its components `Reflect`. Tests can also live in `tests/` now, using `game::testing` to run exact ticks headless.
11. **Emulator-like pull.** All the evidence arrives as fixed-point integers with shifts: `v - (v>>2) + target`, 16.16 turn rates, `>>12`. The quickest path is to transliterate it, as the reference model does.
    Nothing in the template or its docs suggests a conversion layer (units, angle helpers) or warns that bit-exact integer behaviour is usually not the goal.
    A short "from evidence to idiomatic rules" section in the template's AGENTS.md would help. It should cover converting fixed-point to natural units, keeping a literal model in a separate tool as evidence, and testing with tolerances.
    *Resolved:* the template's AGENTS.md has "From evidence to idiomatic code" (README has a summary), with this study's velocity blend as an example.

## Files

- `examples/croc-movement/`: the Bevy demo (`src/croc/movement.rs` rules, `scene.rs`, `verify.rs`), fixtures, `tools/record_trajectory.py` and `tools/reference_model.py`.
- `crates/psx-gte/src/math.rs`: doc update. The quarter-wave sine table read by PsyQ 4.0 `rsin` in CROC.EXE (0x8006D5D4, 1025 entries) equals `round(4096·sin)` for every entry, which confirms psx-gte's `rsin`/`rcos`.

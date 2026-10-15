# Case study: Mega Man 8 (PAL), from a captured stage to a rebuilt walk and jump

A usability study of the toolkit on a 2D game. The aim was to get Mega Man 8
(SLES-01064, SCPH-7502 BIOS) into its intro stage, capture a frame into the
Bevy template, reverse engineer Mega Man's walk and jump from RAM and code,
and rebuild them as idiomatic Bevy systems verified against the original.
The result is [`examples/megaman8-movement`](../examples/megaman8-movement/README.md).

Summary:

- **Movement:** the rebuilt walk and jump match the original exactly over 382
  ticks of recorded input in 7 scenarios, one of them held out. Positions match
  to 0 px, and the state and camera scroll match on every tick.
- **Captured frame in the template:** 99.33 % of pixels match exactly (MAD 0.41 on
  0–255). Every differing pixel is in Mega Man's mirrored sprite, which the
  scene exporter shifts by one texel.
- **Rebuilt frame:** the demo's frame (captured backdrop with Mega Man hidden,
  plus our own player entity drawn with the corrected mirror rule) matches the
  emulator's frame on 100.00 % of pixels at rest.
- **Friction:** the main deliverable, at the end. The biggest gaps are per-tick
  value sampling, RAM search and poke, a text disassembly, multi-frame sprite
  extraction, and any path to level data that is not a screen capture.

All runs used the native backend from the main checkout, with outputs in `.psx/mm8-study/`. Paths below are relative to that directory.

## 1. Into the stage

An earlier replay (START then CROSS every 120 frames from 2800) reached the
stage at about frame 7000 but also opened the weapon menu. A contact sheet of
`psx native --snapshot-every 200` (36 screens) showed the timeline:

| Frames | Screen |
|---|---|
| 400–1400 | Sony, PlayStation and piracy screens |
| 1800–2000 | Capcom and Laguna logos |
| 2400–2800 | Intro FMV (START at 2800 skips it) |
| 3000 | Title |
| 3400 | "Now loading" |
| 3600 | Stage, READY |
| 3800 | Mega Man standing |

The weapon menu opened only because the old replay kept pressing START in the
stage. The replay now pulses START at f and CROSS at f+60 for f = 2800, 2920,
3040, 3160, with no presses after 3300:

```python
for f in range(2800, 3300, 120):
    ev += [{"frame": f, "buttons": ["START"]}, {"frame": f + 5, "buttons": []},
           {"frame": f + 60, "buttons": ["CROSS"]}, {"frame": f + 65, "buttons": []}]
```

`psx run --backend native ... --frames 3900 --replay boot.json --save-state`
gives `s3900/state.bin`, with Mega Man idle in the intro stage. Every later
experiment starts from this state.

## 2. Capture → template

```sh
psx run --backend native ... --from-state s3900/state.bin --frames 2 --gpu-frame 1 --out cap2
psx textures cap2       # 29 page/CLUT pairs + textures/scene.psxscene (594 elements)
```

The frame has 706 GP0 commands:
- 586 textured 16×16 `rect`s: the background tiles and the HUD;
- 7 `poly4`: Mega Man's six 16×16 pieces, drawn mirrored, plus the health bar;
- 2 CLUT uploads;
- 5 texture pages, all 4-bit.

It draws into the buffer at y = 240, and `screen.png` is identical to both
240-line halves of `gpu_frame_after.png` because the scene is static.

In a template project, `cargo run --features dev-mcp -- --mcp --scene captures/mm8/scene.psxscene`
was stepped and screenshotted over the dev MCP (`mcp_drive.py`, adapted from the example driver):

| Comparison (320×240, best shift within ±4 px) | MAD (0–255) | Exact pixels |
|---|---|---|
| Template scene viewer vs emulator `screen.png` | 0.414 | 99.33 % |
| Demo at rest (backdrop, captured Mega Man hidden, our sprite) vs emulator | 0.000 | 100.00 % |
| Demo 13 ticks into a held jump vs emulator | 0.794 | 98.94 % |

The best shift was (0, 0) in every case. The causes of the differences:

- **Scene viewer (516 px, all inside Mega Man).** The game draws Mega Man as
  `poly4` quads with u decreasing left to right (a mirrored sprite). The PS1
  samples a pixel's texel at its left/top edge, so a mirrored quad spanning u
  15→31 over x 148→164 shows texels 31..16. Bevy samples at the pixel centre,
  which gives texels 30..15: a one-texel shift. Shots: `shots/mm_zoom.png`
  (emulator left, template right) and `shots/diff_tpl.png`. The demo draws these
  pieces as `Sprite`s with `flip_x` and the rect moved one texel
  (`src/player/sprite.rs`), which is exact.
- **Mid-jump.** The differing pixels are confined to x 145–176, y 115–163,
  which is Mega Man. The demo has only the standing pose, while the original
  shows its jump pose. Everything else, including the position, is identical.
  See `shots/side_j13.png`.

## 3. Reverse engineering the movement

### Finding the player

The first experiments were four 40-frame runs from the state: idle, RIGHT for 20, RIGHT for 40, and LEFT for 20. I diffed their `ram.bin` with a small
script (`ramdiff.py`), keeping halfwords that order as LEFT20 < idle < RIGHT20 < RIGHT40.
Three addresses matched: 0x8015E8AA, 0x8015E8D0 and 0x801C7A46. A side-by-side
dump around the first showed a 16.16 x at 0x8015E8A8, y at 0x8015E8AC and
velocities after them. `--watch` on those words gave the writer PCs,
`psx disasm` gave the code, and one `--trace --trace-range` window over the
jump confirmed the order of operations with live values.

The player object starts at **0x8015E89C**. It is handled by a state machine
dispatched once per tick from 0x8010C58C. The state byte is at +0x02.

### RAM map

| Address | Field | Meaning | Evidence |
|---|---|---|---|
| 0x8015E89E (+0x02) | u8 | State: 0 stand, 1 step, 2 walk, 3 jump, 4 fall, 5 land, 6 ladder, 10 slide, 11 (ladder variant) | Watch: 0→3 by 0x8010F8F0 on a jump press, 3→4 by 0x8010CA24 at the apex, 4→5 by 0x8010CAC0 on landing, 5→0 by 0x8010CB54. The ground state's code sets 1/2 (0x8010C704). 6, 10 and 11 come from the code only (0x80110F64, 0x8010C774, 0x80110F50) |
| 0x8015E89F (+0x03) | u8 | Substate: 0 = state just entered | Every state function tests it first (e.g. 0x8010C988) |
| 0x8015E8A8 (+0x0C) | s16.16 | x, stage pixels | Moves +1.5/tick when walking right. The integer half is the sprite centre column |
| 0x8015E8AC (+0x10) | s16.16 | y, stage pixels, down positive | 441.0 standing. Decreases while jumping |
| 0x8015E8B0 (+0x14) | s16.16 | vx | 0x00018000 / 0xFFFE8000 (0x8010C914, 0x8010F6F0) |
| 0x8015E8B4 (+0x18) | s16.16 | vy, up positive | 0x0005C900 at a jump (0x8010C9C4) |
| 0x8015E8B8 (+0x1C) | s16.16 | ax | Zeroed by air control (0x8010F6D0) |
| 0x8015E8BC (+0x20) | s16.16 | Gravity in use | Copied from +0x54 by the air states |
| 0x8015E8C1 (+0x25) | u8 | Facing: non-zero = right | Picks the step nudge sign at 0x8010C7D4 |
| 0x8015E8EC (+0x50) | s16.16 | Maximum fall speed, 0xFFFA0000 = −6.0 | Compared and stored at 0x8010F7D0–F7E4 |
| 0x8015E8F0 (+0x54) | s16.16 | Gravity, 0x4000 = 0.25 | RAM value. Read at 0x8010F70C |
| 0x8015E8F4 (+0x58) | u16 | Held buttons in the game's layout: the pad word byte-swapped, so Up 0x1000, Right 0x2000, Down 0x4000, Left 0x8000, Circle 0x20, Cross 0x40 | 0x2000 while RIGHT is held. Jump tests the mask 0x60 |
| 0x8015E8F8 (+0x5C) | u16 | Newly pressed buttons | The jump check reads +0x5C & 0x60 (0x8010F8DC) |
| 0x8015E902 (+0x66) | u8 | Collision flags: bit 7 grounded, bit 6 ceiling | 0x80 while standing. Cleared the tick after take-off and set by 0x8010C054 the tick after a landing snap |
| 0x8015E90C (+0x70) | u8 | Step timer | Set to 5 at 0x8010C7D8 |
| 0x8015E926 (+0x8A) | u8 | High-jump flag: when set, the jump speed is 0x000669D8 = 6.41 | 0x8010C9A8. Cleared on a normal jump (0x8010F8FC). Its meaning is **unverified** (a spring or upgrade?) |
| 0x8015E931 (+0x95) | u8 | Walk grace timer | Set to 5 on stopping or landing. A direction pressed while it is non-zero skips the step |
| 0x8016F26C | s16 | Camera x (left edge of the view) | `max(256, floor(x) − 160)` on every recorded tick. Copied from 0x801D2FC6 by 0x80101218 |
| 0x8016F270 | s16 | Camera y | 256 in this section, unchanged by jumps |

The sprite centre is (x − cam_x, y − cam_y): (160, 185) at the start, which
matches the bounding box of the drawn quads (132–180, 161–209).

### Mechanics

All values are per tick (one PAL field, 50 Hz) in pixels. y is down and vy is
up. The integration routine is 0x80105018:
`vx += ax; vy -= g; x += vx; y -= vy`. The ground variant, 0x80104FF4, has no
gravity.

- **Stand (0x8010C67C).**
  - The grace timer counts down.
  - A held direction sets the state to walk if grace > 0, else to step.
  - A jump press while grounded sets the jump state (0x8010F8C8). Without the grounded flag, the state becomes fall (0x8010F908).
  - State changes take effect next tick.
- **Step (0x8010C798).** The step happens on entry:
  - x moves 1 px toward the facing (0x8010C7F4 writes only the integer half);
  - the timer is set to 5, then decremented each tick, and the state becomes walk at 0;
  - releasing the direction returns to stand.

  A walk therefore starts with +1 px, waits 4 ticks, and then moves 1.5 px/tick.
- **Walk (0x8010C89C).**
  - vx = ±1.5, with LEFT tested first, and x += vx.
  - On release: stand, grace = 5, no move that tick.
- **Jump (0x8010C978 → 0x8010F6FC).** On entry, vy = 5.78515625 (6.4134 with the high-jump flag). Then, every tick including the first:
  1. g = 0.25;
  2. air control: vx = ±1.5 or 0, so there is no horizontal inertia;
  3. integrate.

  After moving:
  - if jump (Cross or Circle) is not held, vy = −1/65536 and then 0;
  - if vy < 0, vy = 0;
  - a ceiling contact also sets vy = 0.

  In each of these cases the state becomes fall. So **releasing jump cuts the rise immediately**: vy goes to 0 and the fall starts from rest.
  A full jump rises for 23 ticks, by Σ_{k=1..23}(5.78515625 − 0.25k) = 64.0586 px (441 → 376.94).
- **Fall (0x8010CA58 → 0x8010F790).**
  - Gravity, air control and integration are the same as in the jump.
  - After moving, vy = max(vy, −6.0): the clamp comes after the move.
  - If the grounded flag is set: walk if a direction is held, else land with grace = 5.
- **Floor collision.** When a move sinks into the floor, only the integer part of y is restored to 441, by 0x80106E40, a halfword store; the sub-pixel part is kept. That is why a landing leaves y = 441.15625 or 441.796875.
  - The grounded flag appears on the tick after the snap.
  - Sinking happens only when the integer part of y passes 441: y = 441.047 does not land.
- **Land (0x8010CB08).**
  - On entry vy = 0. The state otherwise behaves like stand.
  - The state returns to stand when the landing animation ends: 11 ticks after the landing tick, measured.
- **Camera.** cam_x = max(256, ⌊x⌋ − 160) after the move.

Per second, the speeds are: walk 75 px/s, jump 289.3 px/s, gravity 625 px/s² and maximum fall 300 px/s. Whether the PAL release rescaled these from the NTSC version was not checked.

### Verification

The demo's `cargo test -- --nocapture` replays the same inputs into the Bevy
systems and compares every tick with the original. The original's trajectories
come from `--watch` hits on the player object, applied to the state's RAM; see
`tools/mm8_trajectories.py`.

| Scenario | Input (tick:buttons) | Ticks | Max position error | States / camera |
|---|---|---|---|---|
| walk | `0:RIGHT 30:` | 40 | 0 px | identical |
| walk_regrab | `0:RIGHT 12: 14:RIGHT 30:` (grace) | 40 | 0 px | identical |
| walk_left | `0:LEFT 20:` (camera clamp) | 30 | 0 px | identical |
| jump_hold | `0:CROSS` | 62 | 0 px | identical |
| jump_tap | `0:CROSS 5:` (cut jump) | 40 | 0 px | identical |
| run_jump | `0:RIGHT 10:RIGHT+CROSS 30:RIGHT 50:` | 70 | 0 px | identical |
| mixed (held out) | `0:LEFT 3:LEFT+CROSS 8:LEFT 20:RIGHT 40: 44:CROSS 60: 61:CIRCLE+RIGHT 90:` | 100 | 0 px | identical |

`mixed` was recorded after the model was written and was never used to fit it.
It covers a cut jump with air control, landing while a direction is held (straight to walk), a jump on Circle, and reversing direction.
Error is exactly 0 because the constants are dyadic: every value is a multiple of 1/256, which f32 represents exactly at these magnitudes.

The `vx` column is **not** compared. The original leaves a stale vx in RAM after stopping, while the demo zeroes it. That is a representation difference, not a behaviour difference.

**Not verified by trajectory:**
- the −6 px/tick clamp: falls here reach −6.0 only on the landing tick. The clamp is from the code (0x8010F7CC);
- the high-jump flag;
- ceiling hits;
- slopes and walls;
- the facing-left sprite layout: the rig is mirrored about the origin, which was never captured;
- camera behaviour beyond this section.

## 4. The rebuild (`examples/megaman8-movement`)

The demo is structured as a reimplementation would be, not as an emulator:

- **`Player` entity.** Mega Man is one entity. Its components are `StagePos` (stage pixels, y down, as in the level data), `Velocity`, `Facing`,
  `PlayerState` (an enum: `Stand`, `Step{ticks_left}`, `Walk`,
  `Jump{launched}`, `Fall`, `Land{ticks_left}`), `Grounded` and `WalkGrace`.
- **Pipeline.** One chained pipeline runs in `GameSystems::Game`, reading `ControllerState`:
  `control → integrate → after_move → collide_with_floor`, then the camera follow.
  It mirrors where the original makes its decisions: before the move, the move, after the move, then collision. Each rule carries a comment with its code address.
- **Units.** Constants are named and in px/tick at the 50 Hz PAL tick (`Region::Pal`), with px/s given in the docs. Exactness depends on the per-tick values.
- **Stage.** `Solid` floor rectangles, a `StageCamera` resource, and a sync
  system that maps stage space to `Transform` with y flipped.
- **Assets.** There are no game assets. The demo uses a placeholder sprite and floor. If the user's own `psx textures` output is in `assets/captures/mm8/`, the demo instead shows the captured frame as a temporary backdrop. That backdrop is pinned at the capture's camera, with Mega Man's six GPU commands hidden through `PsxSceneElement::command`, and his standing pose is cut from the decoded page.

Screenshots (main checkout `.psx/mm8-study/shots/`):
- `emu_capture_frame.png`: the emulator;
- `scene_tpl.png`: the template viewer;
- `demo_idle.png` / `demo_j13.png` / `demo_r.png`: the demo at rest, mid-jump and walking (the backdrop runs out at the right because it is one screen);
- `demo_placeholder.png`: no assets;
- `boot_sheet.png`: the boot timeline.

## 5. Rebuilding the stage properly

The captured scene is scaffolding. It is one screen of draw calls in screen
space with the parallax already flattened, and it says nothing about collision.
A real rebuild of the intro stage would:

1. **Find the tilemap in RAM.** The background is drawn as 16×16 `rect`s
   from four-bit pages at (512–640, 256): 586 of them in this frame. Trace the
   code that writes those GP0 packets with `--trace` over the ordering-table
   build and `--watch` on a packet. That leads back to the source arrays: a
   tile index grid per layer, probably with per-tile attributes, collision
   among them.
2. **Map the arrays to disc files.** Run `--trace-cd` over the stage load and
   `psx loads` to find the stage file and its offset. Then find the
   decompressor (MM8 data is likely compressed) by tracing the writes into the
   tilemap's RAM.
3. **Decode the formats into plain data:**
   - a tileset PNG per CLUT;
   - layer grids, including the parallax layers and their scroll ratios (which come from the camera code at 0x801D2FC6);
   - a collision layer;
   - entity spawn lists.
   Load these into Bevy as tile layers (e.g. `bevy_ecs_tilemap` or plain sprite chunks) and `Solid`/slope colliders. Enemies and pickups become their own entities with their own systems.
4. **Use the capture only to check the result.** Place the rebuilt camera at (256, 256), render, and diff against
   `screen.png`, as in section 2. The bar is 100 % exact pixels except for sprites whose animation is not yet implemented.

Toolkit support that would make this practical:
- **GP0 provenance:** for each command in `gpu_frame.json`, the RAM address the DMA read it from and the PC that wrote it;
- **Tile extraction:** cut a frame's rect tiles into a deduplicated tileset plus an index grid per draw layer, to check against the decoded map;
- **Reverse `psx loads`:** given a RAM range, report the disc file and offset it came from;
- **Struct views:** "dump this address as this layout across frames";
- **Multi-frame VRAM upload capture:** for sprite sheets (see friction).

## 6. Toolkit friction

The items are ordered roughly by how much time they cost. The suggestions are concrete and mostly small.

### Running and inputs

1. **Data lives only in the main checkout.** `bios/`, `media/`, `.psx/` and
   `external/` are gitignored, so a worktree build must run with CWD in the
   main checkout. Every command needed a wrapper script. The worktree sandbox
   also refused `cd main && worktree/psx ...` as "too complex", so I had to
   write a wrapper file before any run worked.
   *Suggest:* a `PSX_HOME` (or `psx.toml`) for the data and output roots, and
   absolute paths in error messages.
2. **Two overlapping run commands.** `psx native` has `--snapshot-every`
   (which is how I found the stage) but no `--save-state`. `psx run --backend native` has
   `--save-state`, `--gpu-frame` and `--trace` but no snapshots. Their flags also differ (`--disc` against a positional
   argument, `--watch ADDR` against `--watch ADDR:W`). So the boot ran twice: once to see where the
   stage starts, once to save a state.
   *Suggest:* fold snapshots into `psx run` (`--screens-every N`) and add
   `--save-state-at F` (repeatable), so one boot yields states at several points.
3. **No known-good boot replays.** native-core.md says Mega Man 8 reached the
   "Intro stage, weapon menu" with a START/CROSS replay, but the replay isn't in the repo.
   *Suggest:* `tools/replays/<game>.json` with the frame to save at. Mine is in
   section 1 and took one 60 s run plus a contact sheet to derive.
4. **The replay format is verbose.** It is a JSON array of whole-pad states. For 20+ experiments I wrote a
   small DSL (`"0:RIGHT 12: 14:RIGHT+CROSS"`) and a wrapper that writes the
   JSON.
   *Suggest:* accept that inline (`--input "0:RIGHT 30:"`), plus a pulse form
   (`2800:START/5` meaning held for 5 frames).
   Button-name case also differs between components: the replay uses `CROSS`, and the template's MCP uses `cross`.
5. **Mixed frame numbering.** After `--from-state`, replay frames count
   from the run start, but `writes.jsonl` `frame` and `trace.txt` print absolute
   frames (3901, …). Every script had to subtract the state's frame, which it
   can only learn from the first write.
   *Suggest:* emit both `frame` and `run_frame`. Also put `start_frame` in the report, which currently gives only `frames.reached`, without the base.
6. **Reports are long.** `psx native` printed about 300 lines of JSON (SPU voices,
   hot PCs, I/O counts) for a run whose only purpose was screenshots.
   *Suggest:* `--quiet` or a one-line summary, with the full report in the run
   directory.

### RAM analysis

7. **Watch hits can't be turned into a trajectory directly.** `--watch` is
   excellent for finding writers. But each hit is keyed by the store's own
   address and width: a halfword store into the high half of a watched 32-bit
   word (the landing snap at 0x80106E40) appears under 0x8015E8AE, not
   0x8015E8AC. My first per-address trajectory script therefore showed Mega Man
   landing at y = 447 and staying there. Correct values per tick needed a byte
   shadow of RAM, seeded from the state's `ram.bin` (which needs a separate
   `--frames 0` run) with every hit applied in order.
   *Suggest:* `--sample ADDR:fmt[,…]` writing `samples.csv`, with one row per
   frame of end-of-frame values (fmt: `u8`, `s16`, `fx16.16`). This is the single most useful missing feature for gameplay RE.
8. **No RAM search across runs.** `ramdiff.py` (find words whose values order as
   a hypothesis across N runs) found the player in one try. This is the
   classic cheat-search loop, and every agent will rewrite it.
   *Suggest:* `psx ramsearch run_a run_b ... --order lt|eq|changed --width 2|4`,
   aware of 16.16 values.
9. **No RAM poke.** I couldn't force the high-jump flag (+0x8A) or drop Mega Man from a
   height to exercise the −6 clamp, so both remain code-only claims.
   *Suggest:* `--poke ADDR=VAL[@frame]` (repeatable), recorded in the report.
10. **No struct view.** I dumped the object region side by side with a script (`dump.py`).
    *Suggest:* `psx ram run_a run_b --addr X --len N --as s16|fx`, marking differences.

### Code reading

11. **`psx disasm` is JSON only.** One instruction is seven lines of JSON, so a
    170-instruction state function is about 1200 lines. I piped it through a formatter
    (`dis.sh`).
    *Suggest:* text by default (`addr word asm` with labels), `--json` on demand.
12. **`disasm` reads the EXE on disc, not the run's RAM.** Here the code was
    resident and unmodified, so it was fine. But overlays, or code loaded after boot, would
    be disassembled from the wrong bytes without a warning.
    *Suggest:* `psx disasm <run_dir> --addr …` reading `ram.bin`, and a check that
    warns when the EXE and RAM bytes differ.
13. **No callers or xrefs.** To find the state dispatcher I relied on the trace's
    `-> target (from site)` lines.
    *Suggest:* `psx xrefs <exe> ADDR` (static jal/j targets), or callers from `calls.json`.
14. **The trace is the best tool here.** `--trace --trace-range` from a state, with
    per-instruction effects (`[8015e8b4]=0005c900`), confirmed the integration
    order and the release rule in one 3-frame run. A user label file
    (`ADDR name`) feeding `--symbols`, so traces and disasm show
    `player_jump_state` rather than raw addresses, would make iterating nicer.
    `.sym` input exists but isn't documented as a way to add your own names.
15. **`psx symbols` covers libraries only.** It named 417 library functions (8.5 % of code), none in game code,
    which is expected. It took about 1 s and needs `external/` from the main checkout (item 1).

### Capture, textures and scene

16. **`psx textures` decodes the run's final VRAM, not the captured frame's.** Mega Man 8
    uploads Mega Man's current animation frame to VRAM every tick. With `--frames 3 --gpu-frame 1`, the decoded page would come from a
    later frame. I re-ran with `--frames 2`.
    *Suggest:* save `gpu_frame_after.bin` and decode from it by default (the note
    "decoded from the run's final VRAM" is easy to miss).
17. **Mirrored quads are off by one texel in the scene.** Mirrored `poly4` sprites are exported as `Mesh2d`; GPU
    centre sampling then shifts them by one texel against the PS1. This accounts for all 516 differing pixels in the viewer comparison.
    *Suggest:* in the scene exporter, turn axis-aligned 1:1 quads into
    `Sprite`s with `flip_x`/`flip_y` and the corrected rect (u_lo + 1 when mirrored), as
    the demo does. That would take the viewer from 99.33 % to 100 % here.
18. **A capture holds one pose.** Because sprites are streamed into VRAM per frame, a
    single capture contains only the current animation frame of each actor.
    *Suggest:* `--gpu-frames A..B` or `psx sprites`: record every VRAM upload
    over a window and deduplicate it into a sprite sheet with the CLUTs that sampled
    each upload. Better still, find the sprite data on disc.
19. **The scene is screen-space only.** There is no camera offset and no layer
    separation, so as a backdrop it covers exactly one screen. It scrolls rigidly
    and runs out after a few dozen pixels of walking (`shots/demo_r.png`).
    *Suggest:* let `psx textures` take a camera (`--camera 0x8016F26C`) so the
    scene records its stage-space origin. Group elements into layers by texture
    page/CLUT/draw-order runs; parallax layers could then scroll independently, and
    HUD elements (here commands 668+ are the player, and the health bar is a
    `poly4` at the end) could be split off.
20. **Which buffer `screen.png` shows was undocumented.** The capture draws into y = 240, and the
    report's `display.start` is the other buffer. They were identical here, but
    I had to check both halves of `gpu_frame_after.png`.
    *Suggest:* write `gpu_frame_screen.png`, the captured frame's draw area.

### Template

21. **Removing `demo.rs` leaves dead-code warnings.** Doing what the README says ("replace `src/demo.rs`") leaves
    `PsxMaterial::textured/color` and `ControllerState::released/direction`
    unused, so clippy warns. I added `#[allow(dead_code)]` on the template modules.
    *Suggest:* make the template a lib + bin, or mark its library modules `allow(dead_code)`.
    *Resolved:* the template is now a library (`game`) plus a thin binary; removing `src/demo/` leaves no warnings.
22. **The template is a bin crate.** Game logic can't be reached from `tests/` integration tests, so
    trajectory tests live in `#[cfg(test)]` modules inside the bin. That works, but
    a lib would allow `tests/` and examples.
    Running systems headless was easy: `App::new()`, `configure_sets`, plugin, then `run_schedule(FixedUpdate)` per tick. This is worth documenting as *the* way to test gameplay against recorded trajectories.
    *Resolved:* `tests/` can reach every module, and `game::testing::{headless_app, TickApp}` runs exact ticks with scripted buttons (`tests/player.rs` is the pattern), documented in README and AGENTS.md.
23. **The region is a hard-coded NTSC default.** `main.rs` hard-codes `Region::Ntsc`, and Mega Man 8 PAL needs 50 Hz for
    per-tick constants to mean the same thing.
    *Suggest:* `psx template create --region pal` (or take it from the disc's region).
    *Partly resolved:* the region is one constant, `REGION` in `src/lib.rs`, and `--region pal|ntsc` overrides it at run time. `psx template create --region` does not exist yet.
24. **Screenshots need a hand-written MCP client.** The dev MCP worked first time, and step-then-screenshot is deterministic. But
    each agent needs its own JSON-RPC client.
    *Suggest:* ship a `--script plan.json` CLI mode (steps, inputs and screenshots, then exit), or the
    generic driver as `tools/mcp_drive.py`.
    *Open.* (`game_query` now covers numeric checks without screenshots.)
25. **The template isn't rustfmt-clean.** There is no `rustfmt.toml`, and `cargo fmt --check` reports
    diffs across the template's own files (long lines). Agents that run fmt will reformat the whole copy.
    *Resolved:* `rustfmt.toml` (120 columns) and the template is `cargo fmt --check` clean.
26. **Bevy build times.** The first dev-mcp build took 26 s with the warm shared target.
    `cargo build --release` took 8 min, and two clippy passes took about 10 min under
    contention on the shared target dir. That is fine, but worth knowing.
    *Addressed:* release rebuilds all of Bevy in its own profile, which is inherent. The dev profile already builds dependencies at `opt-level = 3`, so the README now says to iterate with `cargo build`/`cargo run` and keep `--release` for shipping; dev debug info is cut to line tables (none for dependencies) to shorten links and shrink the target dir.

### Nudges toward emulator-like code

27. **The capture path is the easiest route, and it is the wrong one.** It turns a frame into "a pixel-faithful reference layout, then
    replace pieces". That is the easiest thing to build on, and the scene is a flat
    list of GPU draws (`rect`, `poly4`, command indices). An agent under time
    pressure will treat it as the level: that is what the backdrop is in this demo,
    knowingly. AGENTS.md says "reference data, not an engine", which helps, but
    no toolkit path leads to real level data (tilemaps, collision, spawns), so
    the capture is the only level-shaped thing available.
    *Suggest:* the provenance and tile tools of section 5, and a README
    example that uses the capture only in a test (render, then diff).
    *Partly addressed:* the template README and AGENTS.md now call captures reference data, not levels, and describe rebuilding stages from decoded tile, collision and spawn data. The toolkit support (provenance, tile extraction) and a render-then-diff test example are still open.
28. **Exact constants pull toward per-tick units.** Matching the original exactly needs per-tick fixed-point
    constants and the original's order of operations (move, then cut the rise; clamp
    after the move; keep the sub-pixel on landing). I kept these as named
    constants and an ordinary system pipeline, not a CPU-like loop. But the pull
    toward "replicate the state byte and substate" is real: `PlayerState` here
    mirrors the original's states one to one, because that was the only way to be
    tick-exact. A freer design (seconds, analytic jump curves) would drift by
    sub-pixels per tick. The template could say explicitly which is preferred:
    a fixed-tick simulation with per-tick constants is legitimate for a 2D action
    game.
    *Resolved:* AGENTS.md, "From evidence to idiomatic code", says when per-tick constants are the right choice, using this study's 16.16 jump speed as the example.
29. **`Region` changes the tick rate and therefore behaviour.** It is a rendering setting in the
    template (`PsxScreen`), but it sets the fixed tick rate too.
    *Suggest:* keep the simulation rate a game-level decision, separate from the display.
    *Resolved:* `TickRate` (from `FIELDS_PER_TICK`) is separate from `PsxScreen::region`.

### What worked well

- `--from-state` runs of 40–100 frames take 0.4–0.8 s, which makes
  hypothesis → run → diff loops cheap.
- `--watch` writer PCs plus `--trace-range` traces answered every "who and how"
  question quickly.
- `psx textures` and the scene viewer gave a near pixel-exact frame in two commands.
  `PsxSceneElement::command` made it trivial to hide the captured player.
- The template's fixed-step `GameSystems::Game` and `ControllerState` edge
  semantics matched the original's per-tick model with no adaptation.

## 7. Files and reproduction

- `examples/megaman8-movement/`: the demo, tests, recorded trajectories and the re-recording tool.
- Main checkout `.psx/mm8-study/` (not committed):
  - `boot.json`, `s3900/` (state), `cap2/` (capture + textures);
  - `r/*` (experiment runs), `shots/`, `sym/`;
  - the helper scripts: `mmrun.py`, `ramdiff.py`, `dump.py`, `traj.py`, `dis.sh`, `cmp.py`, `bbox.py`, `mcp_drive.py`.

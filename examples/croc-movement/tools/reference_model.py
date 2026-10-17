#!/usr/bin/env python3
"""Integer reference model of Croc's movement, checked against the fixtures.

This is *evidence*, not the game: a literal port of the integer arithmetic in
CROC.EXE (PAL) that the case study recovered, used to show the rule set is
complete for flat ground. It reproduces every fixture in game/tests/fixtures/
exactly (0 units, 0 angle steps). The Bevy demo (game/src/croc/movement.rs)
expresses the same rules in natural units and floats instead.

    python3 tools/reference_model.py game/tests/fixtures/*.csv

Variables and functions (player object at 0x800D8A1C, gp = 0x80074B38):
  x/y/z            object+0x370/0x374/0x378     heading  object+0x36A (4096/turn)
  vx/vz (x4096)    gp+0x1D0/0x1D4                turn rate gp+0x698 (16.16)
  gait             gp+0x694 (0 idle, 1 starting, 2 walking, 3 backing)
  rise/fall speed  object+0x150                  rise budget object+0x14C
  floor height     object+0x38
  rise 0x8002D8D4, fall 0x8002D810, walk 0x80043234, turn 0x8004425C,
  jump start 0x80028688 (behaviour script), ground snap: script opcode 0x8002AC7C.
"""
import math
import sys


def rsin(a):
    # libgte rsin/rcos (4096 = 1.0). Rounding sin*4096 matched every angle seen.
    return int(round(math.sin((a & 0xFFF) * 2 * math.pi / 4096) * 4096))


def rcos(a):
    return int(round(math.cos((a & 0xFFF) * 2 * math.pi / 4096) * 4096))


class Croc:
    def __init__(s, x, y, z, heading, floor):
        s.x, s.y, s.z, s.head, s.floor = x, y, z, heading, floor
        s.vx = s.vz = s.omega = s.mode = s.walk = s.vy = s.budget = 0
        s.on_ground, s.rising, s.prev_cross = True, False, False

    def tick(s, b):
        up, down, left, right, cross, l1, r1 = (k in b for k in ("UP", "DOWN", "LEFT", "RIGHT", "CROSS", "L1", "R1"))
        # 1. vertical: rise 0x8002D8D4 / fall 0x8002D810
        if s.rising:
            s.y += s.vy
            s.budget -= s.vy
            if s.budget < 700:
                s.vy = s.vy - 8 - (s.vy >> 2)
            if s.budget < 1400 and not cross:
                s.vy -= s.vy >> 1
            if s.budget < 4 or s.vy < 4:
                s.budget = s.vy = 0
                s.rising = False
        elif not s.on_ground:
            s.y -= s.vy
            s.vy = min(s.vy + 8 + (s.vy >> 2), 576)
            if s.y <= s.floor:
                s.y, s.on_ground, s.vy = s.floor, True, 0
        # 2. walking: 0x80043234 (sidestep tests the *previous* gait)
        s2 = 0
        if not s.on_ground:
            s2 = -2 if right else 2 if left else 0
        elif s.mode != 0:
            s2 = -3 if r1 else 3 if l1 else 0
        if up:
            s.mode = 2 if s.walk == 5 else 1
            s.walk = min(s.walk + 1, 5)
            fwd = 35
        else:
            s.walk = 0
            s.mode, fwd = (3, -15) if down else (0, 0)
        side = {2: 17, -2: -17, 3: 12, -3: -12}.get(s2, 0)
        sn, cs = rsin(s.head), rcos(s.head)
        tx = ((sn * (fwd << 12)) >> 12) + cs * side
        tz = ((cs * (fwd << 12)) >> 12) - sn * side
        s.vx += tx - (s.vx >> 2)
        s.vz += tz - (s.vz >> 2)
        s.x += s.vx >> 12
        s.z += s.vz >> 12
        # 3. turning: 0x8004425C
        a2 = 1 if s.omega >= 26215 else -1 if s.omega < -26214 else 0
        tl, tr = (left, right) if s.on_ground else (l1, r1)
        if tl and a2 != -1:
            a2 = 2
        if tr and a2 != 1:
            a2 = -2
        if a2 != 0:
            s.head = (s.head + (s.omega >> 16)) & 0xFFF
            w = s.omega
            start, cap, grow, div = (0x30000, 0x3F0000, lambda w: 2 * w + (w >> 1), 2) if s.mode in (0, 3) \
                else (0xB0000, 0x340000, lambda w: w + (w >> 2), 3)
            if a2 == 2:
                w = start if w < start else (grow(w) if w < cap else cap)
            elif a2 == -2:
                w = -start if w > -start else (grow(w) if w > -cap else -cap)
            elif a2 == 1:
                w = (w >> div) if w > 0x30000 else 0
            else:
                w = (w >> div) if w < -0x30000 else 0
            s.omega = w
        # 4. behaviour script: snap a falling Croc to a floor less than 131 below
        if not s.on_ground and not s.rising and s.y - s.floor < 131:
            s.y = s.floor
        # 5. behaviour script: Cross pressed on the ground starts a jump
        if cross and not s.prev_cross and s.on_ground:
            s.vy, s.budget, s.rising, s.on_ground = 261, 2300, True, False
        s.prev_cross = cross


def check(path):
    rows = []
    for line in open(path):
        if line.startswith("#") or line.startswith("tick"):
            continue
        t, b, x, y, z, h = line.strip().split(",")
        rows.append((b.split("|") if b else [], int(x), int(y), int(z), int(h)))
    _, x, y, z, h = rows[0]
    c = Croc(x, y, z, h, floor=y)
    worst = 0
    for b, x, y, z, h in rows[1:]:
        c.tick(b)
        worst = max(worst, abs(c.x - x), abs(c.y - y), abs(c.z - z), abs(((c.head - h + 2048) & 0xFFF) - 2048))
    print(f"{path}: {len(rows) - 1} ticks, max error {worst}")
    return worst


if __name__ == "__main__":
    sys.exit(1 if any(check(p) for p in sys.argv[1:]) else 0)

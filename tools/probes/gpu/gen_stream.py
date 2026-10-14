#!/usr/bin/env python3
"""Generate the GP0 command stream drawn by the GPU probe EXE.

Writes `gp0_stream.h` (compiled into gpu.ps-exe) and `gp0_stream.bin` (the same
stream, replayable with `psx gpu-replay`). The format of both is a sequence of
little-endian u32 records `[n, word_1 .. word_n]`, one per GP0 command, ending
with n = 0.

The stream fills x < 768 with a background, uploads test textures at x >= 768,
then draws one test group per 128x128 cell (6 columns x 4 rows). Each cell sets
its own drawing area and offset, so a VRAM diff can be attributed to a group by
the cell it falls in (see GROUPS below, printed by `--list`).

Deterministic: a fixed seed, no dependency beyond the standard library.
"""
import random
import struct
import sys
from pathlib import Path

CELL = 128
COLS = 6
ROWS = 4

rng = random.Random(0x5053_5831)
cmds: list[list[int]] = []


def emit(*words: int) -> None:
    cmds.append([w & 0xFFFF_FFFF for w in words])


def col(r: int, g: int, b: int) -> int:
    return (r & 0xFF) | ((g & 0xFF) << 8) | ((b & 0xFF) << 16)


def xy(x: int, y: int) -> int:
    return (x & 0xFFFF) | ((y & 0xFFFF) << 16)


def uv(u: int, v: int, hi: int = 0) -> int:
    return (u & 0xFF) | ((v & 0xFF) << 8) | ((hi & 0xFFFF) << 16)


def rcol() -> int:
    return col(rng.randrange(256), rng.randrange(256), rng.randrange(256))


def rpt(lo: int = -16, hi: int = 144) -> tuple[int, int]:
    return rng.randrange(lo, hi), rng.randrange(lo, hi)


# Texture locations (x >= 768 is never drawn into by the groups).
TEX15 = (768, 0)  # 64x64 15-bit, page tx=12
TEX8 = (832, 0)  # 128x64 texels in 64 halfwords, page tx=13
TEX4 = (896, 0)  # 128x64 texels in 32 halfwords, page tx=14
CLUT8 = (768, 256)
CLUT4 = (768, 257)


def texpage(tx: int, ty: int, blend: int, depth: int, dither: bool = False, extra: int = 0) -> int:
    return (tx & 15) | ((ty & 1) << 4) | ((blend & 3) << 5) | ((depth & 3) << 7) | (int(dither) << 9) | 0x400 | extra


def clut_word(x: int, y: int) -> int:
    return (x // 16) | (y << 6)


PAGES = {
    15: (texpage(12, 0, 0, 2), 0),
    8: (texpage(13, 0, 0, 1), clut_word(*CLUT8)),
    4: (texpage(14, 0, 0, 0), clut_word(*CLUT4)),
}


def upload(x: int, y: int, w: int, h: int, halves: list[int]) -> None:
    assert len(halves) == w * h
    if len(halves) % 2:
        halves = halves + [0]
    data = [halves[i] | (halves[i + 1] << 16) for i in range(0, len(halves), 2)]
    emit(0xA000_0000, xy(x, y), xy(w, h), *data)


def setup() -> None:
    emit(0xE600_0000)
    emit(0xE100_0000 | texpage(0, 0, 0, 0))
    emit(0xE200_0000)
    emit(0xE300_0000)
    emit(0xE400_0000 | 1023 | (511 << 10))
    emit(0xE500_0000)
    # Background for the drawing cells, a different shade per cell.
    for cy in range(ROWS):
        for cx in range(COLS):
            emit(0x0200_0000 | col(16 + cx * 8, 16 + cy * 8, 40), xy(cx * CELL, cy * CELL), xy(CELL, CELL))
    # 15-bit texture: varied texels, some fully transparent (0) and some with bit 15 (semi).
    t15 = []
    for v in range(64):
        for u in range(64):
            if (u ^ v) % 13 == 0:
                t15.append(0)
            else:
                p = ((u * 4) & 31) | (((v * 4) & 31) << 5) | (((u + v) & 31) << 10)
                if (u + 2 * v) % 7 == 0:
                    p |= 0x8000
                t15.append(p or 1)
    upload(*TEX15, 64, 64, t15)
    # 8-bit indices.
    t8 = []
    for v in range(64):
        for u in range(0, 128, 2):
            a, b = (u * 3 + v * 5) & 0xFF, ((u + 1) * 3 + v * 5) & 0xFF
            t8.append(a | (b << 8))
    upload(*TEX8, 64, 64, t8)
    t4 = []
    for v in range(64):
        for u in range(0, 128, 4):
            n = [((u + i) // 3 + v // 5) & 15 for i in range(4)]
            t4.append(n[0] | (n[1] << 4) | (n[2] << 8) | (n[3] << 12))
    upload(*TEX4, 32, 64, t4)
    clut8 = []
    for i in range(256):
        p = (i & 31) | (((i >> 3) & 31) << 5) | (((255 - i) >> 3) << 10)
        if i % 17 == 0:
            p = 0
        elif i % 5 == 0:
            p |= 0x8000
        clut8.append(p)
    upload(*CLUT8, 256, 1, clut8)
    clut4 = [0 if i == 0 else ((i * 2) | ((31 - i * 2) << 5) | ((i * 3 % 32) << 10) | (0x8000 if i % 4 == 1 else 0)) for i in range(16)]
    upload(*CLUT4, 16, 1, clut4)


GROUPS: list[str] = []


def cell(name: str):
    def wrap(fn):
        i = len(GROUPS)
        GROUPS.append(name)
        cx, cy = i % COLS, i // COLS
        x0, y0 = cx * CELL, cy * CELL

        def run():
            emit(0xE300_0000 | x0 | (y0 << 10))
            emit(0xE400_0000 | (x0 + CELL - 1) | ((y0 + CELL - 1) << 10))
            emit(0xE500_0000 | (x0 & 0x7FF) | ((y0 & 0x7FF) << 11))
            emit(0xE600_0000)
            emit(0xE200_0000)
            emit(0xE100_0000 | texpage(0, 0, 0, 0))
            fn()

        return run

    return wrap


def quad_pts() -> list[tuple[int, int]]:
    """A convex quad in GPU vertex order (v0 v1 / v2 v3), so its two triangles
    never overlap: the draw order of overlapping halves is not what is tested."""
    import math

    cx, cy = rpt(10, 118)
    w, h = rng.randrange(8, 90), rng.randrange(8, 90)
    a = rng.random() * 2 * math.pi
    out = []
    for sx, sy in ((-1, -1), (1, -1), (-1, 1), (1, 1)):
        x, y = sx * w / 2 + rng.randrange(-3, 4), sy * h / 2 + rng.randrange(-3, 4)
        out.append((cx + round(x * math.cos(a) - y * math.sin(a)), cy + round(x * math.sin(a) + y * math.cos(a))))
    return out


def gtri(op: int = 0x30, n: int = 3) -> None:
    words = [op << 24]
    pts = quad_pts() if n == 4 else [rpt() for _ in range(3)]
    for k in range(n):
        c = rcol()
        if k == 0:
            words[0] |= c
        else:
            words.append(c)
        words.append(xy(*pts[k]))
    emit(*words)


@cell("gouraud tris, no dither")
def g0():
    for _ in range(10):
        gtri(0x30)


@cell("gouraud quads, no dither")
def g1():
    for _ in range(6):
        gtri(0x38, 4)


@cell("gouraud tris, dither")
def g2():
    emit(0xE100_0000 | texpage(0, 0, 0, 0, dither=True))
    for _ in range(10):
        gtri(0x30)


@cell("gouraud quads, dither")
def g3():
    emit(0xE100_0000 | texpage(0, 0, 0, 0, dither=True))
    for _ in range(6):
        gtri(0x38, 4)


@cell("flat tris and quads")
def g4():
    for _ in range(10):
        emit(0x2000_0000 | rcol(), xy(*rpt()), xy(*rpt()), xy(*rpt()))
    for _ in range(4):
        emit(0x2800_0000 | rcol(), xy(*rpt()), xy(*rpt()), xy(*rpt()), xy(*rpt()))


def tex_prim(op: int, n: int, depth: int, gouraud: bool, blend: int = 0, dither: bool = False, uvmax: int = 64) -> None:
    page, clut = PAGES[depth]
    page = (page & ~0x60) | (blend << 5)
    words = [(op << 24) | rcol()]
    pts = quad_pts() if n == 4 else [rpt() for _ in range(3)]
    for k in range(n):
        if gouraud and k > 0:
            words.append(rcol())
        words.append(xy(*pts[k]))
        hi = clut if k == 0 else (page if k == 1 else 0)
        words.append(uv(rng.randrange(uvmax), rng.randrange(64), hi))
    emit(*words)


@cell("textured 15-bit raw")
def g5():
    for _ in range(6):
        tex_prim(0x25, 3, 15, False)
    for _ in range(3):
        tex_prim(0x2D, 4, 15, False)


@cell("textured 8-bit modulated, flat")
def g6():
    for _ in range(6):
        tex_prim(0x24, 3, 8, False, uvmax=128)
    for _ in range(3):
        tex_prim(0x2C, 4, 8, False, uvmax=128)


@cell("textured 4-bit gouraud, no dither")
def g7():
    for _ in range(6):
        tex_prim(0x34, 3, 4, True, uvmax=128)
    for _ in range(3):
        tex_prim(0x3C, 4, 4, True, uvmax=128)


@cell("textured gouraud, dither")
def g8():
    emit(0xE100_0000 | texpage(0, 0, 0, 0, dither=True))
    for _ in range(6):
        tex_prim(0x34, 3, 15, True)
    for _ in range(3):
        tex_prim(0x3C, 4, 8, True, uvmax=128)


def semi_gouraud(blend: int, dither: bool) -> None:
    emit(0xE100_0000 | texpage(0, 0, blend, 0, dither=dither))
    for _ in range(4):
        emit(0x6000_0000 | rcol(), xy(*rpt(0, 100)), xy(rng.randrange(8, 64), rng.randrange(8, 64)))
    for _ in range(6):
        gtri(0x32)


@cell("semi gouraud B/2+F/2")
def g9():
    semi_gouraud(0, False)


@cell("semi gouraud B+F")
def g10():
    semi_gouraud(1, False)


@cell("semi gouraud B-F")
def g11():
    semi_gouraud(2, False)


@cell("semi gouraud B+F/4, dither")
def g12():
    semi_gouraud(3, True)


@cell("semi textured (per-texel), blend 0 and 3")
def g13():
    for _ in range(4):
        emit(0x6000_0000 | rcol(), xy(*rpt(0, 100)), xy(rng.randrange(8, 64), rng.randrange(8, 64)))
    for _ in range(3):
        tex_prim(0x26, 3, 15, False, blend=0)
    for _ in range(3):
        tex_prim(0x36, 3, 8, True, blend=3, uvmax=128)


def line(op: int, pts: list[tuple[int, int]], cols: list[int], poly: bool) -> None:
    gouraud = op & 0x10
    words = [(op << 24) | cols[0], xy(*pts[0])]
    for k in range(1, len(pts)):
        if gouraud:
            words.append(cols[k])
        words.append(xy(*pts[k]))
    if poly:
        words.append(0x5555_5555)
    emit(*words)


@cell("flat lines, all octants")
def g14():
    cx, cy = 64, 64
    for k in range(24):
        import math

        a = k * 2 * math.pi / 24 + 0.1
        r = 40 + (k % 3) * 9
        line(0x40, [(cx, cy), (cx + round(r * math.cos(a)), cy + round(r * math.sin(a)))], [rcol()], False)
    for _ in range(6):
        line(0x40, [rpt(), rpt()], [rcol()], False)
    line(0x40, [(5, 5), (5, 5)], [rcol()], False)  # zero length


@cell("gouraud lines")
def g15():
    for _ in range(14):
        line(0x50, [rpt(), rpt()], [rcol(), rcol()], False)


@cell("polylines flat + gouraud, semi")
def g16():
    for _ in range(3):
        n = rng.randrange(3, 7)
        line(0x48, [rpt() for _ in range(n)], [rcol()], True)
    for _ in range(3):
        n = rng.randrange(3, 7)
        line(0x58, [rpt() for _ in range(n)], [rcol() for _ in range(n)], True)
    emit(0xE100_0000 | texpage(0, 0, 1, 0))
    for _ in range(4):
        line(0x52, [rpt(), rpt()], [rcol(), rcol()], False)


@cell("flat rects: variable/1/8/16, semi, clipped")
def g17():
    for _ in range(4):
        emit(0x6000_0000 | rcol(), xy(*rpt(-20, 120)), xy(rng.randrange(1, 70), rng.randrange(1, 70)))
    for op in (0x68, 0x70, 0x78):
        for _ in range(3):
            emit((op << 24) | rcol(), xy(*rpt(-10, 130)))
    emit(0xE100_0000 | texpage(0, 0, 1, 0))
    for _ in range(3):
        emit(0x6200_0000 | rcol(), xy(*rpt(0, 100)), xy(rng.randrange(8, 50), rng.randrange(8, 50)))


@cell("textured rects + texture window")
def g18():
    page, clut = PAGES[8]
    emit(0xE100_0000 | page)
    emit(0x6400_0000 | col(128, 128, 128), xy(0, 0), uv(0, 0, clut), xy(60, 60))
    emit(0x6500_0000, xy(64, 0), uv(10, 5, clut), xy(60, 60))
    emit(0x7C00_0000 | col(200, 100, 60), xy(-4, 64), uv(30, 40, clut))
    # Texture window: mask 2 (16 texels), offset 1 (x) / mask 1, offset 3 (y).
    emit(0xE200_0000 | 2 | (1 << 5) | (1 << 10) | (3 << 15))
    emit(0x6500_0000, xy(20, 64), uv(3, 7, clut), xy(50, 40))
    page15, _ = PAGES[15]
    emit(0xE100_0000 | page15)
    emit(0x6500_0000, xy(72, 64), uv(5, 9), xy(50, 60))
    emit(0x6600_0000 | col(128, 128, 128), xy(20, 108), uv(0, 0), xy(100, 20))


@cell("textured polys + texture window")
def g19():
    emit(0xE200_0000 | 4 | (3 << 5) | (2 << 10) | (1 << 15))
    for _ in range(4):
        tex_prim(0x2D, 4, 15, False)
    emit(0xE200_0000 | 1 | (6 << 5) | (31 << 10) | (31 << 15))
    for _ in range(3):
        tex_prim(0x34, 3, 8, True, uvmax=128)


@cell("mask bit set, then check")
def g20():
    emit(0xE600_0001)
    for _ in range(3):
        gtri(0x30)
    emit(0x6000_0000 | rcol(), xy(10, 10), xy(40, 40))
    emit(0xE600_0002)
    for _ in range(3):
        gtri(0x38, 4)
    emit(0x6000_0000 | rcol(), xy(0, 0), xy(128, 30))
    for _ in range(2):
        tex_prim(0x2D, 4, 15, False)
    emit(0xE600_0003)
    line(0x50, [(0, 127), (127, 0)], [rcol(), rcol()], False)
    emit(0xE600_0000)


@cell("slivers and tiny gouraud tris")
def g21():
    for _ in range(30):
        x, y = rpt(0, 120)
        emit(0x3000_0000 | rcol(), xy(x, y), rcol(), xy(x + rng.randrange(-3, 4), y + rng.randrange(-2, 30)), rcol(),
             xy(x + rng.randrange(-30, 30), y + rng.randrange(-3, 4)))
    for _ in range(4):
        x, y = rpt(0, 120)
        emit(0x3000_0000 | rcol(), xy(x, y), rcol(), xy(x + 100, y + 1), rcol(), xy(x - 3, y + 2))


@cell("large gouraud clipped on all sides")
def g22():
    emit(0x3800_0000 | col(255, 0, 0), xy(-300, -200), col(0, 255, 0), xy(400, -150), col(0, 0, 255), xy(-250, 300),
         col(255, 255, 255), xy(380, 350))
    emit(0x3200_0000 | col(0, 0, 0), xy(-500, 64), col(255, 255, 0), xy(600, 10), col(0, 255, 255), xy(64, 500))
    emit(0x3400_0000 | col(100, 200, 50), xy(-100, -100), PAGES[15][1] << 16, col(50, 100, 200), xy(250, -50),
         uv(63, 0, PAGES[15][0]), col(250, 10, 128), xy(20, 260), uv(0, 63))


@cell("VRAM copy, overlapping")
def g23():
    x0, y0 = 5 * CELL, 3 * CELL
    for _ in range(4):
        gtri(0x30)
    # Non-overlapping copy of the top-left quarter to the bottom-right.
    emit(0x8000_0000, xy(x0, y0), xy(x0 + 64, y0 + 64), xy(64, 64))
    # Overlapping copies: shift right by 3 and down by 2.
    emit(0x8000_0000, xy(x0 + 8, y0 + 8), xy(x0 + 11, y0 + 8), xy(40, 20))
    emit(0x8000_0000, xy(x0 + 70, y0 + 8), xy(x0 + 70, y0 + 10), xy(40, 30))


def main() -> None:
    out = Path(__file__).resolve().parent
    if "--list" in sys.argv:
        for i, name in enumerate(GROUPS_ORDER):
            print(f"{i:2} cell ({i % COLS},{i // COLS}) x={i % COLS * CELL} y={i // COLS * CELL}: {GROUPS[i]}")
        return
    setup()
    for g in GROUPS_ORDER:
        g()
    words: list[int] = []
    for c in cmds:
        words.append(len(c))
        words.extend(c)
    words.append(0)
    (out / "gp0_stream.bin").write_bytes(struct.pack(f"<{len(words)}I", *words))
    lines = ["// Generated by gen_stream.py; do not edit.", "#pragma once", "#include <stdint.h>",
             f"static const uint32_t g_gp0_stream[{len(words)}] = {{"]
    for i in range(0, len(words), 8):
        lines.append("    " + ", ".join(f"0x{w:08x}" for w in words[i:i + 8]) + ",")
    lines.append("};")
    (out / "gp0_stream.h").write_text("\n".join(lines) + "\n")
    print(f"{len(cmds)} commands, {len(words)} words")


GROUPS_ORDER = [g0, g1, g2, g3, g4, g5, g6, g7, g8, g9, g10, g11, g12, g13, g14, g15, g16, g17, g18, g19, g20, g21, g22,
                g23]

if __name__ == "__main__":
    main()

"""Tile PNG screenshots into one contact sheet (stdlib only).

Usage: python3 -I tools/contact_sheet.py OUT.png COLS IMG...
Each image is scaled (nearest) into a 160x120 cell, in argument order.
"""
import struct, sys, zlib

def read_png(path):
    d = open(path, 'rb').read()
    i, idat = 8, b''
    while i < len(d):
        n = struct.unpack('>I', d[i:i + 4])[0]
        t, c = d[i + 4:i + 8], d[i + 8:i + 8 + n]
        if t == b'IHDR':
            w, h, _, ct = struct.unpack('>IIBB', c[:10])
        elif t == b'IDAT':
            idat += c
        i += 12 + n
    bpp = {6: 4, 2: 3}[ct]
    raw, stride, out, prev, p = zlib.decompress(idat), w * bpp, [], bytearray(w * bpp), 0
    for _ in range(h):
        f = raw[p]; p += 1
        line = bytearray(raw[p:p + stride]); p += stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1: line[x] = (line[x] + a) & 255
            elif f == 2: line[x] = (line[x] + b) & 255
            elif f == 3: line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                pp = a + b - c; pa, pb, pc = abs(pp - a), abs(pp - b), abs(pp - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        out.append(bytes(line)); prev = line
    return w, h, bpp, out

def main():
    out, cols, paths = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
    cw, ch = 160, 120
    rows = (len(paths) + cols - 1) // cols
    W, H = cols * cw, rows * ch
    canvas = [bytearray(W * 3) for _ in range(H)]
    for k, p in enumerate(paths):
        w, h, bpp, px = read_png(p)
        ox, oy = (k % cols) * cw, (k // cols) * ch
        for y in range(ch):
            sy = y * h // ch
            row = px[sy]
            for x in range(cw):
                sx = x * w // cw
                canvas[oy + y][(ox + x) * 3:(ox + x) * 3 + 3] = row[sx * bpp:sx * bpp + 3]
    raw = b''.join(b'\0' + bytes(r) for r in canvas)
    def chunk(t, d):
        c = struct.pack('>I', len(d)) + t + d
        return c + struct.pack('>I', zlib.crc32(t + d))
    open(out, 'wb').write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', W, H, 8, 2, 0, 0, 0))
                          + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))

main()

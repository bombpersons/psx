#!/usr/bin/env python3
"""Worked external parser: King's Field PAL duplicated-rectangle upload streams.

Use --asset-parser for listing/extraction; --texture-parser for --textures inputs.
Accepts a single raw archive entry (RTIM or common FDAT[47]), not an entire archive.
No pixel depth or palette role is inferred. See research/raw-texture-uploads.md.
"""
import base64
import json
import struct
import sys


def decode(data):
    uploads = []
    offset = 0
    reason = "eof"
    while offset < len(data):
        if len(data) - offset < 16:
            raise ValueError(f"truncated duplicated rectangle at {offset}")
        header = data[offset:offset + 8]
        x, y, width, height = struct.unpack("<4H", header)
        # GAME.EXE 800173EC stops on mismatched rectangles / zero dimensions,
        # or a matching rectangle with x=-1. Sector padding can be nonzero.
        if header != data[offset + 8:offset + 16] or not width or not height or x == 0xffff:
            if not uploads:
                raise ValueError("no uploads before stream terminator")
            reason = "mismatched_rectangles" if header != data[offset + 8:offset + 16] else "sentinel"
            break
        if x + width > 1024 or y + height > 512:
            raise ValueError(f"rectangle outside VRAM at {offset}")
        end = offset + 16 + width * height * 2
        if end > len(data):
            raise ValueError(f"truncated upload data at {offset}")
        uploads.append({"source_offset": offset, "source_size": end - offset,
                        "rect": [x, y, width, height],
                        "data_base64": base64.b64encode(data[offset + 16:end]).decode("ascii")})
        offset = end
    if not uploads:
        raise ValueError("empty upload stream")
    print(f"{len(uploads)} uploads; {offset} bytes consumed; {len(data)-offset} trailing bytes; {reason}", file=sys.stderr)
    return {"version": 1, "uploads": uploads}


if __name__ == "__main__":
    try:
        data = sys.stdin.buffer.read(64 * 1024 * 1024 + 1)
        if len(data) > 64 * 1024 * 1024:
            raise ValueError("source exceeds 64 MiB")
        print(json.dumps(decode(data)))
    except (ValueError, struct.error) as error:
        print(error, file=sys.stderr)
        sys.exit(1)

#!/usr/bin/env python3
"""Example ARC1 archive index. Return spans into the unchanged input bytes."""
import json
import struct
import sys


def decode(data):
    # Authored example: ARC1, u32 count, then count pairs of u32 offset/size.
    if len(data) < 8 or data[:4] != b"ARC1":
        raise ValueError("expected ARC1 header")
    count, = struct.unpack_from("<I", data, 4)
    end = 8 + count * 8
    if count > 65536 or end > len(data):
        raise ValueError("invalid ARC1 table")
    entries = []
    for index, (offset, size) in enumerate(struct.iter_unpack("<II", data[8:end])):
        if offset < end or offset + size > len(data):
            raise ValueError("ARC1 entry outside payload")
        entries.append({"offset": offset, "size": size, "name": f"entry_{index}"})
    return {"version": 1, "entries": entries}


if __name__ == "__main__":
    try:
        print(json.dumps(decode(sys.stdin.buffer.read())))
    except (ValueError, struct.error) as error:
        print(error, file=sys.stderr)
        sys.exit(1)

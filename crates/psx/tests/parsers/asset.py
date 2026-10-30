#!/usr/bin/env python3
"""Example wrapper adapter. Emit one ordinary TIM/TMD/VAB/VAG payload."""
import base64
import json
import struct
import sys


def decode(data):
    # Authored example: WRAP, u32 payload length, then one standard asset.
    if len(data) < 8 or data[:4] != b"WRAP":
        raise ValueError("expected WRAP header")
    size, = struct.unpack_from("<I", data, 4)
    if size != len(data) - 8:
        raise ValueError("WRAP size mismatch")
    return {"version": 1, "data_base64": base64.b64encode(data[8:]).decode("ascii")}


if __name__ == "__main__":
    try:
        print(json.dumps(decode(sys.stdin.buffer.read())))
    except (ValueError, struct.error) as error:
        print(error, file=sys.stderr)
        sys.exit(1)

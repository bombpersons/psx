#!/usr/bin/env python3
"""Example per-frame wrapper adapter. Emit a standard BS v1/v2/v3 frame."""
import base64
import json
import sys


def decode(data):
    # Authored example: VBS1 followed by a standard BS frame. A real decoder
    # can read PSX_WIDTH / PSX_HEIGHT and PSX_STR_FRAME from its environment.
    if len(data) < 12 or data[:4] != b"VBS1":
        raise ValueError("expected VBS1 header")
    payload = data[4:]
    if payload[2:4] != b"\x00\x38" or int.from_bytes(payload[6:8], "little") not in (1, 2, 3):
        raise ValueError("expected BS v1/v2/v3 payload")
    return {"version": 1, "data_base64": base64.b64encode(payload).decode("ascii")}


if __name__ == "__main__":
    try:
        print(json.dumps(decode(sys.stdin.buffer.read())))
    except ValueError as error:
        print(error, file=sys.stderr)
        sys.exit(1)

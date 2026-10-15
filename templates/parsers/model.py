#!/usr/bin/env python3
"""Example TRI1 model decoder. Replace decode(), keeping the JSON contract."""
import json
import struct
import sys


def decode(data):
    # Authored example: TRI1, u16 vertex count, u16 triangle count, then
    # signed i16 XYZ vertices and u16 vertex indices, all little endian.
    if len(data) < 8 or data[:4] != b"TRI1":
        raise ValueError("expected TRI1 header")
    vertices, triangles = struct.unpack_from("<HH", data, 4)
    if len(data) != 8 + vertices * 6 + triangles * 6:
        raise ValueError("TRI1 size mismatch")
    points = list(struct.iter_unpack("<hhh", data[8:8 + vertices * 6]))
    faces = list(struct.iter_unpack("<HHH", data[8 + vertices * 6:]))
    if any(i >= vertices for face in faces for i in face):
        raise ValueError("TRI1 vertex index out of range")
    return {"version": 1, "objects": [{"vertices": points, "polygons": [
        {"vertices": face, "colors": [[255, 128, 64]]} for face in faces]}]}


if __name__ == "__main__":
    try:
        print(json.dumps(decode(sys.stdin.buffer.read())))
    except (ValueError, struct.error) as error:
        print(error, file=sys.stderr)
        sys.exit(1)

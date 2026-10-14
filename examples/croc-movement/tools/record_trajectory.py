#!/usr/bin/env python3
"""Record Croc's per-tick player trajectory from the native emulator core.

Runs `psx run --backend native --from-state STATE --replay ... --watch ...` for
one input scenario and turns the watch hits into a CSV with one row per game
tick (25 Hz = every second PAL vblank):

    tick,buttons,x,y,z,heading

* x/y/z: Croc's position, s32 at 0x800D8D8C/90/94 (player object 0x800D8A1C
  + 0x370/0x374/0x378), in Croc world units, +y up.
* heading: u16 at 0x800D8D86 (object + 0x36A), 4096 units per turn.
* buttons: what the pad held during that tick (as the replay drove it).

The state must be one taken in level 1-1 standing at the start (the case study
used frame 5400 of `.psx/native/replay_start.json` from power-on). Usage, from
the psx checkout that has bios/, media/ and the state:

    record_trajectory.py --psx target/release/psx --state .psx/croc-study/boot/state.bin \
        --frames 100 --spec "UP@0-50" -o walk.csv

`--spec` is `BTN[,BTN]@FROM-TO;...` in frames after the state (TO exclusive).
Ticks run on odd frames (1, 3, 5, ...); keep button edges on even frames so
every tick sees one well-defined input.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

POS = {"x": 0x800D8D8C, "y": 0x800D8D90, "z": 0x800D8D94}
HEADING = 0x800D8D86


def parse_spec(spec):
    spans = []
    for part in filter(None, spec.split(";")):
        btns, rng = part.split("@")
        a, b = map(int, rng.split("-"))
        spans.append(([x for x in btns.split(",") if x], a, b))
    return spans


def held_at(spans, frame):
    out = []
    for btns, a, b in spans:
        if a <= frame < b:
            out += [x for x in btns if x not in out]
    return out


def replay_events(spans, frames):
    events, last = [], None
    for f in range(frames + 1):
        cur = held_at(spans, f)
        if cur != last:
            events.append({"frame": f, "buttons": cur})
            last = cur
    return events


def signed(v, width):
    bits = 8 * width
    return v - (1 << bits) if v >= 1 << (bits - 1) else v


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--psx", required=True)
    ap.add_argument("--state", required=True)
    ap.add_argument("--bios", default="bios/scph7502.bin")
    ap.add_argument("--disc", default="media/Croc - Legend of the Gobbos.cue")
    ap.add_argument("--frames", type=int, required=True)
    ap.add_argument("--spec", required=True)
    ap.add_argument("-o", "--out", required=True)
    args = ap.parse_args()

    spans = parse_spec(args.spec)
    with tempfile.TemporaryDirectory() as tmp:
        replay = os.path.join(tmp, "replay.json")
        json.dump(replay_events(spans, args.frames), open(replay, "w"))
        run_dir = os.path.join(tmp, "run")
        cmd = [args.psx, "run", "--backend", "native", "--bios", args.bios, args.disc,
               "--from-state", args.state, "--frames", str(args.frames), "--replay", replay,
               "--out", run_dir, "--max-events", "2000000",
               "--watch", "%x:4" % POS["x"], "--watch", "%x:4" % POS["y"], "--watch", "%x:4" % POS["z"],
               "--watch", "%x:2" % HEADING]
        subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)
        writes = [json.loads(l) for l in open(os.path.join(run_dir, "writes.jsonl"))]
        # Values before the first tick come from the state's RAM via the first
        # write's `old` field (every tracked variable is rewritten every tick).
        cur = {}
        first_frame = min(w["frame"] for w in writes)
        for w in writes:
            for name, addr in list(POS.items()) + [("heading", HEADING)]:
                if w["address"] == addr and name not in cur:
                    cur[name] = signed(w["old"], w["width"])
        base = first_frame - 1  # watch frames are absolute (from power-on)
        by_frame = {}
        for w in writes:
            if w.get("writer") == "cpu":
                by_frame.setdefault(w["frame"] - base, []).append(w)

    with open(args.out, "w") as f:
        f.write(f"# Croc PAL level 1-1, psx native core, scenario: {args.spec}\n")
        f.write("# tick 0 = state at the save state; tick k = end of frame 2k-1\n")
        f.write("tick,buttons,x,y,z,heading\n")
        f.write(f"0,,{cur['x']},{cur['y']},{cur['z']},{cur['heading']}\n")
        for frame in range(1, args.frames + 1):
            for w in by_frame.get(frame, []):
                for name, addr in list(POS.items()) + [("heading", HEADING)]:
                    if w["address"] == addr:
                        cur[name] = signed(w["value"], w["width"])
            if frame % 2 == 1:
                tick = (frame + 1) // 2
                btns = "|".join(held_at(spans, frame - 1))
                f.write(f"{tick},{btns},{cur['x']},{cur['y']},{cur['z']},{cur['heading']}\n")
    print(args.out, file=sys.stderr)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Record Mega Man's RAM trajectory on the original game for each test scenario.

Needs the psx toolkit, your own Mega Man 8 (PAL, SLES-01064) disc image and a
SCPH-7502 BIOS. Run from the psx checkout root (where media/ and bios/ live):

    python3 examples/megaman8-movement/tools/mm8_trajectories.py \
        --state .psx/mm8-study/s3900/state.bin \
        --out examples/megaman8-movement/tests/data [scenario ...]

The state is an in-stage save state (see the case study for the boot replay).
Each scenario runs `psx run --backend native --from-state ... --watch` on the
player object, rebuilds every tick's values from the watch hits applied to the
state's RAM, and writes frame,x,y,vx,vy,state,cam_x (stage pixels, y down).
"""
import argparse, json, os, struct, subprocess, sys

SCENARIOS = {
    "walk": (40, "0:RIGHT 30:"),
    "walk_regrab": (40, "0:RIGHT 12: 14:RIGHT 30:"),
    "walk_left": (30, "0:LEFT 20:"),
    "jump_hold": (62, "0:CROSS"),
    "jump_tap": (40, "0:CROSS 5:"),
    "run_jump": (70, "0:RIGHT 10:RIGHT+CROSS 30:RIGHT 50:"),
    # Held out: recorded after the model was written, never used to fit it.
    "mixed": (100, "0:LEFT 3:LEFT+CROSS 8:LEFT 20:RIGHT 40: 44:CROSS 60: 61:CIRCLE+RIGHT 90:"),
}
PLAYER = 0x8015E89C
WATCH = [f"{PLAYER:08X}:4", "8015E8A8:4", "8015E8AC:4", "8015E8B0:4", "8015E8B4:4", "8016F26C:2"]
# (address, format): x, y, vx, vy as 16.16; state byte; camera x.
COLUMNS = [(0x8015E8A8, "fx"), (0x8015E8AC, "fx"), (0x8015E8B0, "fx"), (0x8015E8B4, "fx"), (0x8015E89E, "u8"), (0x8016F26C, "s16")]


def replay(script):
    """'0:RIGHT 30:' → psx replay events (each is the whole pad state from that frame)."""
    events = []
    for tok in script.split():
        frame, _, buttons = tok.partition(":")
        events.append({"frame": int(frame), "buttons": [b for b in buttons.split("+") if b]})
    return events


def run(psx, bios, disc, state, frames, script, out):
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, "replay.json")
    with open(path, "w") as f:
        json.dump(replay(script), f)
    cmd = [psx, "run", "--backend", "native", "--bios", bios, disc, "--from-state", state,
           "--frames", str(frames), "--replay", path, "--out", out]
    for w in WATCH:
        cmd += ["--watch", w]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)


def trajectory(run_dir, base_ram, frames):
    """Per tick (relative to the state), the COLUMNS values after that tick's writes."""
    ram = bytearray(open(base_ram, "rb").read())
    by_frame = {}
    for line in open(os.path.join(run_dir, "writes.jsonl")):
        w = json.loads(line)
        by_frame.setdefault(w["frame"], []).append(w)
    start = min(by_frame)
    rows = []
    for tick in range(frames):
        for w in by_frame.get(start + tick, []):
            off = w["address"] & 0x1FFFFF
            ram[off:off + w["width"]] = (w["value"] & ((1 << (8 * w["width"])) - 1)).to_bytes(w["width"], "little")
        row = []
        for addr, fmt in COLUMNS:
            o = addr & 0x1FFFFF
            row.append({"fx": lambda: struct.unpack_from("<i", ram, o)[0] / 65536,
                        "s16": lambda: struct.unpack_from("<h", ram, o)[0],
                        "u8": lambda: ram[o]}[fmt]())
        rows.append(row)
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--psx", default=os.environ.get("PSX_BIN", "psx"))
    ap.add_argument("--bios", default="bios/scph7502.bin")
    ap.add_argument("--disc", default="media/Mega Man 8.cue")
    ap.add_argument("--state", required=True)
    ap.add_argument("--work", default=".psx/mm8-study/trajectories")
    ap.add_argument("--out", required=True)
    ap.add_argument("names", nargs="*")
    a = ap.parse_args()
    # The state's own RAM is the starting point the watch hits are applied to.
    base = os.path.join(a.work, "_base")
    subprocess.run([a.psx, "run", "--backend", "native", "--bios", a.bios, a.disc, "--from-state", a.state,
                    "--frames", "0", "--out", base], check=True, stdout=subprocess.DEVNULL)
    os.makedirs(a.out, exist_ok=True)
    for name, (frames, script) in SCENARIOS.items():
        if a.names and name not in a.names:
            continue
        d = os.path.join(a.work, name)
        run(a.psx, a.bios, a.disc, a.state, frames, script, d)
        rows = trajectory(d, os.path.join(base, "ram.bin"), frames)
        with open(os.path.join(a.out, name + ".csv"), "w") as f:
            f.write(f"# input: {script}\n# frame,x,y,vx,vy,state,cam_x  (psx run --watch on Mega Man 8 PAL, from the in-stage state)\n")
            for tick, (x, y, vx, vy, st, cam) in enumerate(rows):
                f.write(f"{tick},{x:.6f},{y:.6f},{vx:.6f},{vy:.6f},{st},{cam}\n")
        print(name, len(rows), "ticks", file=sys.stderr)


if __name__ == "__main__":
    main()

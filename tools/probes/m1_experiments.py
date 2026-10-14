"""M1 backend experiments against PCSX-Redux via `psx run`.

Usage: python3 -I tools/probes/m1_experiments.py OUT_DIR
Runs from the psx project root. Prints a JSON summary.
"""
import hashlib, json, subprocess, sys, time
from pathlib import Path

OUT = Path(sys.argv[1]).resolve()
PSX = "./target/debug/psx"
EXE = "tools/probes/basic/basic.ps-exe"
BIOS = "bios/SCPH1001.BIN"
FRAMES_WATCH = "0x80014808"


def run(name, *args):
    out = OUT / name
    t = time.monotonic()
    p = subprocess.run([PSX, "run", EXE, "--bios", BIOS, "--out", str(out), "--timeout", "120", *args],
                       capture_output=True, text=True, env={"PATH": "/usr/bin:/bin"})
    wall = time.monotonic() - t
    r = json.loads(p.stdout)
    if "error" in r:
        raise SystemExit(f"{name}: {r['error']}")
    r["_wall_s"] = round(wall, 2)
    return r


def h(r, f):
    return r["files"][f]["sha256"] if f in r["files"] else None


def same(a, b, files=("ram.bin", "scratch.bin", "vram.bin", "screen.bin")):
    diff = [f for f in files if h(a, f) != h(b, f)]
    if a["cpu_cycles"] != b["cpu_cycles"]:
        diff.append("cpu_cycles")
    if a["registers"] != b["registers"]:
        diff.append("registers")
    return diff


OUT.mkdir(parents=True, exist_ok=True)
summary = {}

a1 = run("det_a", "--frames", "300")
a2 = run("det_b", "--frames", "300")
summary["determinism"] = {"differences": same(a1, a2), "wall_s": [a1["_wall_s"], a2["_wall_s"]]}

w = run("observed", "--frames", "300", "--watch", FRAMES_WATCH)
summary["observation_effect"] = {"differences": same(a1, w), "watch_hits": w["watch"]["hits"], "wall_s": w["_wall_s"]}

replay = OUT / "replay.json"
replay.write_text(json.dumps([{"frame": 150, "buttons": ["CROSS"]}, {"frame": 200, "buttons": []}]))
straight = run("straight", "--frames", "300", "--replay", str(replay))
first = run("split_first", "--frames", "180", "--replay", str(replay), "--save-state")
# Frames and replay events are relative to the job start, so shift the replay.
replay2 = OUT / "replay2.json"
replay2.write_text(json.dumps([{"frame": 0, "buttons": ["CROSS"]}, {"frame": 20, "buttons": []}]))
second = run("split_second", "--frames", "120", "--replay", str(replay2),
             "--from-state", first["files"]["state.bin"]["path"])
ram = (OUT / "straight/ram.bin").read_bytes()
cross = int.from_bytes(ram[0x14804:0x14808], "little")
summary["save_resume"] = {
    "differences_vs_straight": same(straight, second),
    "cross_frames_straight": cross,
    "cross_frames_resumed": int.from_bytes((OUT / "split_second/ram.bin").read_bytes()[0x14804:0x14808], "little"),
    "state_bytes": (OUT / "split_first/state.bin").stat().st_size,
}
summary["speed"] = {"frames": 300, "wall_s_unobserved": a1["_wall_s"], "wall_s_watch": w["_wall_s"]}
print(json.dumps(summary, indent=2))

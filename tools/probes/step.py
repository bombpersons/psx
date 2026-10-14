"""Advance a saved state with button presses; save the new state and screen.

Usage: python3 -I tools/probes/step.py DISC BIOS FROM_STATE OUT_NAME FRAMES [FRAME:BTN+BTN ...]
  e.g. step.py media/x.cue bios/b.bin .psx/states/a.bin b 120 0:START 4:
Each FRAME:BUTTONS sets the held buttons from that frame (empty = release).
Writes .psx/states/OUT_NAME.bin and .psx/states/OUT_NAME.png; prints JSON.
"""
import json, shutil, subprocess, sys, tempfile
from pathlib import Path

disc, bios, state, name, frames, *events = sys.argv[1:]
replay = []
for e in events:
    f, _, b = e.partition(":")
    replay.append({"frame": int(f), "buttons": [x for x in b.split("+") if x]})
tmp = Path(tempfile.mkdtemp(prefix="psx-step-", dir=Path(".psx")))
(tmp / "replay.json").write_text(json.dumps(replay))
out = tmp / "run"
p = subprocess.run(["./target/debug/psx", "run", disc, "--bios", bios, "--frames", frames,
                    "--from-state", state, "--save-state", "--replay", str(tmp / "replay.json"),
                    "--out", str(out)], capture_output=True, text=True)
r = json.loads(p.stdout)
if "error" in r:
    sys.exit(json.dumps(r))
states = Path(".psx/states")
shutil.copy(out / "state.bin", states / f"{name}.bin")
shutil.copy(out / "screen.png", states / f"{name}.png")
print(json.dumps({"state": str(states / f"{name}.bin"), "screen": str(states / f"{name}.png"),
                  "frames": r["frames"], "wall_ms": r["wall_ms"], "run_dir": str(out)}))

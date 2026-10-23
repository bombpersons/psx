#!/usr/bin/env bash
# Diff psx-gpu against Mednafen (psx-oracle) on the GPU probe.
#   tools/probes/gpu/compare.sh PSX_BIN PSX_ORACLE_BIN OUT_DIR [TAG]
# (PSX_ORACLE_BIN is target/release/psx-oracle from `cargo build --release -p psx-oracle`.)
# Run from the main checkout (needs bios/). Runs the probe EXE in Mednafen
# once (cached in OUT_DIR/oracle), replays gp0_stream.bin into
# psx-gpu and prints per-cell differing pixel counts (cell = test group, see
# `gen_stream.py --list`). Only the drawing cells (x < 768) are compared.
# Build gpu.ps-exe first: `make` in this directory (see Makefile).
set -euo pipefail
psx=${1:?psx binary}
oracle=${2:?psx-oracle binary}
out=${3:?output dir}
tag=${4:-replay}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
if [ ! -f "$out/oracle/vram.bin" ]; then
    "$oracle" run "$here/gpu.ps-exe" --bios bios/scph7502.bin --frames 200 --out "$out/oracle" > "$out/oracle.json"
fi
"$psx" gpu-replay "$here/gp0_stream.bin" -o "$out/$tag.bin" --png "$out/$tag.png" > /dev/null
"$psx" vram-diff "$out/oracle/vram.bin" "$out/$tag.bin" --region 0,0,768,512 --png "$out/$tag.diff.png" --samples 0 \
    > "$out/$tag.diff.json"
python3 -I - "$out/$tag.diff.json" "$here/gen_stream.py" <<'EOF'
import json, subprocess, sys
d = json.load(open(sys.argv[1]))
names = subprocess.run([sys.executable, "-I", sys.argv[2], "--list"], capture_output=True, text=True).stdout.splitlines()
print(f"differing {d['differing']} / {d['pixels']} (mask-bit only {d['mask_bit_only']})")
print("channel deltas (psx-gpu - mednafen):", json.dumps(d["channel_delta_b_minus_a"]))
for c in d["cells"]["differing"]:
    i = c["cx"] + 6 * c["cy"]
    print(f"  {c['count']:6}  {names[i] if i < len(names) else i}")
EOF

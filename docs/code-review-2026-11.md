# Code review, November 2026

A review of the whole first-party codebase on branch `psx-web` (commit 3da95b6): about 80K
lines of Rust across the eleven workspace crates, `crates/psx/src/web/index.html`, the Bevy
template, `tools/`, the install scripts and CI. The vendored Mednafen tree and the vendored crate
copies under `templates/*/vendor` and `examples/*/vendor` were not reviewed.

At review time `cargo clippy --workspace --all-targets` reported no warnings and
`cargo test --workspace` passed with no failures.

Status column: **open** until fixed; fixes are committed one at a time and this table is updated.

## Overall

The emulator core, GPU and GTE are close ports of Mednafen, and no logic errors were found in
them. The risks are where outside input (disc names, manifests, URL parameters, file sizes) is
used as a path, an action or an allocation size without being checked, and a few places that
behave differently from what their comments say.

## High

| # | Where | Defect | Status |
|---|---|---|---|
| H1 | `crates/psx-media/src/iso9660.rs:163-171`, `crates/psx/src/media_cmd.rs:305,371` | Disc directory names containing `..` or `/` are joined onto the output folder, so `psx media extract` can write anywhere. | fixed |
| H2 | `crates/psx/src/messages.rs:108,62,173`, `mcp.rs:496`, `main.rs:413` | Sending a message marks every earlier message as seen by the agent, and the `message` tool never delivers pending ones, so the person's messages can be lost. `mark_seen` is not locked; listing shows 12 threads but marks all seen. | fixed |
| H3 | `crates/psx/src/web.rs:334,903-1056,256` | `replay?start=1` and `port_replay?start=1` start processes from a GET with no token; `scenario`/`name` are not checked for `..`; no frame-blocking header. | fixed |
| H4 | `crates/psx-assets/src/tmd.rs:202-264`; `psx-media/src/disc.rs:379`, `iso9660.rs:142,213`, `str_video.rs:443` | A 1.6 MB TMD whose objects share one large table expands to hundreds of millions of polygons. Other parsers reserve memory from untrusted size fields. | fixed |

## Medium

| # | Where | Defect | Status |
|---|---|---|---|
| M1 | `crates/psx/src/template.rs:611,359` | Template update deletes any manifest key whose hash matches, with no check against `..`/absolute paths. | fixed |
| M2 | `crates/psx/src/code.rs:247,526,594,827` | Disassembly ranges near 0xFFFFFFFF overflow the loop counter: panic in dev, endless loop in release. | fixed |
| M3 | `crates/psx/src/mcp.rs:512`, `run.rs:561` | A huge `timeout` panics when added to a `Duration`/`Instant`. | fixed |
| M4 | `crates/psx/src/ghidra.rs:944` | The analysis cache key leaves out the contents of the language spec files. | fixed |
| M5 | `crates/psx/src/calls.rs:92-107`, `reports.rs` | Reviewer reports, meant for the person only, are copied into `.psx/calls.jsonl`. | fixed |
| M6 | `crates/psx/src/states.rs:318,642` | Concurrent rebuilds `remove_dir_all` each other's output folder. | fixed |
| M7 | `crates/psx-assets/src/adpcm.rs:46-52` vs `psx-core/src/spu.rs:739` | The offline ADPCM decoder says it matches the SPU but clamps filters/shifts and rounds differently. | fixed |
| M8 | `crates/psx-core/src/bus.rs:859,938` | Byte/halfword reads of timers and cache control are not masked to the access width. | fixed |
| M9 | `crates/psx-core/src/cpu.rs:1050-1059,1208-1237` | COP0 moves: EPC writable, SR/DCIC unmasked, no reserved-instruction exception for registers 0/1/2/4/10, MFC0 from 16-31 writes rt, BC0x/BC2x raise RI. | fixed |
| M10 | `crates/psx-gte/src/gte.rs:61-74` | The opcode alias table runs commands Mednafen treats as no-ops. | fixed |
| M11 | `crates/psx-gpu/src/raster.rs:669` | X-flipped sprites start sampling at `u`, Mednafen and this crate's timing at `u\|1`. | open |
| M12 | `crates/psx-core/src/memcard.rs:135` | A corrupt card's block link indexes past the image and panics. | open |
| M13 | `crates/psx-core/src/callcheck.rs:129` | `compare()` reloads state without reattaching the disc. | open |
| M14 | `templates/psx-bevy/psx-bevy/src/scenario.rs:342`, `game/src/disc.rs:83`, `dev_mcp.rs:596` | A replay passes after zero ticks when no compared quantities overlap; ticks count while the game waits for data; `game_step` max does not fit the 120 s reply limit. | open |
| M15 | `.github/workflows/release.yml:20` | `contents: write` for every job; actions pinned to movable tags. | open |

## Low

| # | Where | Defect | Status |
|---|---|---|---|
| L1 | `psx-media` cue/chd/disc/iso9660, `psx-scenario/src/value.rs:243`, `psx-symbols/src/lib.rs:277`, `psx/src/vram_diff.rs`, `psx/src/port/assets.rs:436` | Unchecked integer arithmetic on file data. | open |
| L2 | `crates/psx-core/src/cpu.rs:1098,1138` | SWL/SWR/LWL/LWR use full-word accesses on I/O. | open |
| L3 | `crates/psx/src/web/index.html:1746,1920,2291,1581` | Held key stuck after labelling; one failed POST stops input; leaked `mouseup` listeners; racing `startPlayer`. | open |
| L4 | `install.sh:57`, `install.ps1:40,22` | Installers look for a `psx-clean` binary the release does not ship; relative `PSX_INSTALL_DIR` saved to PATH. | open |
| L5 | `examples/croc-movement/psx-bevy` | Missing template fixes, including the two-run determinism check. | open |
| L6 | `crates/psx/src/tickfind.rs:21` | Wrapped 8/16-bit counters misranked (mask applied to one side only). | open |
| L7 | `crates/psx/src/decomp.rs:332,357,371` | Linker script path breaks on `,`/`"`; versioned gcc names give wrong binutils names. | open |
| L8 | `crates/psx/src/ghidra.rs:951-1048` | `reanalyze` leaves the old Ghidra project behind. | open |
| L9 | `crates/psx/src/coverage.rs:193` | Chunks before a function's entry underflow. | open |
| L10 | `crates/psx-core/src/bus.rs:158,609`, `lib.rs:600`, `cdrom.rs:444` | Addresses 0x20000000-0x7FFFFFFF mirror RAM; `words*4` overflow; unvalidated BSS size; unchecked root LBA. | open |
| L11 | `crates/psx-gpu/src/lib.rs:654,706`, `vram.rs:6`, `prim.rs:254` | Untimed `draw_time` underflows; draw area Y masked to 9 bits; VRAM deserialize unchecked; zero-size upload encodes as 1024x512. | open |
| L12 | `crates/psx-oracle/src/ffi.rs:129,247`, `run.rs:131` | Safe FFI wrappers assume an open emulator; unchecked add inside an `extern "C"` callback. | open |
| L13 | `crates/psx-symbols/src/names.rs:293,441` | Symlink cycles recurse forever; `s * len` unchecked. | open |
| L14 | `crates/psx-fixed/src/mode.rs:153` | Shift counts of 64 or more panic in debug; 32-63 differ from MIPS `& 31`. | open |
| L15 | `crates/psx/src/parser.rs:118` | A forked child holding the pipes makes a finished parser time out. | open |

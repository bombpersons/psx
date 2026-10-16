run executes a disc, EXE, BIOS boot or checkpoint for a bounded number of
frames (vsyncs). backend native (default) gives exact observations and native
states; redux is the separately installed PCSX-Redux oracle with watches,
trace_cd and trace_bios only, and fast uses its dynarec without observation.
context explains how BIOS and media defaults resolve; from_state requires the
same BIOS, media and backend identity it was saved with. run never downloads.

Checkpoints: save_state writes state.bin at the end; save_state_at saves
state_fNNNNNN.bin at the start of chosen run frames. Continue with from_state.
Input: input (inline array) or replay (file); see guide topic input.

Observations (native):
- sample ADDR:FMT[:NAME] writes samples.csv, one row per vblank. Row N is
  memory after N run frames, before frame N's input and pokes. sample_every
  thins rows; the last frame is always sampled.
- watch ADDR[:WIDTH] logs every CPU/DMA store into the range to writes.jsonl
  with writer PC, old and new value. watch_changes_only drops same-value stores.
- watch_reads LO-HI or ADDR[:WIDTH] logs CPU loads to reads.jsonl with PC.
- poke ADDR=VALUE[:WIDTH|:FMT][@FRAME|@A..B|@A..] writes memory at the start of
  a run frame to test a hypothesis; keep an unmodified control run.
- trace writes trace.txt (windowed by trace_from_frame / trace_range),
  coverage.txt and calls.json; max_events caps every event log.
- trace_cd logs CD transfers for loads; trace_bios logs BIOS calls.
- trace_spu logs voice key on/off (settings, storing PC, sample_sha256),
  each keyed voice's first END and sound RAM uploads to spu.jsonl. spu.samples
  groups key ons by sample; sample_sha256 equals assets_extract adpcm_sha256,
  and upload.ram_hex is the sample's main RAM source.
- gpu_frame N captures one frame's GPU commands; gpu_next_draw searches for
  drawing (see guide topic assets). snapshot_every saves screens/fNNNNNN.png.
- memcard1/memcard2 take a .mcd path or new; originals are never modified and
  written cards are saved in the run directory. pad dualshock enables sticks.

Frame numbering: frames, input, poke, gpu_frame and trace windows count from
the run start (run_frame). frame in logs is absolute (vblanks since power-on,
or since the checkpoint's own origin).

Results: the full report is always saved as report.json in run_dir. brief
returns a short summary with artifact descriptors; summary returns only
status, frames, paths and counts for the observations you asked for. Read any
file with artifact_read. status partial with termination timeout means the
wall-clock budget ran out: check frames.reached. Watch metadata distinguishes
disabled, empty and capped logs (complete / truncated). input.jsonl records
when the scheduler applied each event, not that the game read it.

native is an emulator diagnostic (boot from power-on, report PC, CD history,
SPU state, unhandled I/O and hot PCs); lockstep compares the native core with
PCSX-Redux instruction by instruction. Neither is needed for game investigation.

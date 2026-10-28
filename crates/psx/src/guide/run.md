run executes a disc, EXE, BIOS boot or checkpoint for a bounded number of
frames (vsyncs) on the native core, with exact observations and native states.
context explains how BIOS and media defaults resolve; from_state requires the
same BIOS and media identity it was saved with. run never downloads.

Checkpoints: save_state writes state.bin at the end; save_state_at saves
state_fNNNNNN.bin at the start of chosen run frames. Continue with from_state.
Input: input (inline array) or replay (file); see guide topic input.

Observations (native):
- sample writes samples.csv, one row per vblank. Each entry is ADDR:FMT[:NAME]
  text or {"addr", "format", "name"}; FMT is u8 i8 x8 u16 i16 x16 u32 i32 x32,
  fxI.F (fx16.16, fx4.12), bits of a word (u16[14], u8[4..7]; guide topic
  fields) or a watch-style width 1/2/4 (unsigned), default u32.
  Row N is memory after N run frames, before frame N's input and pokes.
  sample_every thins rows; the last frame is always sampled. With a field map
  (samples.toml, or map) sample takes names, and a [tick] adds a tick column:
  see guide topic fields.
- find_tick counts every CPU store to main RAM by PC and address and groups
  them by the fields they fell in (tick_finder.json): finding a game's tick,
  guide topic fields.
- until NAME=VALUE (also != < <= > >=) stops at the first vblank where a
  named sample holds: waiting on a menu or a level (guide topic fields).
- watch ADDR[:WIDTH] logs every CPU/DMA store into the range to writes.jsonl
  with writer PC, old and new value. watch_changes_only drops same-value stores.
- watch_reads LO-HI or ADDR[:WIDTH] logs CPU loads to reads.jsonl with PC.
- poke ADDR=VALUE[:WIDTH|:FMT][@FRAME|@A..B|@A..] writes memory at the start of
  a run frame to test a hypothesis; keep an unmodified control run. With a
  field map, NAME=VALUE[@...] writes one field by name (lara.x=100), VALUE in
  the field's format; through a pointer subject it writes where the pointer
  leads at that frame, and a frame whose pointer is null writes nothing
  (counted as skipped).
- trace writes trace.txt (windowed by trace_from_frame / trace_range),
  coverage.txt, code.bin (each RAM page's history: a copy when code first
  runs in it, another whenever code runs in it after it changed, and one
  as another page is copied when RAM changed it with no code run there,
  code_pages.refreshed: so each copy is RAM's code at its moment),
  executed.bin (each executed word with the copy it ran from, the
  instruction it ran as, the moment it first ran: the number of the latest
  copy then, and whether it ever continued the code before it: ran
  straight on from the word before, outside a jump's delay slot (after a
  branch's, taken or not), or was returned to by a call two words before) and calls.json (each call target and
  call site with the copy each ran from, and the moments a call to it ran
  in, from the call until it returned); max_events caps every event log.
  It caps trace.txt's lines (lines_dropped), not code.bin, executed.bin or
  calls.json: those are whole unless code_pages_dropped (pages past the
  copy cap) or call_frames_dropped (calls nested past the return stack)
  is above 0, which the run's summary also reports. The inventory and
  coverage read only those files.
- trace_cd logs CD transfers for loads; trace_bios logs BIOS calls.
- trace_spu logs voice key on/off (settings, storing PC, sample_sha256),
  each keyed voice's first END and sound RAM uploads to spu.jsonl. spu.samples
  groups key ons by sample; sample_sha256 equals assets_extract adpcm_sha256,
  and upload.ram_hex is the sample's main RAM source.
- audio records what the console plays (SPU voices, reverb and CD/XA audio,
  mixed as the game sets them) to audio.wav, 44.1 kHz stereo, from the run
  start; the report gives its length and peak (peak 0: silent).
- profile writes profile.json: CPU cycles by function (call targets, named
  from symbols), with calls, cycles per call and the hottest instruction.
  Cycles are Mednafen's timing, so shares are real; time in the game's VSync
  wait shows how much of each frame it leaves idle.
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
SPU state, unhandled I/O and hot PCs). It is not needed for game investigation.

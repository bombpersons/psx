# SPU voice trace, 2026-10-17

`run --trace-spu` (native) records, from the emulated SPU itself:

- `key_on` per voice: frame/cycle, the storing PC, start address, raw pitch and
  rate, left/right volume (fraction, or `null` for a sweep), ADSR, noise and pitch
  modulation, and the sample: SHA-256 of its ADPCM bytes from the start through
  the first END block, its length and whether that END block repeats;
- `key_off` per voice, with the storing PC;
- `end`: a voice keyed during the trace reaching its first END block (ENDX).
  Silent voices keep walking sound RAM and set ENDX as on hardware, so voices
  keyed before the trace report no END;
- `upload`: DMA-4 transfers (with the main RAM source) and manual FIFO writes,
  merged while contiguous.

`assets_extract` reports the same hash as `adpcm_sha256` for every VAB waveform
and VAG. The report's `spu.samples` groups recorded key ons by hash, with voices,
PCs, first/last run frames and, when the upload was traced, the main RAM source
of the sample (`upload.ram_hex`) for `loads`/`trace_cd` provenance.

## Validation

- Unit (psx-core): DMA and merged FIFO uploads, key on settings and hash, a
  single END per key on, key off, and no END report for a voice keyed while not
  tracing.
- CLI/MCP (authored BIOS, no firmware or media): one FIFO-uploaded END block,
  voice 1 keyed at 44.1 kHz; report parity, the key-on PC, sample hash/size and
  the upload link; `--backend redux` refuses the flag.
- Mega Man 8 (PAL), the held-out `mixed` scenario from the in-stage state, 100
  frames: 68 key ons, 74 key offs, 55 ENDs, 9 distinct samples. Extracting the
  70 VABs under `/SOUND/` (1143 waveforms) matched all 9 hashes: music voices
  0-9 to `PBGM01.PAC` (shared instruments also match other banks), and voice 21
  effects to `PCOMMON.PAC` waveforms 52 (run frame 5, two frames after Cross),
  3 and 2. The samples were uploaded before the state, so no upload link is
  available in that run.

## Limits

- Only key on/off records voice settings; later pitch or volume writes (bends,
  fades) are not logged. A `--watch` on the driver's voice tables covers them.
- Key ons come from the sound driver, usually one routine. The PC identifies
  the driver, not the gameplay code that requested the sound.
- Music sequencing (SEQ/SEP) and template playback remain in the backlog.

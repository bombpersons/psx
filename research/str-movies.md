# STR movies offline (`psx str`)

`psx str <image.cue>::/PATH -o DIR` (or `<image.cue> /PATH`) decodes an STR
movie straight from the disc, without running the game:

- `frame_00001.png …`: one PNG per frame, cropped to the STR header's width × height
- `audio.wav`: the interleaved XA-ADPCM channel at 44.1 kHz, from the same code as `psx xa`
- `contact.png`: up to 24 evenly spaced frames at half size, for a quick look
- `frames.json`: per frame: STR frame number, first LBA, chunk count, missing chunks, BS version, quant, end code
- `summary.json`: the same JSON as stdout

Options: `--frames A..B` (1-based, inclusive; also `A..`, `..B`, `N`),
`--channel N` (default: the XA channel with the most sectors), `--no-audio`,
`--y4m` (uncompressed 4:4:4 `movie.y4m` at 15 fps for ffmpeg/mpv).

## Pipeline

1. **Demux** (`psx_media::str_video`). A sector whose user data starts with
   STR ID 0160h and type 8001h (or 8101h, Chrono Cross disc 2) is a video
   chunk: header fields chunk/chunks, frame number, frame size, width, height.
   XA audio sectors (submode bit 2) are counted per (file, channel); anything
   else is reported as `other`.
2. **Reassembly.** `FrameAssembler` collects the chunks of a frame in any
   order, with audio interleaved between them. A frame is emitted when all
   chunks arrive, or when another frame starts first (missing chunks are listed).
3. **BS bitstream → MDEC run-length** (what games do in software). BS header
   `[MDEC size/4, 3800h, quant, version]`, then halfwords read MSB-first. Per
   macroblock six blocks (Cr, Cb, Y1..Y4): DC (v1/v2: raw 10 bits; v3:
   Huffman-coded difference ×4 against the previous Cr/Cb/Y DC), the AC
   Huffman table (psx-spx "BS Compression AC Values", including the 22-bit
   escape), EOB → `FE00h`. Output is the exact halfword stream a game DMAs to
   MDEC0. The end-of-frame code (1FFh / `1111111111`) is checked after the
   last macroblock and reported per frame.
4. **RLE → pixels** on the native core's hardware MDEC model
   (`psx_core::mdec::Mdec`, unchanged). It is loaded as libpress
   `DecDCTReset` leaves it: the standard IDCT scale table (MDEC(3)) and the
   MPEG default intra matrix in zig-zag order for luma and chroma (MDEC(2)).
   Each macroblock goes in as an MDEC(1) 24 bpp command, and macroblocks are
   placed column-major (16 px wide strips top to bottom, left to right).

Final Fantasy VII prefixes each frame with 28h bytes of extra (camera)
data before the BS header; that prefix is detected (BS header at 28h, not at 0)
and skipped. It is counted in `ff7_prefixed_frames`.

Frame rate: frames over the sectors they span (`sectors_per_frame`) at the
drive speed. The speed comes from the selected XA channel: one sector holds
2016 (4-bit stereo) samples, so 37.8 kHz needs 18.75 sectors/s. The median
distance between that channel's sectors (usually 8) gives 150 sectors/s (2x).
The median is used because some streams have audio-free stretches. Without
audio it assumes 2x.

## Not decoded (detected and reported)

`decode` refuses, with the variant named in the error and in `bs_variants`:

- `not_bs`: STR video sectors whose frames have no 3800h BS header. Ape
  Escape's `DEMO/*.STR` uses type 8001h for polygon streams with 0×0
  dimensions, and the error says so.
- `v0` (Serial Experiments Lain), `iki_or_encrypted` (iki: Gran Turismo,
  Legend of Dragoon; the Star Wars XOR/SWAP variants), and `fraquant`
  (X-Files and others: a 16-bit quant scales the quant table).
- Files without STR sectors: Croc's `CUTS/*.AN2` (its own format: 2107
  non-STR Form 1 sectors plus XA) and audio-only streams such as Spyro's
  `MUSIC*.STR` (use `psx xa` for those).

Also not handled: EA's VLC0/MDEC chunk formats, FF9 (type 0004h, Form 2
video in reverse order), FF8, and Wacwac. None of these are in `media/`.
Crash Bandicoot has no STR files.

## Results (all in `media/`)

| Game | File | Size | BS | Frames | fps | Notes |
|---|---|---|---|---|---|---|
| Croc | `/ARGOLOGO.STR` | 320×240 | v2 | 131 | 15.0 | |
| Croc | `/FOXLOGO.STR` | 320×240 | v3 | 135 | 15.0 | bit-exact vs emulator (see below) |
| Tomb Raider | `/FMV/CORELOGO.FMV` | 320×208 | v2 | 152 | 15.0 | bit-exact vs emulator |
| Tomb Raider | `/FMV/CAFE.FMV` | 320×208 | v2 | 1547 | 15.0 | |
| Castlevania SOTN | `/LOGO15XA.STR` | 320×240 | v3 | 143 | 15.0 | |
| Castlevania SOTN | `/OPN_WS.STR` | 320×240 | v2 | 922 | 15.0 | |
| Mega Man 8 | `/MOVIE/CAPCOM15.STR` | 320×240 | v2 | 116 | 15.0 | silent channel 0 plus music channel 1 |
| Mega Man 8 | `/MOVIE/ROCK8_0.STR` | 320×240 | v2 | 1343 | 15.0 | |
| Final Fantasy VII | `/MOVIE/BIKEGET.MOV` | 320×224 | v1 | 751 | 15.0 | 28h-byte prefix |
| Resident Evil | `/PSX/MOVIE/CAPCOM.STR` | 320×240 | v2 | 290 | 30.0 | 5 sectors/frame |
| WipEout | `/WOPAL.AV` | 320×192 | v1 | 2336 | 25.0 | PAL, 6 sectors/frame |
| Metal Gear Solid | `/MGS/ZMOVIE.STR` | 320×176 | v2 | 2215 | 14.3 | archive of 4 movies; 4 subtitle sectors (type 0001h) |
| R-Type Delta | `/XA/IREM.STR` | 320×240 | v2 | 76 | 15.0 | |
| Oddworld | `/F1.MOV` | 368×240 | v3 | 889 | 15.0 | |
| Ape Escape | `/DEMO/A_HA_D1.STR` | – | – | – | – | polygon stream, refused |

Every decoded frame in these files ended with the end-of-frame code at
the expected macroblock count, with no Huffman errors. The PNGs were
inspected by eye: clean images, correct colours, no block errors.

## Cross-check against the emulator

The native core plays these movies through the game's own software decoder
running on the emulated CPU, feeding the same MDEC model. Matching its screen
therefore checks the bitstream decoder, table setup and macroblock layout
against the game's own code. Snapshots every 10 frames:

```
psx native --bios bios/scph7502.bin --disc "media/Croc - Legend of the Gobbos.cue" \
  --frames 1500 --dump .psx/str/emu_croc/croc --snapshot-every 10
```

- **Croc, FOXLOGO.STR** (emulator frames 1230-1490 against decoded frames 1, 2, 5, …, 77):
  - Every snapshot's best match is the expected frame. The movie advances 3 decoded frames per 10 video frames: 15 fps on a 50 Hz PAL display.
  - At 8 bits per channel the PSNR is 42.6–43.1 dB (max |Δ| 7). Croc decodes at 15 bpp; after truncating both images to 5 bits per channel, 100 % of pixels are identical in every snapshot.
- **Tomb Raider, CORELOGO.FMV** (emulator frames 1110-1490, decoded 1, 4, 7, …, 115):
  - Bit-exact at 8 bits (100 % of pixels identical; Tomb Raider decodes and displays in 24 bpp).
  - The 320×208 movie sits at y = 16 in the 320×240 display.
- **Audio:** `audio.wav` is byte-identical (`cmp`) to `psx xa … --channel N -o` for Croc FOXLOGO (ch 1), Tomb Raider CAFE (ch 1) and Mega Man 8 CAPCOM15 (ch 1). It is the same code.

Because both paths use `psx_core::mdec`, this does not test the MDEC model
against real hardware. The model's known approximations (IDCT rounding,
floating-point YUV→RGB; see `mdec.rs` and psx-spx) apply to both.

## Open

- AN2 (Croc cutscenes), polygon streams, iki/v0/EA/FF9/encrypted variants are not decoded.
- `--frames` limits only the PNGs; `audio.wav` is always the whole channel.
- Dimensions not a multiple of 16 are cropped from the decode buffer. None of the movies tested have them.
- `y4m` always declares 15 fps. Use `video.fps.fps` from the summary when muxing.

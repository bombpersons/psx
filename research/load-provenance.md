# Load provenance: CD sectors → RAM (Croc PAL, first 2400 vsyncs)

Run:

```bash
psx run "media/Croc - Legend of the Gobbos.cue" --bios bios/scph7502.bin --frames 2400 --trace-cd --trace-bios
psx loads <run_dir> --disc "media/Croc - Legend of the Gobbos.cue"
```

The traced run uses the interpreter with the debugger and takes about 39 s of wall time.

## Method

1. **Tracing.**
   - **CD controller writes:** Lua write breakpoints on `0x1F801800–3` record the CD controller register writes. The index register is tracked, not logged.
   - **DMA3 writes:** `MADR`/`BCR`/`CHCR` writes are logged too.
   - **Store values:** Redux breakpoints report no value, so each one is decoded from the store instruction at the PC (`sb`/`sh`/`sw` source register).
2. **Payload snapshot at the DMA instant.**
   - When `CHCR` starts a transfer, a one-shot write breakpoint is placed over `[MADR, MADR+len)`.
   - Redux fires it with cause `DMA channel 3 write` right after copying the data-FIFO bytes. The first 64 bytes and an Adler-32 of the payload are recorded.
   - The breakpoint disables itself and is removed at the next vsync, because removing it inside its own callback is unsafe.
3. **Decoding.**
   - Setloc parameters (BCD MSF), Setmode (sector size bit 5) and ReadN/ReadS give a counted sector position.
   - Each payload is then searched by content: sectors within ±32 of that position, at plausible FIFO offsets (0, 12, 24, or the counted offset).
   - Evidence per transfer is `content`, `ambiguous` or `inferred`.
4. **Mapping.**
   - Sectors map to ISO9660 files through `psx-media`.
   - Runs are compared with the final RAM dump (`still_in_ram_bytes`).

Result: of 4937 CD DMA transfers, **4936 were identified by content**, 1 was ambiguous and 0 were inferred.

## Findings

| Frames | File | Initiating PC | Destination |
|---|---|---|---|
| 509–523 | licence sectors 4–11 | `0x80067770` (BIOS shell code in RAM) | `0xA0010000` |
| 733–2361 | PVD/directory sectors | `0xBFC06618` (BIOS ROM CD routine) | `0xA000B070` |
| 805 | `/SYSTEM.CNF` | BIOS | `0xA000B070` |
| 885–918 | `/SLES_005.93` | BIOS | header → buffer; 124 KB → `0x80100000–0x8011F000` (= EXE `T_ADDR`) |
| 1043–1068 | `/WARNINGH.IMG` | `0x801092D8` (loader's libcd) | sector headers → `0x8010D600`; data → `0x80122DA0–0x801485A0` |
| 1356–1799 | `/FOXLOGO.STR` | `0x8010B940` (loader's streaming code) | ring buffer `0x8017ADA0…` |
| 1819–2239 | `/ARGOLOGO.STR` | `0x8010B940` | same ring buffer |
| 2242–2267 | `/COPYRGHT.IMG` | `0x801092D8` | same buffer as WARNINGH; 153,976 of 154,500 bytes still intact at frame 2400 |
| 2374–2399 | `/CROC.EXE` | BIOS, via `A(42h) Load("cdrom:\CROC.EXE;1")` at frame 2350 | → `0x80010000` (= its `T_ADDR`); still loading at the end of the run |

The BIOS trace confirms that the boot chain is `SYSTEM.CNF` → `SLES_005.93` (loader at `0x80100000`) → `Load(CROC.EXE)`.

## Caveats

- **Redux FIFO behaviour.** In 2340-byte mode the loader DMAs each sector's 12-byte header and then its 2048 data bytes. Content matching shows that the data sometimes comes from sector *N+1* after the header of sector *N*: re-requesting data reloads the FIFO with the newer sector in Redux. This describes what happened in this Redux run. Whether real hardware behaves the same is unverified.
- **CPU-only fields.** Other DMA channels (GPU, SPU, MDEC) are not traced yet. Watch hits label them `writer: dma` with no CPU PC.

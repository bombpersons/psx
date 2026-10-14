# Croc (PAL, SLES-00593): level 1-1 load

How the states were reached, using `tools/probes/step.py` and the interpreter backend:

1. Boot for 2700 vsyncs to the title screen: `.psx/states/croc_title_i.bin`.
2. Start pressed at frames 200 and 400 (+900 vsyncs): main menu.
3. Cross pressed (+900 vsyncs): island map, "1-1 And So The Adventure Begins".
4. Cross pressed, then 1200 vsyncs with `--trace-cd --trace-bios`: gameplay in level 1-1. Saved as `.psx/states/croc_level1.bin`.

The states are local (`.psx/`, ignored). They are bound to the Redux build and BIOS recorded in each run's identity.

## CROCFILE.DIR format (confirmed against the load trace)

```
u32 count                       // 648
count × { char name[12];        // NUL-padded, e.g. "CROC.WAD"
          u32 size;             // bytes
          u32 offset;           // into CROCFILE.1
          u32 zero; }           // always 0 on this disc
```

The entries are contiguous with no gaps, and the last one ends at 87,088,637 bytes, which is exactly the size of `CROCFILE.1`.

## What starting 1-1 loads

`psx loads` reports 848 CD DMA transfers: 845 identified by content and 3 inferred. All of them come from
`CROC.EXE`'s CD routine (`pc 0x80069CC8`) and go through a 32 KB staging buffer at `0x801F0000`
during frames 49–433 after Cross is pressed, 1.7 MB in total. The data is therefore copied or decompressed
from the staging buffer elsewhere. Finding where requires watching reads of `0x801F0000`, which is the next step.

These entries were read in full (entries that show only a few hundred bytes are sector-alignment spill):

| Frame | Entry | Size |
|---|---|---|
| 49 | LOAD.BIN | 80,723 |
| 59 | MAGMUS.BIN | 288 |
| 67 | MAP10.WAD | 14,028 |
| 74 | CROC.WAD | 678,104 |
| 185 | TK05_ENM.BIN | 270,649 |
| 244 | TK05_TRK.BIN | 116,951 |
| 260 | TK05_CAM.MOD | 11,460 |
| 283 | MP010_00.WAD | 343,800 |
| 380 | TB08.BIN | 73,805 |
| 426 | AMBIBIRD.BIN | 41,083 |

The following were read in part, from their first byte: MAP1.BIN (14,720 of 47,488), MAP11.WAD (8,481 of 10,388),
MP010_01.WAD (32,042 of 234,756) and TB09.BIN (31,081 of 94,447). Whether these are deliberate
partial reads or the reads were still in progress is open.

The roles in the following hypotheses come from the names only and are unverified: `CROC.WAD` holds the player model, `TK05_*` holds the
track/level data (geometry, enemies, camera), `MP010_*` is map data and `AMBI*` holds ambient sound.

# PS1 archive format survey (2026-10-15)

**Implementation update:** the first DOT-T/shared-entry task is complete. See
[archive-support.md](archive-support.md) for behavior, validation and measured effort.

The catalogue contains **204 distinct named games, 283 evidence rows and 281
distinct game/family pairs**. DOT1 has the largest explicitly identified reuse,
including archives nested inside custom outer containers. These are counts in a
curated documentation sample, **not estimates of prevalence across the PS1
library** or counts of formats our toolkit supports.

## Evidence and counting

The main source is the authored reverse-engineering reference
[PSX-SPX's archive chapter](https://psx-spx.consoledev.net/dev/ps1/cdr/cdromfileformats/archives/).
The catalogue follows its game-specific subsections, expands explicitly named
games in shared-format lists, and checks nested-container descriptions. Independent
implementation documentation supplies FF7 and additional Argonaut examples:
[ff7tools](https://github.com/cebix/ff7tools),
[PS1-Argonaut-Reverse](https://github.com/OverSurge/PS1-Argonaut-Reverse/blob/master/Documentation/WAD.md).
King's Field comes from the local sibling study's
[feedback](../../external_agent_tests/kings_field/PSX_FEEDBACK.md).

- Count a game once per family/category, regardless of file count, discs, regions,
  repeated demos or evidence rows. Count sequels separately; normalize regional
  aliases such as Kula World / Roll Away and spelling variants.
- Include outer and nested archives. Group documented variants into families for
  planning; this does not promise compatibility with one undifferentiated parser.
- Separate compression from container layout. FF7's LZSS archive has a DOTLESS
  table; payload handling is still FF7-specific. This grouping is based on the
  [extractor code](https://github.com/cebix/ff7tools/blob/master/ff7/binlz.py).
- Exclude unidentified `MAG 3`, demo menu archives, the Lightspan connection
  utility, and unenumerated claims such as "various other games". Headerless
  concatenation without a named example is not counted. Alien Resurrection is
  excluded from Argonaut WAD because the source describes that link tentatively.

This is a curated catalogue, not an exhaustive extraction of every incidental
mention. Frequencies are conservative documented examples in the catalogue.
The source favors magazine demos, understood formats and games with active
reverse-engineering communities. A missing entry means unknown coverage.

Each [CSV row](archive-survey.csv) records the game, family, structural category,
release evidence, main-checkout overlap, source section and evidence note.
`release_evidence` describes the observation (`demo`, `retail`, `mixed`, or
`unspecified`), not release exclusivity. Relative source paths resolve from the
toolkit root. The CSV is the maintained factual catalogue.

## Structural frequency

Categories describe the main representation of each catalogued format. They are
engineering groupings, not interchangeable file formats. Games can contribute
to several rows, so shares do not sum to 100%.

| Representation | Games | Share of 204-game sample |
|---|---:|---:|
| Named directories, embedded or external | 76 | 37.3% |
| Offset and size tables, outside the named-directory category | 57 | 27.9% |
| Offset tables, sizes inferred from boundaries | 44 | 21.6% |
| Sequential chunks / tagged sections | 32 | 15.7% |
| Dedicated folder structures | 24 | 11.8% |
| Size tables with sequential placement | 13 | 6.4% |
| Paged / custom streaming containers | 5 | 2.5% |
| Indexes outside ISO9660 files | 3 | 1.5% |

The first six categories follow the source's organization, correcting the `PX`
`.RR` family to offset/size despite its placement in the source's size-table
section. Dedicated complex formats receive an explicit category. Classification
does not imply successful decoding.

## Repeated format families

Full membership and source links are in the CSV and script's `--json` output.
These counts include variants and nested uses, subject to the rules above.

| Family | Games | Examples / scope |
|---|---:|---|
| DOT1 | **21** | R-Types, Rayman, FF9; nested in MGS, Xenogears, GT2 and others |
| WAD.WAD | **11** | Crash 3, CTR, Spyro 2/3, Madden 98; excludes unrelated WAD layouts |
| DOTLESS | **8** | Ape Escape, Hot Shots Golf 1/2, MGS; FF7's offset-table container |
| EA BIGF | **7** | PGA Tour 96/97/98, FIFA 98/2000, NFS III, WCW Mayhem |
| IFF | **7** | Forsaken, Viewpoint, Vigilante 8; custom chunk payloads |
| Named HED/WAD | **6** | Spider-Man 1/2, THPS, Apocalypse, MDK, old Mat Hoffman demo |
| Argonaut sectioned WAD | **5** | Croc 2, Emperor, Aladdin, Harry Potter 1/2; differing section sets |
| Sony FIL | **5** | NFL Gameday 98/99/2000, NCAA Gamebreaker 98/2000; 16/32-bit variants |
| Gex 2-style BIGFILE.DAT | **4** | Gex 2/3, Akuji, Disney racing; sorted/unsorted variants |
| Little-endian IFF variant | **4** | Thrasher, Dave Mirra, Colony Wars and Vengeance |
| PKG | **4** | Pandemonium II, Herc's Adventures, Unholy War, 102 Dalmatians |
| Argonaut DIR/DAT | **3** | Croc 2, Emperor, Aladdin |
| Argonaut FESOUND | **3** | Croc 2, Emperor, Aladdin |
| Crash NSD/NSF | **3** | Crash 1/2/3; version-specific paged format |
| Ridge Racer TIM chunks | **3** | Ridge Racer, Revolution, Type 4 |
| DOT-T | **2** | King's Field SCES-00510 and Armored Core demo |
| PaRappa INT | **2** | PaRappa and Um Jammer Lammy |
| GT-ARC | **2** | Gran Turismo 1/2 |
| Dino Crisis dummy-header containers | **2** | Dino Crisis 1/2 |
| Croc 1 DIR | **1** | Only Croc is positively assigned |

Other two-game families are retained in the CSV/script. Single-game labels are
evidence identifiers; their number is not a count of incompatible binary formats.
Further review may merge or split those labels.

DOT1's 21 games are Ace Combat 2/3, Demolition Racer, FF9, GT2, Incredible Crisis,
Klonoa, Legend of Mana, MLB 2000, MGS, Nightmare Project: Yakata, Pandemonium II,
Project Horned Owl, R-Types, Rayman, Test Drive 5, Test Drive Off-Road 3, Threads
of Fate, Witch of Salzburg, X-Men: Mutant Academy and Xenogears. Nested-container
descriptions account for many of these; every assignment has a source section.

## Local overlap

Nine catalogue titles overlap the **19 CUE images in the main checkout**. All 19
were successfully listed using the existing CLI. Candidate filenames for the nine
were checked against those listings: availability is verified, binary format
conformance is not. Results are in
[archive-survey-local.json](archive-survey-local.json); large candidate lists are
explicitly truncated.

| Local game | Candidate files present | Survey relevance |
|---|---|---|
| Ape Escape | `KKIIDDZZ.HED/.DAT/.BNS`, `MINIGAME/*` | Outer reader plus DOTLESS candidates |
| Crash Bandicoot | `S0/*.NSD`, `S0/*.NSF` | Version-specific paged reader |
| Croc | `CROCFILE.DIR`, `CROCFILE.1` | Existing reader; shared entry/provenance API |
| FF7, disc 1 | `INIT/KERNEL.BIN`, `INIT/WINDOW.BIN`, `MOVIE/OPENING.BIN`, `MOVIE/STAFF*.BIN` | GZIP-member and DOTLESS/LZSS inputs |
| Gran Turismo | `BG.DAT`, `COURSE.DAT`, other `.DAT` files | Named/GT-ARC candidates; check demo variants |
| MGS, disc 1 | `MGS/STAGE.DIR` | Outer stage loader and nested DOT1/DOTLESS |
| PaRappa | `S0/COMMON.INT`, `S0/COMPO00.INT`, other `.INT` files | INT reader; existing asset-scan evidence |
| Ridge Racer | `TEX0.TMS` through `TEX4.TMS` | Small sequential TIM-container target |
| Spyro the Dragon | `WAD.WAD` | Source documents demo `PETE.WAD`; check retail layout |

King's Field's sibling study supplies a tenth locally available survey title and
the already studied DOT-T examples. It is outside the main-checkout inventory.

The other ten local titles are Castlevania SOTN, Mega Man 8, Oddworld: Abe's
Oddysee, Pandemonium!, R-Type Delta, Resident Evil, Tekken 3, Tomb Raider, Vagrant
Story and WipEout. Some have embedded-asset evidence in [assets.md](assets.md);
this survey does not assign an archive family without format evidence. Do not
substitute Oddysee for Exoddus, Pandemonium! for II, R-Type Delta for R-Types,
or WipEout for 2097.

## Toolkit implications

Keep DOT-T first: concrete local evidence makes it useful for King's Field's
geometry/texture work. Give readers a shared entry representation with reader
identity, numeric index, optional name, parent provenance, byte span and
format-specific payload/compression metadata.

After DOT-T, the survey supports this reader order:

1. **DOT1 and DOTLESS** with validated variants: largest documented reuse and
   local nested/FF7 candidates.
2. **WAD.WAD and named HED/WAD** as separate readers. Keep PKG's folder/unused
   entry handling explicit despite similarities to WAD.WAD.
3. **EA BIGF and IFF**: repeated documented uses; obtain validation inputs before
   claiming support. PS1 archive fields are not universally little-endian.
4. **PaRappa INT and Ridge Racer TIM chunks** as small local validation targets;
   then dedicated readers driven by an active game study.

This recommendation does not reorder the agreed five-item queue. Frequency alone
does not determine priority; actual bytes, variants, nesting and compression also
matter. Prefer bounded recursive traversal with explicit readers to guessing that
every plausible integer list is an archive.

## Extension policy after the survey

The agreed direction is to keep shared indexes, provenance, standard decoders
and exporters in core, and supply game-specific parsers as explicitly selected
executables. The toolkit now supports an external archive index followed by an
external model decoder or standard-asset wrapper adapter. Generate runnable
starters with `psx template parser DIR --kind archive|model|asset|video`.
See [the protocol](../docs/external-parsers.md) and
[implementation evidence](parser-extensions.md).

The frequency ranking is evidence for choosing reusable built-ins, not a promise
to implement the entire catalogue. Promote a parser to core when independent
games or active studies demonstrate reuse, its variants are understood, and
maintaining a shared reader is useful. Existing DOT-T and Croc behavior remains.
The estimates below record the original survey assessment before that delivery.

## Implementation size

Assessment against the current code on 2026-10-15: the individual table readers
are mostly small. The first integrated change is medium-sized because it also
establishes the shared entry API and consistent scan/extract provenance. Supporting
every surveyed family and variant would be a substantial ongoing effort.
These were engineering estimates at survey time. The subsequent DOT-T delivery
and its measured session effort are recorded in [archive-support.md](archive-support.md).

| Work package | Estimated size | Main work / uncertainty |
|---|---|---|
| Shared entries plus DOT-T, integrated into CLI/MCP scan/extract | Medium | Numeric entries, byte bounds, empty entries, provenance and malformed-input fixtures; King's Field inputs available |
| DOT1 reader after that foundation | Small–medium | Small core table parser; alignment, end offsets, empty entries and detection ambiguity need explicit variants |
| DOTLESS reader after that foundation | Small–medium | Inferring table length and avoiding false positives are harder than reading offsets; FF7 decompression is additional work |
| Bounded nested traversal | Medium | Depth/entry/byte budgets, parent entry paths, duplicate scanning and selection/extraction semantics |
| WAD.WAD or named HED/WAD, each | Small–medium | Separate readers; padding, unused entries, external index resolution and release variants |
| EA BIGF or generic IFF container, each | Small–medium | Directory/chunk parsing; local validation inputs still needed; game-specific IFF chunk contents are separate |
| A specified compression wrapper/codec | Medium, codec-dependent | Bound decoded size; retain compressed-source provenance and distinguish container support from payload decoding |
| PaRappa INT or Ridge Racer TIM container, each | Small–medium | Focused local targets; validate exact retail layouts before fixing scope |
| Crash NSD/NSF, MGS stages, other custom streaming/folder systems | Medium–large, per family | More specialized semantics, versions, relocation/loading behavior or coupled content formats |

The existing [asset command layer](../crates/psx/src/assets_cmd.rs) already resolves
disc/host inputs, associates Croc directory entries with asset hits and writes
extraction reports. It currently scans whole byte buffers and stores only a flat
`(name, offset, size)` index. General entries, nesting and decoded buffers need a
clearer provenance model. The [asset scanner](../crates/psx-assets/src/scan.rs)
already validates TIM/TMD/VAB/VAG payloads, so ordinary uncompressed archive readers
can reuse those decoders. Compressed or custom payload formats need more work.

A bounded first delivery is DOT-T plus shared entry/provenance handling, preserving
existing scan/extract behavior and Croc coverage, with authored malformed tables
and the existing King's Field archives as validation. Add later readers as
separate reviewable changes. Raw entry listing/extraction and decoding every asset
inside an entry are distinct completion criteria.

## Reproduction and validation

```sh
python3 tools/archive_survey.py
python3 tools/archive_survey.py --json
# Optional: check source fragments against a downloaded reference page.
python3 tools/archive_survey.py --reference-html /tmp/psx-archive-survey.html
```

The script validates columns, required values, evidence uniqueness, classification
values and source links, then counts sets of games. Supplying reference HTML also
checks every PSX-SPX fragment. It does not fetch sources, inspect binaries or
mutate the catalogue. Its JSON output includes the CSV SHA-256.

The reference HTML downloaded on 2026-10-15 had SHA-256
`83660ccaa9897cb5a69c0a5ca6c83d5c54a2d9e869d31c6b8da5165ed11d76df`;
the source page is not vendored. Catalogue checks and source-fragment checks pass.
All 19 local `psx ls` calls completed without warnings. The survey itself changed
no production behavior; the subsequent archive implementation is documented above.

The port's assets come from the player's disc through tools/import (the
overview's Assets: formats, importers, checks). These commands find and read
what the disc holds.

## The disc

disc_info gives tracks, volume, SYSTEM.CNF and the boot EXE (and the
track hashes that identify the release); disc_ls lists entries with
filters and paging; disc_exe parses an EXE (image.cue::/PATH). disc_extract
writes a file, or with offset and length returns just those bytes inline
(hex up to 0x100) without extracting a large volume file; Form 2 files come
out as raw 2352-byte sectors (.raw). disc_sector describes sectors: form,
XA subheader, owning file. Images: .cue with its .bin, a raw .bin, .iso or
CD .chd. LibCrypt discs need the redump .sbi or .lsd beside the image.

## Standard formats

assets_scan lists TIM, TMD, VAB and VAG found by signature and validation;
narrow it with source and filter. No hits does not mean the game has no
textures, models or sounds: most games use their own formats.
assets_extract converts what scan finds (TIM to PNG per CLUT, TMD to glTF,
VAB and VAG to WAV), keeping the original bytes and provenance beside each;
brief, limit and report_offset page it. TMD's skipped_primitives says what
geometry was left out.

Streams: XA audio and STR movies are listed under coverage.skipped_files;
assets_extract on one decodes STR to frame PNGs, audio.wav and a contact
sheet, XA to one WAV per channel.

Archives: DOT-T sector archives and Croc-style sibling .DIR indexes are
recognised; files[].archive lists each entry, and entry:N extracts one raw.
The vendored psx_assets::archive reads the same layouts for the importer's
import::index.

Other formats need evidence from the code that reads them: trace_cd (topic
run) finds which run loads a file and where it lands in RAM; a watch on that
RAM finds the parser; its disassembly is the layout, written in the format's
doc. assets_scan and assets_extract also take external parsers
(--archive-parser, --asset-parser, --model-parser, --frame-parser; see their
help) for a quick look while investigating; the port's importers are Rust
in tools/import.

## What the game built

run with gpu_frame captures one frame's GPU commands; gpu_next_draw searches
for a frame that draws (fills count: gpu_draw_filter primitives or textured
narrows it). assets_textures decodes the captured frame's VRAM through its
CLUTs into PNGs and a .psxscene for the template's --scene viewer: screen
drawing, not world geometry. assets_diff compares two vram.bin dumps. These,
and RAM after a load, are what an importer's test compares against.

Sounds: trace_spu logs which voice keyed which sample; its sample_sha256
equals assets_extract's adpcm_sha256 for the waveform in a VAB, naming the
bank behind a sound (banks share waveforms, so one may match several).

assets_memcard lists the saves on a .mcd.

## Which assets

How much of the game is extracted is the person's choice. An asset name is
a disc path or archive entry (/MAPS/*.WLD, /CROCFILE.1:CROC.*; * and ?
within a name) or format:<id>: every file of a format doc's id, or of a
standard format (format:TIM). goal.toml's assets is true (every file),
false, or a list of names (["format:level", "format:car"]); taking a name
out needs the person to confirm again. A catalogue entry's assets names
what its recordings need to replay (a level's geometry), brought in
whatever the goal says. port_next gives a feature's before its own work and
the goal's after the features: Format (a format: id no doc has yet, an
entry of a file no doc calls an archive, or a file with no format), Index,
Import and Check. A name that matches nothing is a problem. port_status
counts the needed files; port_status assets=NAME lists the files a name
covers, each with its format and state.

The port's assets come from the player's disc through tools/import: each
file's state is below. These commands find and read what the disc holds.

## Asset states

Each disc file but the code is:
- unknown;
- format found: a formats/<id>.md doc names it (TOML front matter: files
  as globs, loader as the code that parses it; then the layout;
  formats/README.md), or assets_scan finds it whole as TIM, TMD, VAB or VAG,
  or it is a stream of XA and STR sectors only;
- imported: the last port_check ran the importers (tools/import) on the disc,
  and an output's Provenance::of_disc names it;
- checked: a passing importer test marked it with import::checking after
  comparing the import with what the game built (RAM after loading, VRAM
  uploads, a GPU capture);
- or unused: formats/unused.toml, with the runs that never load it.

An archive's format doc says archive = true: the importer lists its entries
(import::index), which are then the assets, named /ARCHIVE:NAME (with @0xOFF
where names repeat) or /ARCHIVE@0xOFF. port_status counts them always, with
how many no traced run (trace_cd) loaded.

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
(the *_parser arguments, each with its *_args; see their
descriptions) for a quick look while investigating; the port's importers are its
own (tools/import on the template).

## What the game built

The debugger captures a field's GPU commands (topic debug): debug_go gpu
the first whole field from its stop, debug_go draw stops at the end of the
first field that draws and captures it (any counts fills; primitives or
textured narrows it). debug_show gpu gives the capture and its directory;
assets_textures with that directory as run_dir decodes the field's VRAM
through its CLUTs into PNGs: screen drawing, not world geometry. assets_diff compares two vram.bin dumps. These,
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
false, or a list of names (["format:level", "format:car"]). A scenario's assets (in its toml)
names what its recordings need to replay (a level's geometry), brought in
whatever the goal says. A name that matches nothing is a problem. port_status
counts the needed files; port_status assets=NAME lists the files a name
covers, each with its format and state.

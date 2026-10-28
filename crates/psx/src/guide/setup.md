Call context with an image path, or from_state for a checkpoint. Read build,
project_root, resolved, capabilities and warnings.
guide itself only returns documentation; it does not validate the installation.

All relative input and output paths resolve against the server's project root:
--root/PSX_ROOT if set, else the client's single workspace root, else the server's
working directory. context reports the root in use. Use project_root on an MCP
call to select a different existing study directory. Each scoped call runs in an
isolated process; returned continuation calls retain that root. It does not
change other calls or the server default. A server started with
mcp --require-project-root refuses calls that omit project_root.
context(save:true) persists defaults in the selected root's .psx/project.json: use it
deliberately, since other clients sharing the root see those defaults too.

Moving a project: the files psx keeps (saved defaults, state chains, case
recipes) name what lies inside the project relative to its root, so the
folder can move. Files written by an older psx, and run and handoff
reports, keep absolute paths; one that no longer exists is found again by
its trailing components under the root (.psx/states/a.bin, samples.toml),
or beside it (rom/game.cue for a disc kept next to the project). states
rebuild and case check write a chain's disc and a recipe's paths back
relative; context save does the same for the defaults. A chain checks each
state's hash, so a state found wrongly stops it (chain_broken) rather than
passing.

bios_list inventories installed images. bios_install can import a local dump,
or with download:true fetch the pinned image for a region (psx never downloads
on its own: call it yourself). Use a BIOS of the disc's region (context reports
disc_region_hint): the BIOS makes the console, so a PAL disc on an NTSC BIOS
boots but runs on an NTSC console (its GPU clock and BIOS), not the one the
game was made for. When none of that region is installed, install it with
bios_install(region, download:true) before recording anything; do not record
on another region's BIOS instead. context warns when the selected BIOS's
region differs.
Native checkpoints require the exact BIOS/media identity they saved.
sha256 identifies the BIOS file; native_sha256 identifies the 512 KiB ROM image
used by the native core, so they can differ for padded/truncated inputs.
BIOS discovery checks the selected project's bios/ and the shared store, not
sibling toolkit checkouts. Import a known local dump once with bios_install(from),
set an absolute PSX_BIOS_DIR, or save an explicit context bios path for the project.
Signature matching uses explicit symbols.sigs, otherwise PSX_SIGS_DIR, otherwise
external/psx_psyq_signatures in the selected project, otherwise the installer's
shared copy (context reports signatures.path and selection). Use an absolute PSX_SIGS_DIR
in the server environment to share a checkout. context.signatures reports its
default path and directory presence without loading or downloading the index.

Disc images can be .cue (with its .bin files), a bare raw .bin, a cooked .iso
or a CD .chd; a .chd and the .cue it was made from give the same sectors,
track_sha256 (disc --hash) and save-state identity.
disc inspects the disc and boot EXE; ls filters/pages its ISO entries.
LibCrypt-protected discs need the redump .sbi/.lsd beside the image (same base
name); disc reports subchannel.source and the native core honours it. exe,
disasm and asset tools accept disc.cue::/PATH where documented. run accepts a
disc or host EXE path; do not assume every tool accepts the disc-member syntax.

Boot and menu duration vary by game; use snapshot_every and save_state to locate a
useful starting point instead of assuming a universal gameplay frame number.

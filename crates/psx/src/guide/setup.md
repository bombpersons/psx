Call context with an image path, or from_state for a checkpoint. Read build,
project_root, resolved, capabilities and warnings before choosing a backend.
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

bios_list inventories installed images. bios_install can import a local dump;
download:true is an explicit setup action, never an automatic part of inspection
or running. Native checkpoints require the exact BIOS/media identity they saved.
sha256 identifies the BIOS file; native_sha256 identifies the 512 KiB ROM image
used by the native core, so they can differ for padded/truncated inputs.
BIOS discovery checks the selected project's bios/ and the shared store, not
sibling toolkit checkouts. Import a known local dump once with bios_install(from),
set an absolute PSX_BIOS_DIR, or save an explicit context bios path for the project.
Signature matching uses explicit symbols.sigs, otherwise PSX_SIGS_DIR, otherwise
external/psx_psyq_signatures in the selected project. Use an absolute PSX_SIGS_DIR
in the server environment to share a checkout. context.signatures reports its
default path and directory presence without loading or downloading the index.

disc inspects the disc and boot EXE; ls filters/pages its ISO entries. exe,
disasm and asset tools accept disc.cue::/PATH where documented. run accepts a
disc or host EXE path; do not assume every tool accepts the disc-member syntax.

Native is the default investigation backend. Redux is a separately installed
oracle with fewer observation options and incompatible checkpoint files. Boot
and menu duration vary by game; use snapshot_every and save_state to locate a
useful starting point instead of assuming a universal gameplay frame number.

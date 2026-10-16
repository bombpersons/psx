Call context with an image path, or from_state for a checkpoint. Read build,
project_root, resolved, capabilities and warnings before choosing a backend.
guide itself only returns documentation; it does not validate the installation.

All relative input and output paths resolve against the server's project root.
Use project_root on each MCP call to select an existing study directory. Each
scoped call runs in an isolated process; returned continuation calls retain that
root. It does not change other calls or the server default. Alternatively, start
a dedicated server with psx --root /absolute/study mcp (CLI also accepts PSX_ROOT).
context(save:true) persists defaults in the selected root's .psx/project.json: use it
deliberately, since other clients sharing the root see those defaults too.

bios_list inventories installed images. bios_install can import a local dump;
download:true is an explicit setup action, never an automatic part of inspection
or running. Native checkpoints require the exact BIOS/media identity they saved.
sha256 identifies the BIOS file; native_sha256 identifies the 512 KiB ROM image
used by the native core, so they can differ for padded/truncated inputs.

disc inspects the disc and boot EXE; ls filters/pages its ISO entries. exe,
disasm and asset tools accept disc.cue::/PATH where documented. run accepts a
disc or host EXE path; do not assume every tool accepts the disc-member syntax.

Native is the default investigation backend. Redux is a separately installed
oracle with fewer observation options and incompatible checkpoint files. Boot
and menu duration vary by game; use snapshot_every and save_state to locate a
useful starting point instead of assuming a universal gameplay frame number.

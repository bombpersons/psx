# BIOS setup

BIOS setup is a one-time operation shared by projects. `context` (which lists the
installed images and the download catalog) and ordinary emulation calls only inspect
local files. Downloads happen only through
an explicit `bios install --download` / MCP `bios_install({download:true,...})` call.
No firmware is bundled with psx.

```bash
psx context
psx bios install --from /path/to/bios.bin
# Or explicitly fetch a pinned regional image:
psx bios install --download --region pal
psx context --image 'media/Game.cue' --save
psx run --frames 300 --save-state --brief
```

Regions are `pal`, `ntsc-u` and `ntsc-j`. The download catalog uses
[Internet Archive's the-ultimate-playstation-1-bios collection](https://archive.org/details/the-ultimate-playstation-1-bios).
The installer requires `curl`, follows HTTPS redirects only, limits the transfer
to 512 KiB and verifies the exact size and a source-pinned SHA-256 before publishing
the file. Network errors or mismatches leave no installed image. Local imports
are copied and hashed; unknown bytes remain explicitly unrecognized.

| Image | Region | SHA-256 |
|---|---|---|
| scph5500.bin | NTSC-J | `9c0421858e217805f4abe18698afea8d5aa36ff0727eb8484944e00eb5e7eadb` |
| scph5501.bin | NTSC-U | `11052b6499e466bbf0a709b1f9cb6834a9418e66680387912451e971cf8a1fef` |
| scph5502.bin | PAL | `1faaa18fa820a0225e488d9f086296b8e6c46df739666093987ff7d8fd352c09` |
| scph7502.bin | PAL | `5e84a94818cf5282f4217591fefd88be36b9b174b3cc7cb0bcd75199beb450f1` |

Downloads select the 550x image for the requested region. The catalog also
recognizes the 7502 image used by existing checkpoints. Installations use
`<sha256>.bin` names and preserve existing files; repeated local imports are
idempotent. The default shared location is `PSX_BIOS_DIR`, then
`$XDG_DATA_HOME/psx/bios`, then `~/.local/share/psx/bios` (Windows also checks
`LOCALAPPDATA/psx/bios`). `--dir` overrides the installation destination. For a
custom destination to be discovered later, set `PSX_BIOS_DIR` to it or save its
BIOS path with `context --bios PATH --save`.

Discovery checks the project's `bios/` directory and shared store. Selection order:

1. Explicit `--bios` / MCP `bios` override.
2. For native checkpoints, exact BIOS identity from the state header. Regional
   substitutes are never chosen. The inventory exposes both file `sha256` and
   `native_sha256`; these differ for authored ROMs padded/truncated to the native
   core's 512 KiB image.
3. Project preference in `.psx/project.json`, saved by `context --save`.
4. Disc/EXE region: first matching catalog image (5502 before 7502 for PAL), then
   a unique 512 KiB image with a recognized regional version marker.

Ambiguous or unknown selection returns `bios_required` with candidates and a
suggested setup action. A BIOS-only boot has no disc region, so supply a BIOS or
save a project preference. Arbitrary filenames never establish BIOS identity.

For resumed runs, a sibling `report.json` may
supply BIOS/media paths only when it contains the matching state file hash.
The core still verifies BIOS and media identity when loading the checkpoint.

`context` reports the running source build ID, workspace/output paths, selected
BIOS and selection reason, candidate hashes, backend capabilities, setup gaps and
next-call examples. `--save` writes resolved media, BIOS and backend to
`.psx/project.json`; inspection alone does not change the project. The build ID
also appears in `psx --version`, MCP initialization and run reports, so stale
server processes can be distinguished from a newly built binary.

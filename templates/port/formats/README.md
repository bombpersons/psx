# Formats

One file per format of the disc's files, named by its id: `level.md`,
`sound-bank.md`. Most of a game's files are in its own formats; psx's
decoders know only the standard ones (TIM, TMD, VAB, VAG, XA, STR), and a
file `psx assets scan` finds whole as one of those needs no doc.

```toml
+++
id = "level"
title = "Level geometry and objects"
files = ["/MAPS/*.WLD"]          # disc paths; * and ? match within a name
loader = ["0x8004A120"]          # the code that parses it, as the inventory has it
+++
The layout, from that code: the header's fields, each table, its units and
what reads it, with the addresses that show each one.
```

How to find a format: `psx run --trace-cd` writes `loads.json`, where each
file goes in RAM; a watch on that buffer gives the code that parses it;
read that code (`psx code disasm`, `decompile`).

`unused.toml` lists the files the game never loads, each with the runs that
show so:

```toml
"/DUMMY.DAT" = "no transfer from it in any traced run: boot, every level of world 1"
```

Each disc file is then unknown, format found, imported (an importer in
`tools/import` read it: its output's `Provenance::of_disc` names the file),
checked (a passing importer test compared the import with what the game
built from it, marked with `import::checking`) or unused. `psx port check`
runs the importers on the disc and their tests; `psx port status` shows the
files by state. When `goal.toml` has `assets = true`, `psx port next` asks
for each file until every one is checked or unused.

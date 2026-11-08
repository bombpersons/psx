# The catalogue

`not_ported.toml` is optional. `psx port status` lists the functions the
recordings ran that the port's code (`game/`, `tools/`) doesn't cite by
address; this file takes out the ones the port won't follow, by entry or
address range (the end excluded), with why:

```toml
"0x80010088" = "memcpy: the port has its own"
"0x80050000..0x80058000" = "libgte's helpers: the port does its own maths"
```

## Drawing

What drawing shows is checked through quantities, not screens: the
camera, animation and the HUD have their quantities in the glossary and
their recordings. `psx scenario show --screens` puts both games' screens
side by side for you to look at.

## The goal

`goal.toml` holds the person's description, the assets to extract and
their plays held out. A scenario's toml names the assets its recordings
need (`assets = [...]`, as `formats/README.md` says).

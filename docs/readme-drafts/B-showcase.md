<!-- Draft B: showcase first. A hero image, the two ports, then how. -->

<div align="center">

<!-- TODO: a wide hero banner, e.g. a 16:9 tr2-rs shot with the logo text over it -->
<img src="docs/images/hero.png" width="100%" alt="psx: a PlayStation game running as a native widescreen port">

# psx

**PS1 games → native, moddable Bevy ports, written by your agent.**

[Install](#install) · [How it works](#how-it-works) · [Ports built with psx](#ports-built-with-psx) · [Docs](README_FOR_AGENTS.md)

</div>

psx is a reverse-engineering toolkit plus a port template. Your agent uses the toolkit to find out
exactly how a game works, and the template to rebuild it as a clean engine, the game on top, a level
editor and an example mod. The renderer, widescreen, high resolution, controls and testing come
with the template, so the agent spends its time on the game.

## Ports built with psx

<table>
<tr>
<td width="50%" valign="top">

<!-- TODO screenshot -->
<a href="https://github.com/bombpersons/tr2-rs"><img src="docs/images/tr2-rs.png" alt="tr2-rs: Lara at the Great Wall, widescreen"></a>

### [tr2-rs](https://github.com/bombpersons/tr2-rs)
Tomb Raider II as a reusable engine plus the game. Movement, traversal, swimming, cameras and
room rendering checked against the original, with a full-resolution level editor.

</td>
<td width="50%" valign="top">

<!-- TODO screenshot -->
<a href="https://github.com/bombpersons/gt2-rs"><img src="docs/images/gt2-rs.png" alt="gt2-rs: the B-1 licence course in chase view"></a>

### [gt2-rs](https://github.com/bombpersons/gt2-rs)
Gran Turismo 2. Course rendering, cameras and a car physics model built piece by piece from
original recordings, with a hand-made circuit as the example mod.

</td>
</tr>
</table>

Both repos ship source only. Each player bakes the assets from their own disc.

## How it works

```
 your disc ──► psx toolkit ──────────► evidence ──► port workspace
               emulator you can query               ├─ engine/        reusable, no game specifics
               Ghidra, disc & asset decoders        ├─ game/          the title, tick-exact tests
               RAM search, watches, traces          ├─ mods/example/  a new game built on game/
                                                    ├─ editor/        opens & saves engine formats
                                                    └─ psx-bevy/      renderer, menus, test harness
```

The workspace layout is the trick. The agent has to keep a game-agnostic **engine**, an **editor**
for its formats and a working **example mod** alongside the port. Hard-coding the original breaks
one of them, so the port stays data-driven and extensible by construction.

## The renderer you don't have to write

<!-- TODO: side-by-side of the same scene at native 4:3 and 16:9 2× -->
<img src="docs/images/template-demo.png" width="49%" alt="The template demo at native resolution"> <img src="docs/images/template-widescreen.png" width="49%" alt="The same scene in widescreen at 2×">

- Authentic PS1 rendering: vertex wobble, affine textures, 15-bit colour, dithering, blend modes.
- **Widescreen** that follows the window shape, and **2× / custom resolutions**, from a display menu.
- Each effect can be switched off for a clean modern look.
- Remappable controls, fullscreen, PAL/NTSC tick rates.
- A developer MCP to step, inspect and screenshot the port headless.
- `psx template update` keeps existing ports current.

## Install

You need your own disc images and a PS1 BIOS; **neither is included or should ever be committed**.

```bash
curl -fsSL https://raw.githubusercontent.com/bombpersons/psx/master/install.sh | sh
psx bios install --from /path/to/bios.bin
claude mcp add --scope user psx -- ~/.local/bin/psx mcp
```

Windows, building from source and BIOS downloads: see [BIOS setup](docs/bios-setup.md) and the
[agent reference](README_FOR_AGENTS.md).

Start a port: put the disc in a folder with the [example port prompt](docs/port-prompt.md) as
`AGENTS.md` and open your agent there.

## What the agent can ask

| Question | Tool |
|---|---|
| Which byte is the player's X? | `ram_search` across LEFT / idle / RIGHT runs |
| What writes it? | `watch` |
| What does that code do? | `ghidra_decompile` |
| Where did this texture, model or sound come from? | `gpu_frame`, `assets_scan`, `trace_spu`, `loads` |
| Does the port play the same? | `compare`, tick by tick |

Worked examples: [Mega Man 8](research/case-study-megaman8.md) · [Croc](research/case-study-croc.md).

## Limits

The emulator is checked against PCSX-Redux and hardware tests, but booting isn't proof of accuracy.
Custom formats need a parser. Findings are evidence to check. See [compatibility](docs/compatibility.md).

## License

MIT or Apache-2.0. Mega Drive sibling: [mdx](https://github.com/bombpersons/mdx).

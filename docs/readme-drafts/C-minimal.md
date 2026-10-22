<!-- Draft C: short and punchy. Everything else lives in linked docs. -->

<div align="center">

# psx

**Turn a PlayStation disc into a moddable native port. Your agent does the work.**

<!-- TODO screenshots -->
<img src="docs/images/tr2-rs.png" width="48%" alt="tr2-rs"> <img src="docs/images/gt2-rs.png" width="48%" alt="gt2-rs">

[tr2-rs](https://github.com/bombpersons/tr2-rs) · [gt2-rs](https://github.com/bombpersons/gt2-rs) · [Install](#install) · [Agent docs](README_FOR_AGENTS.md)

</div>

---

**1. Tell your agent:** *"Port this game with psx."*

**2. It investigates** with an emulator it can query, Ghidra, and disc and asset decoders, all over
MCP. Each behaviour it ports is backed by recorded evidence and a tick-by-tick test.

**3. When it needs a human, it hands you the controller.** Some moments are hard to reach by script:
a boss, a late level, a menu deep in the game. The agent opens a window with the game and a note
saying where it needs to get to. You play there and hand it back. It gets a save state and your
inputs, and carries on.

**4. It builds four things side by side:**

- `engine/`: a reusable engine with no game specifics, plus a sandbox game that proves it.
- `game/`: the game itself, on top of the engine.
- `editor/`: a modern desktop editor for the engine's formats, growing with the port.
- `mods/example/`: a new game built on the port without touching it.

That split is the point. The agent can't hard-code its way through, so you end up with a native
port people can mod.

**5. You get the rest free:** a PS1 renderer, widescreen and high-res modes, remappable controls and
a headless test harness, kept current with `psx template update`. The port ships no game data: players
drop their own disc next to the game, and it imports everything on first start.

<img src="docs/images/template-editor.png" width="640" alt="The template editor with the demo level open: outline, top-down view, inspector and status bar">

## Install

Bring your own disc and BIOS; **neither is included or should be committed**.

```bash
curl -fsSL https://raw.githubusercontent.com/bombpersons/psx/master/install.sh | sh
psx bios install --from /path/to/bios.bin
claude mcp add --scope user psx -- ~/.local/bin/psx mcp
```

Then open your agent in a folder with your disc and the [example port prompt](docs/port-prompt.md).
Windows and source builds: [BIOS setup](docs/bios-setup.md), [agent docs](README_FOR_AGENTS.md).

## Learn more

[Template](templates/psx-bevy/README.md) · [Workflow](docs/agent-workflow.md) ·
[Compatibility](docs/compatibility.md) · Case studies: [Mega Man 8](research/case-study-megaman8.md),
[Croc](research/case-study-croc.md) · Mega Drive sibling: [mdx](https://github.com/bombpersons/mdx)

MIT or Apache-2.0.

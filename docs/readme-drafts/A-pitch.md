<!-- Draft A: concept first, one strong pitch, then proof. -->

<div align="center">

# psx

### Point an agent at a PS1 game. Get back a moddable native port.

A PlayStation reverse-engineering toolkit and Bevy game template, built so an AI agent can turn a
disc into a real engine, a faithful port, an editor and a working mod.

[![Release](https://img.shields.io/github/v/release/bombpersons/psx)](https://github.com/bombpersons/psx/releases)
[![License](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue)](#license)
[![Bevy](https://img.shields.io/badge/Bevy-0.20-232326)](https://bevyengine.org)

<!-- TODO screenshots: tr2-rs in-game at 16:9, gt2-rs chase cam, tr2-rs editor -->
<img src="docs/images/tr2-rs.png" width="32%" alt="Tomb Raider II running natively in widescreen"> <img src="docs/images/gt2-rs.png" width="32%" alt="Gran Turismo 2's B-1 licence course in chase view"> <img src="docs/images/tr2-rs-editor.png" width="32%" alt="The tr2-rs level editor">

<sub>Built with psx: <a href="https://github.com/bombpersons/tr2-rs">tr2-rs</a> · <a href="https://github.com/bombpersons/gt2-rs">gt2-rs</a></sub>

</div>

---

## The idea

Give your agent a disc image and one instruction: *port this game with psx.*

It gets an emulator it can interrogate, Ghidra, disc and asset decoders, and a game template, all as
MCP tools. And it gets a **shape it has to fill in**, which is what keeps the result from being a
one-off hack:

| | What the agent must build | Why it matters |
|---|---|---|
| **`engine/`** | The game's engine family: object types, actions, level and tuning formats. No names, numbers or assets from the game. | Proven reusable by a tiny sandbox game that ships with it. |
| **`game/`** | The title itself, on top of the engine, with trajectory tests against the original. | Every ported behaviour is backed by recorded evidence. |
| **`mods/example/`** | A small game built on `game` from hand-made assets, without editing it. | If the mod can't be written, the port isn't flexible enough yet. |
| **`editor/`** | A modern desktop editor that opens, shows and saves every engine format. | Formats are real data, not structs baked into code. |

Because the editor and the example mod have to work at every step, the agent can't take shortcuts
that hard-code the original. What comes out is a codebase you can actually extend.

## What you get for free

- **The PS1 look, done.** Vertex wobble, affine textures, 15-bit colour, dithering and PS1 blend
  modes, while game code stays plain Bevy.
- **Widescreen and high resolution.** `--aspect window` widens the view to any window shape; the
  display menu offers native, 16:9 and 2× resolutions. Toggle the PS1 effects off if you like.
- **Remappable controls**, fullscreen, region and tick-rate handling out of the box.
- **Exact gameplay.** Replay the same input in the port and the original and compare every tick.
- **A developer MCP** for stepping, querying and screenshotting the port headless, so the agent can
  check its own work.
- **Template updates in place.** `psx template update` brings later renderer and tooling fixes into
  an existing port.

No game data goes in your repo: the disc is an asset source only, baked locally by each player.

## Get started

You need your own disc image and a PS1 BIOS. **Neither is included, and neither should ever be
committed.**

```bash
curl -fsSL https://raw.githubusercontent.com/bombpersons/psx/master/install.sh | sh
psx bios install --from /path/to/bios.bin
claude mcp add --scope user psx -- ~/.local/bin/psx mcp
```

(Windows: `irm https://raw.githubusercontent.com/bombpersons/psx/master/install.ps1 | iex`. More in
[BIOS setup](docs/bios-setup.md).)

Then put your disc image in a folder, drop in the [example port prompt](docs/port-prompt.md) as
`AGENTS.md`, and open your agent there. Or start a workspace by hand:

```bash
psx template create ../my-game --region pal
cd ../my-game && cargo run
```

## Under the hood

The agent isn't guessing. psx answers questions with evidence:

| Question | How psx answers |
|---|---|
| *Which byte is the player's X?* | Run the same moment holding LEFT, nothing, RIGHT; `ram_search` keeps what moved the right way. |
| *What code writes it?* | `watch` the address: every store, with the instruction that made it. |
| *What does that code do?* | `ghidra_decompile` the writer's PC. Import and analysis are automatic. |
| *Where did this texture or sound come from?* | `gpu_frame`, `assets_scan`, `trace_spu` and `loads` trace it back to the disc file. |
| *Does my port play the same?* | Replay the input in the port and `compare` every tick. |

The emulator is checked against [PCSX-Redux](https://github.com/grumpycoders/pcsx-redux) and
hardware tests. Case studies: [Mega Man 8](research/case-study-megaman8.md) (walk and jump, exact
for 101 ticks) and [Croc](research/case-study-croc.md).

## Honest limits

Booting a game isn't proof every system is accurate, and emulator timing work continues. Custom
formats need a parser, which the agent writes. Every finding is evidence to check, not a guarantee;
[compatibility](docs/compatibility.md) and the [validation notes](research/) say what has been tested.

## More

- [README_FOR_AGENTS.md](README_FOR_AGENTS.md): every command and flag.
- [templates/psx-bevy](templates/psx-bevy/README.md): the port workspace.
- [docs/agent-workflow.md](docs/agent-workflow.md) · [docs/ghidra-integration.md](docs/ghidra-integration.md) · [PLAN.md](PLAN.md)
- Sibling for the Mega Drive: [mdx](https://github.com/bombpersons/mdx).

## License

MIT or Apache-2.0, at your option.

# The catalogue

What the agent has set aside or does not know yet, for the whole goal:
three files, each optional until it has something in it.

## not_ported.toml

The code to port is every function the work recordings run (every
recording under `scenarios/` but the person's plays, `goal.toml`'s
`held_out`), less what this file lists, by entry or address range (the
end excluded), with why:

```toml
"0x80010088" = "memcpy: the port has its own"
"0x80050000..0x80058000" = "libgte's helpers: the port does its own maths"
```

Library code, the CD and sound drivers, the GPU's command building and
debug code go here; the port has its own of those, or none. Every other
function they run is cited by address in the port's code (`game/`,
`tools/`) where the port follows it: one cited nowhere is work, and the
goal is finished only once none is. When code listed here is on the path
to quantities that differ, the pointer says so.

## wont_reach.toml

The branches of the code to port no recording can reach, each with why:

```toml
"0x8002D8A4" = "the debug camera's car: no game mode sets it"
"0x8002D800..0x8002D860" = "the 4WD path: every car on the disc is 2WD"
```

A key is a conditional branch's address, a script position, a test as
`psx port next` gives it (it answers for every branch testing it), or a
range (the end excluded) answering for the branches in it no recording
ran; a recording that runs one makes it a problem. `psx port confirm`
says how many branches each range answers for. The person confirms them
with `psx port confirm --yes`; one added or changed later needs
confirming again.

## questions.md

What is not known yet, one `- ` line each:

```markdown
- does the handbrake change grip, or only steering?
```

Take a line out once it is answered (the answer goes where it is used: a
citation, a scenario, the glossary). The goal is not finished while one
is open.

## Drawing

What drawing shows is checked through quantities, not screens: the
camera, animation and the HUD have their quantities in the glossary and
their recordings. `psx scenario show --screens` puts both games' screens
side by side for you to look at.

## The goal

`goal.toml` holds the person's description, the assets to extract and
their plays held out. A scenario's toml names the assets its recordings
need (`assets = [...]`, as `formats/README.md` says): psx asks for them to
be imported and checked before those recordings' work. `psx port
confirm` shows the goal, the plays, the won't-reach branches and any
problems; the person confirms it with `psx port confirm --yes`. The
review asks whether the recordings show everything the description asks
for.

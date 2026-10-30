# Game compatibility (native core)

How far each game gets on `psx-core`, and the emulation problems seen in it.
Add a row or an issue whenever a game is tried, including problems found in
passing (a play's note, a screenshot that looks wrong). Fix details and
debugging trails belong in [research/native-core.md](../research/native-core.md);
link to them from here.

**Status:** *playable* means a person has played it in a `scenario play` window.
*Gameplay* means scripted input reached in-game play. *Boots* means it gets
past the logos but no further.

All discs are PAL unless noted, on the SCPH-7502 BIOS unless noted. Media and
`.psx/` evidence live in the main checkout and are not committed.

## Games

| Game | Serial | Status | Furthest point | Open issues |
|---|---|---|---|---|
| Crash Bandicoot | SCES-00344 | Playable | N. Sanity Beach, played by hand to the first crate | |
| Croc: Legend of the Gobbos | SLES-00593 | Gameplay | Level 1-1, walking to the gate | |
| Tomb Raider (DE) | | Gameplay | Lara in the Caves | [Native/Redux progress divergence](#tomb-raider-nativeredux-progress-divergence) |
| Castlevania: Symphony of the Night | SLES-00524 | Gameplay | Prologue, Richter on the castle stairs | |
| Mega Man 8 | SLES-01064 | Gameplay | Intro stage, weapon menu | |
| Spyro the Dragon | SCES-01438 | Gameplay | Artisans home world | |
| Metal Gear Solid (Disc 1, DE) | SLES-01507 | Gameplay | The Docks | |
| Final Fantasy VII (Disc 1, DE) | SCES-00869 | Gameplay | Midgar train platform | |
| Tekken 3 | SCES-01237 | Gameplay | Arcade fight | |
| Ridge Racer | SCES-00001 | Gameplay | Race, with CD-DA music | |
| Resident Evil (DE) | SLES-00228 | Gameplay | Mansion hall and dining room | |
| WipEout | SCES-00010 | Gameplay | Altima VII race | |
| Oddworld: Abe's Oddysee (DE) | SLES-00839 | Gameplay | Rupture Farms first screen | |
| R-Type Delta | SCES-01762 | Gameplay | Stage 1 | |
| Pandemonium! | SLES-00526 | Gameplay | Skull Fortress level | |
| PaRappa the Rapper | SCES-00743 | Gameplay | Stage 1 | |
| Gran Turismo | SCES-00984 | Gameplay | High Speed Ring race | |
| Gran Turismo 2 | | Gameplay | Race loads (needed the GetlocL/seek-phase fix, psx 0.3.2) | |
| Vagrant Story (DE) | SLES-02756 | Gameplay | Wine cellar with Ashley | Needs redump's `.sbi` (LibCrypt), as on hardware |

## Open issues

### Tomb Raider: native/Redux progress divergence

- **Reported** by an agent (TR B4 in [the feedback review](../research/tomb-raider-wipeout-feedback-review.md)): the same input progresses differently on the native core and in Redux. It has not been rerun since.
- **Suspected cause:** CD or FMV pacing.
- **Next step:** compare the two cores with event-aligned traces, keeping the BIOS, media and backend identical.

## Fixed

| Game | Problem | Fix |
|---|---|---|
| Resident Evil | Black screen after the Virgin logo | The CD response FIFO survives the interrupt acknowledge ([details](../research/native-core.md#fix-the-cd-response-fifo-survives-the-interrupt-acknowledge-resident-evil)) |
| Gran Turismo 2 | Races did not load | CD GetlocL and seek phase (psx 0.3.2) |
| Tomb Raider | Stuck before "Spiel Starten" | CD-DA playback and pregap positions ([details](../research/native-core.md#tomb-raider-pal-german-on-the-native-core)) |
| Croc | Hung on the FOX logo | MDEC |
| Crash Bandicoot | Level mesh tore into flat horizontal stripes on every other drawn frame (seen in a handoff, 2026-10-20) | Primitives crossing x=1023 after the drawing offset wrap as a whole ([details](../research/native-core.md#fix-primitives-wrap-as-a-whole-crash-bandicoot)) |

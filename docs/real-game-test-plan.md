# Real-game test plan (2026-11-05)

Everything since `d7d7b7d` (the master plan) and the November review
(`b6ff9d2` to `ef06b04`) was tested on hand-written programs and unit
tests only. This plan runs it on the discs in `media/` and the three
workspaces in `../external_agent_tests/` (croc, gran_turismo_2,
tomb_raider_chronicles). Done in order, one session, no parallel agents.

Results go where they already belong: a `*Done (date):*` line under the
master plan's step, a row in `docs/compatibility.md`, or a fix commit.

## 0. Baseline

- Rebuild release at `HEAD`; `cargo test --workspace`.
- Build the pre-review core once in a scratch worktree at `3da95b6`
  (`psx-before`), for the A/B runs in step 2.
- Check the tools the later steps need: `psx context` (BIOS, Ghidra),
  and `mipsel-linux-gnu-gcc`.

*Done (2026-11-05):* `cargo test --workspace --release`: 475 passed, 12
ignored. `ghidra::tests::queued_project_owners_wait_cancel_and_respect_deadlines`
failed once under the full run; its panic message was lost (the log kept
only summary lines). Not reproduced in 5 workspace runs, 35 runs of the
test binary and 80 of the test alone, with every core busy. Its waits
that only guard a hang (1-3 s) are now 30 s, the 75 ms one that must
expire is unchanged. **If it fails again**, keep the full `cargo test`
output (the `---- ... stdout ----` block) and look again. Ghidra 12.1.4 and MIPS GCC 14.2 found. The
repo's `.psx/project.json` named a deleted wipeout workspace's disc, which
made `psx context` fail; the entry was removed. `psx-before` built in the
scratchpad (thin LTO).

## 1. The core against Mednafen

The review changed emulation in M7 to M11, L10 and L11 (bus widths,
COP0, GTE aliases, X-flipped sprites, KUSEG mirror, GPU draw area and
timing). Ground truth is Mednafen.

- `psx-oracle suite`: the six lockstep runs (BIOS, Crash, Croc, FF7,
  Resident Evil, Tomb Raider) must still match with nothing adopted.
- Any divergence: `psx-oracle lockstep` on that run, find the change,
  fix or revert it.

*Done when* the suite passes at `HEAD`.

*Done (2026-11-05):* the suite passes at `ef06b04`, all six runs with no
divergence (5 min 11 s; Tomb Raider the longest, 8 G steps).

## 2. Every disc, before and after

The suite stops early in each game; the compatibility table reaches
much further. For each of the 19 games in `docs/compatibility.md`:

- Rebuild its states (`states rebuild`) and run the same input from the
  furthest state with `psx-before` and `HEAD`: screenshot, RAM and VRAM
  (`ram-diff`, `vram-diff`).
- Identical: fine. Different: explain it against Mednafen (`psx-oracle
  run` from the same point) — a review fix doing its job, or a
  regression.
- Also `media extract` one disc end to end (H1's name check must not
  refuse real names) and `assets scan` on two (H4's bounds must not
  refuse real TMDs or STRs).

*Done when* every game reaches its row's furthest point and each
difference is explained in the table.

*Done (2026-11-05):* nothing to explain. Each of the 19 discs in
`media/` ran from power-on with its survey replay (`.psx/native/`, the
frames in `research/native-core.md`; Ridge Racer needs 6300, its replay
ends at 6200) on `ef06b04` and on `3da95b6`: final RAM, scratchpad,
VRAM, screen, registers and cycle count are identical for every game,
and each screen is the table row's furthest point (Ape Escape, not in
the table, at its title menu). `states rebuild` in the three workspaces:
all 18 states (croc 6, GT2 8, TRC 4) the same as saved. `disc extract
--all` of Croc (41 files), FF7 (3381) and MGS (11) and `assets scan` of
Croc, Crash and Spyro are identical on both builds. So the review's
emulation changes (M7 to M11, L10, L11) change nothing these games do
in these stretches; step 1 shows they agree with Mednafen where the
suite reaches.

Noted in passing: `/tmp` here is a 16 GB tmpfs, mostly in use, and
filled during the extractions; runs writing at that moment died with
exit 101 and empty output, and passed when rerun. Keep large outputs
(whole-disc extractions) off `/tmp`.

## 3. The existing recordings still replay

The workspaces hold recordings made before any of this (croc: 5
scenarios, GT2: 5). They are a free regression corpus.

- `scenario check` on every scenario, `--stock`, in both workspaces:
  the original still reproduces its own recordings.
- The ports' own tests (`cargo test` in each workspace) after `port
  update` to the current template (A4's exclusive input, the two-run
  determinism check, M14's zero-tick and length checks).

*Done when* every stock check passes and both ports pass what they
passed before, or each new failure is a real one the new checks found.

*Done (2026-11-05):* `scenario check`: croc's 5 scenarios (42
recordings) and GT2's 8 (24) all deterministic, no differences. `port
update` to `ef06b04`'s template in both (croc keeps its own
`psx-bevy/src/gte.rs`; GT2's `frames.rs` conflict was an older template
copy brought in by hand in its `2b840db`, replaced by the new one). The
seeded changes applied by hand, keeping each game's own content: the
determinism rules (`AGENTS.md`), the tap line (`game/AGENTS.md`), and
`--replay` refusing to start without an import (`game/src/disc.rs`,
`replay: false` in `game/tests/disc.rs`). Ports' tests, before → after:
croc 97 → 100 passed, GT2 98 → 101 (the template's new tests), and the
one failing target in each is the same `scenarios` test with the same
failures: croc's `platforms_1_1 onto_the_rim` from tick 91 (41 of 42
recordings pass), and every GT2 scenario (93 failing quantities, and the
port's own panic at `game/src/car/wheels.rs:155`, an index past a
two-wheel array). Those are the ports' unfinished work, not psx. Neither
port's two replays of a recording part.

Found on the way: GT2 already has `measure_latch`/`measure_stock`
recordings in every scenario and `notes/pad-words.md` (2026-11-04), so
A5's measurement was started there; its menus read the pad from a
different place than the race's word `0x800A95B4`. Step 4 starts from
that.

## 4. Track A on real games

In the master plan's order; each step's *Done when* is already written.

1. **A1** `run --pad-reads` from a state in play on Croc, GT2 and TRC:
   buffer found within 2 fields, stable for 600, bytes match the reply.
   The library and joker cross-checks wait for the PsyQ signatures.
2. **A2** a do-nothing adjustment measures the same on Croc's scenarios
   (`scenario adjust-check`).
3. **A3** latch on Croc and GT2: every buffer read in a tick sees that
   tick's input; note the lag frames and handler-ending ticks reported.
4. **A6** steady: find a Croc or GT2 stretch that slows down stock
   (fields per tick in the recording), confirm it runs at its normal
   rate with `steady`, and compare audio and video timing.
5. **A5** adjust-check on every scenario of Croc and GT2, latched pads
   against GT2's `INPUT` answers; then remove `INPUT`.
6. **A7** decide from A6's numbers, and close or build it.
7. **A8** re-record both games adjusted, run both ports, record the
   verdicts before and after.

*Done (2026-11-05):* A1, A2, A3, A6, A5, A7 and A8 each have their
real-game results under them in the master plan. Found on the way: GT2's
pad library skips its poll while the serial port is busy (about 3 fields
in 10), so a fifth of its releases reach the game a tick late, latched or
not. The decision is that ports never copy such quirks: A9 (new) has psx
report late input and the agent patch the game. `INPUT` is retired, and
`adjust-check` gained `--adjust` to measure one adjustment alone. The
ports' new failures are their own: Croc's re-recorded `play_running`
reaches ground its port does not match, and GT2's port fails before input
matters.

## 5. Track C on Croc

- **C1** a Croc scenario with the enemies' update off (needs a `names/`
  entry for it): no enemy moves, adjust-check reports only enemy
  quantities, a misspelt name is refused with a suggestion.
- **C2** on a scratch Croc workspace: Restore appears once an enemy
  feature exists.

*C2 done (2026-11-05), on a scratch TRC copy:* Restore appears once
the attackers' feature matches, and after it the port fails where the
attackers move (the master plan's C2).

*C1 done (2026-11-05), on TRC instead:* Croc's objects are bytecode
scripts with no update function to turn off. On Tomb Raider Chronicles'
Old Mill attackers all three conditions hold (the master plan's C1).
`adjust-check` gained `--adjust off`, and reports `off_calls`.

Found on the way: TRC recordings took time growing with the square of
their length (200 fields 100 s, 2400 did not finish in 600 s; a plain run
of 200 takes 1.2 s). Coverage tracing walked each code page's chain of
changed copies back to its whole copy for every instruction run and for
every word reported, and TRC writes data beside its code each frame, so
the chains grow as the run does. The latest copy's bytes are now read
directly, and the report applies each page's copies in order. 200 fields
take 5.7 s, 2400 take 100 s, with coverage and recordings byte for byte
the same. **Still slow, to address after the master plan:** 2400 fields
should take about 70 s at 200's rate, and a plain run is about five
times faster than a recording.

## 6. Tools that touch real games

- **psx web** (H3, L3, the play queue): play Croc from the page, record,
  replay with and without the token, gamepad, the log's auto-scroll and
  the chart colours from `USER_NOTES_DELETE_AFTER_DONE.md`.
- **Messages** (H2): send from the page while the agent sends; nothing
  lost.
- **States** (M6): two concurrent `states rebuild` on one workspace.
- **Ghidra** (M4, L8, and `docs/ghidra-settings-dir-fix.md`): analyse
  Croc, reanalyse, check the old project is gone and the settings
  folder is the same inside and outside the home folder.
- **code check** (D10, D11): one Croc function in C against the
  original, from a state. Needs the MIPS GCC.
- `code inventory` without `--unpacked` on a packed game (the user note).

*psx web done (2026-11-05), on a scratch Croc copy, except the gamepad
(none to hand):* the user played from the page and the recording was
written with their description, the request answered.
- **Token:** starting a replay or the port's replay is refused without
  the page's token, with a wrong one, and from another origin. Reading
  works.
- **Replays:** the page's replay matched the recording tick for tick;
  its first ticks are black because Croc fades in. The port's replay
  built the game with cargo (about 3 minutes from nothing) and drew its
  stand-in Croc.
- **Oddities:** the Play form shows an `off` box, ticked, though Croc
  has no `[off]`. The name it proposes is not the one written (a later
  minute). The recording panel is wider than the space left beside
  Messages.
- **Auto-scroll:** not reproduced. Over six long runs and many short
  calls, with scrolling up and back down mid-run, it stayed at the
  newest call and picked up again once scrolled to the bottom (the
  pane was hidden, so the page drew a few frames a second and caught
  up within one). Still the user's to confirm.
- **Colours:**
  - "Features over time" colours every mark by the feature's stage
    now. On GT2 and Croc every feature is `located`, so every row is
    the same blue.
  - Activity draws calls and lines added in that same blue, and
    failures in a red near `recorded`'s pink.
  - The closest palette pairs are `located`/`reviewed` (ΔE 21 in dark
    and PlayStation, 36 in light) and `recorded`/failed (25, 41).
  - **Changed:**
    - Each feature in "Features over time" now has its own hue, a golden
      angle on from the row above, and its stage is written at the end
      of the row.
    - Activity uses grey and red for calls, and green and red for lines.
    - "Recorded" and "reviewed" have new colours, so no stage pair is
      under CIEDE2000 22 in light and dark (it was 16–18). The closest
      is now in PlayStation: catalogued against located, 21.8, unchanged.
    - Checked on GT2 in all three themes.

*Messages done (2026-11-05):* the page sent 40 messages at once while
40 agent processes each sent one and then made a call. All 80 were
kept, each of the person's reached the agent exactly once, and both
seen counts ended at 80. But the page's messages sent within one
millisecond shared an id (60 distinct of 80), so a reply could join
the wrong thread. Ids now carry the message's place in the log as well:
80 of 80 distinct.

*States done (2026-11-05), on the scratch Croc copy:* `states rebuild
level_1_1` beside `states rebuild level_1_1_idle` both reported `same`
in 22 s. The first ran the five steps, and the second reused the four
it shares and ran its own last one. Two `states rebuild` of every chain
at once, from an empty `.psx/rebuild`, both reported all six states
`same` in 23 s, about one rebuild's time. The first ran each step and
the second waited for each step's lock and reused it, and all six step
folders were left whole.

*Ghidra done (2026-11-05), on a scratch Croc copy:* Ghidra refuses any
project path with a folder starting with '.', and there is no switch to
allow one, so the usual `.scratch` copy failed with only "Ghidra exited
1". psx now stops before starting Ghidra and names the folder. In
`external_agent_tests/scratch-ghidra`, the first analysis found 915
functions and 1153 GTE instructions, and decompiled 8005A6CC in full.
Ghidra's settings went to `config/ghidra`. `--reanalyze` took 21 s and
gave the same counts, and only its new project was left in
`ghidra-projects/`. In a copy outside the home folder (the scratchpad),
the preferences, tools and PsxGte extension went to
`config/maisy-ghidra`, as Ghidra expects there. Only the launcher's
`java_home.save` is in `config/ghidra`, because the launcher script
always writes that name. The cache key includes the language files'
hash (M4).

## 7. What the discs now make possible

- **L2**: LWL/LWR/SWL/SWR as narrow accesses with a 24-bit width, then
  step 1 and 2 again to confirm timing still matches.

## Not in this plan

B1 and track D's unbuilt steps (D5 to D8, D12 to D15) are new work, not
testing.

Save a useful native checkpoint, then compare short runs from that same state
with different inputs. Keep BIOS and media identities fixed. context
with from_state explains the checkpoint's required identity.

run's input field is an inline JSON array of complete held controller states:
[{"frame":0,"buttons":["CROSS"],"until":5},{"frame":10,"buttons":[]}]
replay is a file path containing the same array. Choose one input mechanism.
Frame numbers are relative to this run's start, including after from_state.
Log frame values are absolute; log run_frame values are relative.

Each event replaces the whole held state. Without until it stays held until the
next event; buttons:[] releases all. until is exclusive, so [0,5) holds for five
frames. Events must be strictly ordered within 0..=frames and cannot overlap an
earlier until interval. Before the first event, a checkpoint retains its saved
pad state. Use explicit neutral input when that matters.

Button names are case insensitive: SELECT, L3, R3, START, UP, RIGHT, DOWN, LEFT,
L2, R2, L1, R1, TRIANGLE, CIRCLE, CROSS, SQUARE. Optional sticks are
[left_x,left_y,right_x,right_y], 0..255 with 128 centered. Noncentral sticks need
a native DualShock configured at boot; pad settings are restored from checkpoints.

Inspect report.json's input schedule, final pad state and input.application counts.
Native input.jsonl records the actual relative/absolute application frame for
each event, including releases; timed-out runs report pending events. This is not
proof that the game polled every event. Compare RAM, screenshots or sampled variables;
identical outcomes can mean the game was not accepting that input at that time.
Use ram_diff / ram_search to find candidates, sample for trajectories, watch for
writers and poke to test a proposed rule. Keep an unmodified control run.

## Ask a person: handoff

When scripted input cannot reach a place in reasonable time (platforming,
combat, long routes), ask the person at the machine. `handoff` opens a window
on the computer running psx with the game (from media or from_state) on the
left and your request, the keys and messages in a panel beside it. Nothing is
drawn over the game. Over MCP it returns a ticket at once; poll
`handoff_status` with wait (up to 600 s per call) until status is no longer
waiting. People take minutes.

Write the request as a precise target: the level and spot, which way to face,
health or items, and whether a cutscene should already have played.

The person plays with keyboard or gamepad (F1 or Start+Select rebinds, saved
per user), can quick save/load (F5/F7) and start over from the handoff start
(F8 twice), and hands back with F12 (done) or F10 (cannot do it), both with an
optional note. Results:

- status done | declined | closed (window closed) | abandoned (process died);
- state.bin: continue with run from_state (report.json supplies media and BIOS);
- input.json: every pad change since the handoff start as a replay, with input
  lost to quick loads and restarts removed. `reproduce` gives the run call that
  replays it; verify=true checks it reproduces state.bin;
- screen.png and the person's note.

Start from a checkpoint near the target rather than power-on when you have one.
For analog games boot with pad dualshock: a digital pad maps the left stick to
the d-pad. The window needs a desktop session (DISPLAY or WAYLAND_DISPLAY) on
the psx machine; a server started over plain SSH cannot open one.

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

## Ask a person: scenario play

When scripted input cannot reach a place in reasonable time (platforming,
combat, long routes), ask the person at the machine. `scenario_play` opens a window
on the computer running psx with the game (from media or from_state) on the
left and your request, the keys and messages in a panel beside it. Nothing is
drawn over the game. Over MCP it returns a ticket at once; poll
`scenario_play` with status (the ticket's run_dir) and wait (up to 600 s per call) until status is no longer
waiting. People take minutes.

With a scenario id and name, the play becomes that scenario's recording:
the window starts at the scenario's state (from: at the end of one of its
recordings, to split a long play-through) once its first tick has run with
the pad released, as scenario_record starts. When the person hands back
with F12, psx records their input as scenario_record does, with its
coverage, and the result's recording says whether it ended where the play
did (matches_play). input.json then counts frames as scenario_record's
input does, and reproduce is the scenario_record call that makes the
recording again. A play the person declined or closed is not recorded.

Write the request as a precise target: the level and spot, which way to face,
health or items, and whether a cutscene should already have played. With a
known address, say it in expect too (run's until syntax: 8007528C:u8=3):
conditions are checked before the window opens, and the result's expect gives
each condition's value at the hand-back, whether it held, a verdict and
ask_again. Ask again, in game terms, when the person said done but expect
did not hold. To find
where the game keeps something, ask for marks too: "press F6 just before and
just after you pick up the key".

The person plays with keyboard or gamepad (F1 or Start+Select rebinds, saved
per user), can rewind about 5 s at a time (F2, up to 2 minutes), quick
save/load (F5/F7), start over from the play's start
(F8 twice), mark moments for you with an optional label (F6), mute the sound (F4), and
hands back with F12 (done) or F10 (cannot do it), both with an
optional note. Results:

- status done | declined | closed (window closed) | abandoned (process died);
- state.bin: continue with run from_state (report.json supplies media and BIOS);
- input.json: every pad change since the play's start as a replay, with input
  undone by rewinds, quick loads and restarts removed. `reproduce` gives the run call that
  replays it (without a scenario); verify=true checks it reproduces state.bin;
- ram.bin, screen.png and the person's note;
- marks: each F6 press as marks/<n>/ (state.bin, ram.bin, screen.png,
  mark.json with the label and frames). The report gives each mark's RAM diff
  against the mark before it on its route (or the start: marks/start/ram.bin),
  a run call to continue from it, and on_route (false when a quick load or
  start over took play elsewhere afterwards). route_snapshots lists start, the
  marks on the route and the end in order, ready for ram_search. The status
  lists the marks made so far while the person plays.

Start from a checkpoint near the target rather than power-on when you have one.
For analog games boot with pad dualshock: a digital pad maps the left stick to
the d-pad. The window needs a desktop session (DISPLAY or WAYLAND_DISPLAY) on
the psx machine; a server started over plain SSH cannot open one.

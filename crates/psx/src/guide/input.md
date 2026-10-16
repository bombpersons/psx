Save a useful native checkpoint, then compare short runs from that same state
with different inputs. Keep BIOS, media and backend identities fixed. context
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

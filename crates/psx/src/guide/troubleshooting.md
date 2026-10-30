First record context's build ID and effective project root. An older connected
MCP server may differ from the current source or executable on disk. Missing
relative paths are interpreted against the server root, not the client shell.
Use project_root per call or absolute paths; check saved project defaults before
assuming another game's media or BIOS was selected incorrectly.

For large results, use filter/offset/limit and page continuation. read
reads returned files: JSON pointer selects a section; JSONL/CSV support frame/PC
filters and columns. Continue next_cursor with the same query and omit offset.
An empty page can still have a cursor when its scan budget was reached.

status:complete and termination:frames_reached mean the run met its frame budget,
not that boot completed or gameplay began. Read warnings: no_gpu_draws means
nothing was drawn for the last frames_without_draws frames. With cd_retry_loop
(LBA, owning file, Setloc count and period) the game is stuck re-reading the CD,
a load that never finishes: report it as an emulator bug with that LBA rather than
waiting longer. With screen_blank and no loop, look at final_pc and hot PCs. A
still title or loading screen also draws nothing and is not a fault. For GPU captures, inspect complete,
draw_commands and search reason. gpu_next_draw skips empty candidates but accepts
fills; use gpu_draw_filter to require primitives or textured draws. A successful
exact capture may contain no draws. Use snapshot_every to
find relevant events; increasing the search window alone cannot navigate menus.

Raw screen dimensions can change during boot. Inspect video mode/interlace/range
metadata before comparing images. Do not force every mode to 320x240 or treat a
resolution change alone as an emulator defect.

If input appears ineffective, check input.events, pad state, frame origins and
whether the game is polling that button. A checkpoint preserves controller/card
configuration. Unknown input keys and out-of-budget events should fail validation.

PNG responses require a vision-capable client/model. Without vision, leave
return_screen false and inspect JSON/logs; read on a PNG automatically
attaches the image too. Hashes, dimensions, RAM/samples and command counts provide
limited evidence, not visual fidelity. Report that limitation instead of claiming
to have inspected the picture. Preserve raw artifacts and precise errors for a retest.

Menus that ignore short presses. Many games read the pad once a tick, act on
a press only after seeing the button released, or poll menus at their own
slower rate, so a tap of one or two fields can fall between reads and do
nothing. In menus, hold each button for several ticks (4 to 6 fields) and
release it as long before the next press; check the screen after each step
rather than scripting a whole menu blind. A press that works when held and
not when tapped is the game's input timing, not lost input (input.jsonl shows
it was applied).

An interrupted analysis. A Ghidra query that times out keeps its partial
project and is never cached as complete: retry with a larger timeout. One
whose process was killed (a closed client, a restarted MCP server) can leave
psx.lock in the analysis folder under .psx/ghidra/, and every later query
reports analysis_busy. psx never removes another process's lock: read the pid
in psx.lock, check that process is gone, then delete the file. project.lock
means the project is open in the Ghidra GUI; close it there. A query after a
Ghidra update reports stale_analysis and analyses again from the source.

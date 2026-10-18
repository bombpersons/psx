First record context's build ID and effective project root. An older connected
MCP server may differ from the current source or executable on disk. Missing
relative paths are interpreted against the server root, not the client shell.
Use project_root per call or absolute paths; check saved project defaults before
assuming another game's media or BIOS was selected incorrectly.

For large results, use filter/offset/limit and page continuation. artifact_read
reads returned files: JSON pointer selects a section; JSONL/CSV support frame/PC
filters and columns. Continue next_cursor with the same query and omit offset.
An empty page can still have a cursor when its scan budget was reached.

status:complete and termination:frames_reached mean the run met its frame budget,
not that boot completed or gameplay began. For GPU captures, inspect complete,
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
return_screen false and inspect JSON/logs; artifact_read on a PNG automatically
attaches the image too. Hashes, dimensions, RAM/samples and command counts provide
limited evidence, not visual fidelity. Report that limitation instead of claiming
to have inspected the picture. Preserve raw artifacts and precise errors for a retest.

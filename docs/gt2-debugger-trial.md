# GT2 trial: the debugger

Step 8 of docs/run-plan.md. The brief below goes to a fresh agent session
in `external_agent_tests/gran_turismo_2` (a new session, so it loads the
current psx tool list from `.mcp.json`).

State of the workspace when this was written (2026-11-07):

- Chains rebuilt and committed (57b92e7); all 8 scenarios check
  deterministic.
- `psx port check` at 0180540: F-0002, F-0003 and F-0004 pass; the rest
  fail. b1_course/offroad (F-0010, also F-0007 and F-0008) matches for
  228 ticks and first differs at tick 229 (car_position_x, car_position_z,
  car_speed; car_yaw at 243).
- `port_next` gives a Review item, which this session sets aside.

## Brief

> This session is a trial of psx's debugger (guide topic debug), set
> apart from the usual loop: leave port_next's Review for another
> session.
>
> psx port check says b1_course/offroad (F-0010) first differs from the
> original at tick 229: car_position_x, car_position_z and car_speed,
> then car_yaw at 243. Find why the port differs there, using the
> debugger wherever it helps, fix the port, and see port check pass
> for b1_course. Commit as you would any fix (name F-0010).
>
> Then write psx-feedback-debugger.md at the workspace root for the psx
> developers: what you used the debugger for and in what order, what
> it answered quickly, what you wanted from it and it did not have (or
> had and you could not find), where you went back to run or other
> tools and why, and anything in its results that misled you. Concrete
> calls and results over impressions. If you could not find the
> cause, say so, how far you got, and what stopped you.

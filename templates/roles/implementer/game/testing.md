## Testing gameplay

`engine::testing` runs rules headless for exact ticks with scripted
buttons (`tests/player.rs`). The behaviour to match comes from the case
files in `specs/cases/` (`specs/README.md` gives their format): each case
becomes a test that sets up its start, replays its input tick by tick
(each tick gets the pad of the field it ran in, from the case's `timing`)
and compares every row's columns in `compare`, within their tolerances,
repeats included. One runner serves every case: write it once, check it
against each case's `timing` before any behaviour, and add a case with
one line. Use tolerance 0 where the spec says the rule is exact.


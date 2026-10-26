## Testing gameplay

`engine::testing` runs rules headless for exact ticks with scripted
buttons (`tests/player.rs`). The behaviour to match comes from the case
files in `specs/cases/` (`specs/README.md` gives their format), and
`engine::testing::Case` replays them (`tests/cases.rs` shows the form on
the demo's own case). Point its `load` at `specs/cases/`, write one runner
per subject, which sets up the case's start and reads each compared column
back in the case's units, and add each case with one line.

The runner runs as many ticks before each row as the case's `tick` column
says, gives each tick the pad of the field it ran in, and checks that
against the case's `timing` before any behaviour. It compares, within each
column's tolerance and repeats included, only the columns and rows that
the specs you list in `implementing` claim (the case's `coverage`), and
lists the rest as waiting for their specs. A case whose start is a state
(`specs/states/`) is set up as that state's description says. Use
tolerance 0 where the spec says the rule is exact.


A case is one recording that backs a spec: the subject's fields every
field from a start state with pad input, compared row by row by whoever
implements the spec. case record makes one from a field map (guide topic
fields) and writes three files:

- specs/cases/NAME.csv, the observations: run_frame, frame, tick, then the
  subject's fields by their field names and the other columns by their map
  names. No addresses: the map's names only.
- specs/cases/NAME.json: id, spec, role (heldout, calibration, diagnostic
  or exploratory), level, subject (its name, or name, index and type for an
  array element, type read from the field kind names at row 0), start
  (from first_row), input, activation (columns that are input, not
  compared), observations and compare (column: tolerance; default every
  other column, exactly), with fields_per_tick and first_tick from the tick,
  the timing vectors and, when given, coverage (below).
- notes/recordings/NAME.json, the recipe: the arguments, the start state's
  and map's hashes, the media and BIOS identity and the psx version. It
  names no addresses either, but it is the investigator's: keep it on that
  side.

  case record NAME --from-state S --frames 120 --subject lara \
      --sample camera_x --input '[{"frame":0,"buttons":["LEFT"],"until":40}]' \
      --spec SPEC-0007 --role calibration --level assault

Timing (rows and ticks as guide topic fields describes): row 0 is the
subject before any tick; tick counts the ticks completed since row 0, and
the ticks between rows r-1 and r ran during field r-1 and read its pad.
first_tick is the first row whose tick rose and fields_per_tick the fields
per tick over the recording, rounded: both describe steady play, and the
tick column decides. The result's timing gives fields_by_ticks_run and
changed_without_tick: rows there (a column moving on a row whose tick did
not) belong in the spec's notes, or mean [tick] needs moving.

The case's timing holds the vectors a runner checks itself against before
any behaviour: ticks_run, per row, the ticks completed since the row before
(0, 1, or 2 when the game catches up), and reads_field, the field those
ticks ran in and read the pad of (r-1 for row r; null with no tick). Tick
0, 2, 2, 2, 3 gives ticks_run 0, 2, 0, 0, 1 and reads_field null, 0, null,
null, 3.

Coverage says which spec claims which cells, so a runner compares a case
only over what each spec it implements claims, and knows which spec the
rest waits for. coverage=SPEC:COLUMNS[:ROWS], repeatable, one per spec:
COLUMNS are compared columns (a,b, or * for all of them); ROWS are A..B
(inclusive), A.. (to the end) or A..until COND, through the first row from
A where COND holds (until's syntax on the case's columns, & between
conditions: "1..until state=2", the hop through the row she lands). No ROWS
is every row. The case keeps each spec's columns and rows, and the until it
came from; a compared column no spec claims is a warning.

  case record hop --coverage 'SPEC-0006:state,goal:1..until state=2' \
      --coverage 'SPEC-0003:yaw' ...

case check [NAME...] records cases again from their recipes (all of them
by default) and compares each with its published CSV cell by cell, by
column name, and the case's fields_per_tick, first_tick, input, subject,
timing and coverage (an until's rows resolved again).
same is exact; otherwise the first differing cells are listed. Run it after
a psx update or a map change; write=true replaces the cases that differ.
A key this psx writes that a case lacks (timing, in a case from an older
psx) is listed in case_keys_to_add, not a difference; write=true adds it
at the end of the case's JSON and leaves the rest of its files as they
were. Writing a case that differs keeps keys added to it by hand.
A case record over an existing case needs replace=true.

Start states: take them where the behaviour starts (a handoff's mark, a
run's save_state, or run until a named state holds) and name them with
states make or states save (guide topic states), so each rebuilds from
power-on; the case's row 0 is that state.

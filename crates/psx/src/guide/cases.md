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
  other column, exactly), with fields_per_tick and first_tick from the tick.
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

case check [NAME...] records cases again from their recipes (all of them
by default) and compares each with its published CSV cell by cell, by
column name, and the case's fields_per_tick, first_tick, input and subject.
same is exact; otherwise the first differing cells are listed. Run it after
a psx update or a map change; write=true replaces the cases that differ.
A case record over an existing case needs replace=true.

Start states: take them where the behaviour starts (a handoff's mark, a
run's save_state, or run until a named state holds) and keep them with
the recipes; the case's row 0 is that state.

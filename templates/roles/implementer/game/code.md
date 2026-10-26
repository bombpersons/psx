## From specs to idiomatic code

The specs give behaviour in game terms: values as observed, rules as a
player would describe them, test vectors for small algorithms. The job is
to turn them into *rules* a reader recognises, in ordinary Bevy.

### Name the rule, then convert the units

Write down what the spec says a piece of behaviour does in one sentence,
then implement that sentence. Convert numbers at the boundary, once, with
named constants and small unit helpers, and cite the spec each came from.
Choose the units that fit:

- **Natural units with `Time`** (metres or pixels per second, scaled by
  `time.delta_secs()`) for behaviour whose exact per-tick shape no case
  pins.
- **Per-tick units** when the cases pin behaviour tick by tick: jump
  heights, landing ticks, frame windows. A fixed-tick simulation with
  per-tick constants is legitimate; say so in the module doc.
- Keep the game's coordinate conventions only where they help matching,
  mapped to `Transform` in one sync system. Otherwise use Bevy's.

### When to keep exact integer behaviour

Keep exact integer behaviour where it changes what the player sees and the
cases pin it: how many ticks something lasts, thresholds hit on exact
values, rounding the spec describes. Wrap it in a small documented helper
with a test, called by its meaning, not inline arithmetic in a system.

### Cite the specs in comments

Every constant and every non-obvious rule gets a comment naming the spec
and the case it comes from, or the data file it is read from. Say what no
case checks yet.

### State machines

Model the states, not a dispatch:

- One `enum` component per independent state, with the data each state
  needs inside its variant. Timers live in the state that uses them. A
  number from the data becomes a variant at the boundary, with one
  variant carrying the number for any state no rule covers yet.
- A few systems, chained in the tick's order the specs give, one concern
  each. Document why the order matters.
- `match` on the enum inside each system; transitions are assignments of
  a new variant. No `state: u8`, no `fn table[state]()`, no giant
  per-tick function.
- Separate concerns into separate components and systems (facing,
  vertical motion, animation).


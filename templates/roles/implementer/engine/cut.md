## Cut it where the game's data cuts

Most PS1 games already separate:

- **Data the engine interprets**: level geometry, collision, animation tables,
  object placement, trigger and script tables, setup values. The `FMT` specs
  describe these files; the importers turn them into the engine's formats.
- **Behaviour per object type**: the player's states, each enemy, special
  traps and bosses, as the specs describe them.
- **Behaviour shared between types**: where the specs give one rule for a
  whole family (every creature's movement, every gun), it becomes one module
  with per-type parameters, in `game/` when it is this game's rules, here
  when another game would need it.

Keep what the data expresses as data and per-type behaviour as code, and
generalising costs no accuracy. Accuracy breaks when the specified rules are
replaced by abstractions they don't have (a generic "enemy behaviour" schema,
a physics component in place of the specified movement): they lose update
order, timing within a tick, rounding and special cases, and the cases fail.
Configurable layers can come after the cases pass, and must keep them
passing. `docs/engine-model.md` records what the specs and data files show
is data, shared behaviour and per-type behaviour; this crate follows it.


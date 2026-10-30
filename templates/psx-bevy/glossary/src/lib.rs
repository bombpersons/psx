//! The game: the quantities scenarios compare.
//!
//! Each field of `Quantities` is one quantity, named in game terms, in the
//! original's raw units: a bool, an integer, a fixed-point alias
//! (`pub type Fx20_12 = i32;`) or an enum. Its doc comment says what it is,
//! with a line `Unit: …` for its unit. Scenarios compare them by name
//! (`scenarios/<id>.toml`, `[compare]`).

pub struct Quantities {}

//! The game: the quantities scenarios compare.
//!
//! Each field of `Quantities` is one quantity, named in game terms, in the
//! original's raw units: a bool, an integer, a fixed-point alias
//! (`pub type Fx20_12 = i32;`) or an enum. Its doc comment says what it is,
//! with a line `Unit: …` for its unit. Scenarios compare them by name
//! (`scenarios/<id>.toml`, `[compare]`).
//!
//! A quantity is something a player could notice by watching or listening:
//! where things are and which way they face, what's drawn (the camera, the
//! animation playing and its frame, the model or sprite shown) and what the
//! game shows or plays (the HUD's numbers, a sound starting). The original's
//! counters, timers, flags and script variables are not quantities: the port
//! may reach the same results its own way. Screens are not compared.

pub struct Quantities {}

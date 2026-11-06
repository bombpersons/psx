//! The game: the quantities scenarios compare.
//!
//! A sled on a hill: right pushes it down the hill, left back up, cross
//! brakes, and the snow slows it every tick.

/// A distance or speed with 12 fractional bits: 4096 is one unit.
pub type Fx20_12 = i32;

pub struct Quantities {
    /// How far down the hill the sled is
    /// Unit: 1/4096 unit
    pub sled_x: Fx20_12,
    /// How fast the sled moves down the hill; negative is up it
    /// Unit: 1/4096 unit per tick
    pub sled_speed: Fx20_12,
}

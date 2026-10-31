//! The game: the quantities scenarios compare.
//!
//! Each field of `Quantities` is one quantity, named in game terms, in the
//! original's raw units: a bool, an integer, a fixed-point alias
//! (`pub type Fx20_12 = i32;`) or an enum. Its doc comment says what it is,
//! with a line `Unit: …` for its unit. Scenarios compare them by name
//! (`scenarios/<id>.toml`, `[compare]`).
//!
//! World units: the original's integer position units. Croc stands about
//! 225 below the level origin at the start of 1-1; y grows upwards.

/// A world-unit speed with 12 fractional bits: 4096 is one world unit per
/// tick.
pub type Fx20_12 = i32;

/// A turn rate with 16 fractional bits: 65536 is 1/4096 turn per tick.
pub type Fx16_16 = i32;

/// How Croc is moving on his feet, as his movement decides it each tick.
pub enum Gait {
    /// No forward or backward input
    Standing,
    /// The first ticks of forward input
    Walking,
    /// Forward input held past the walk
    Running,
    /// Backward input
    Backing,
    /// Sliding (a steep slope or a fast platform)
    Sliding,
    /// Sliding on a surface of type 8
    SlidingSteep,
}

pub struct Quantities {
    /// Croc's position across the level (east-west)
    /// Unit: world units
    pub croc_x: i32,
    /// Croc's height; grows upwards
    /// Unit: world units
    pub croc_y: i32,
    /// Croc's position along the level (north-south)
    /// Unit: world units
    pub croc_z: i32,
    /// The way Croc faces: 4096 is a whole turn
    /// Unit: 1/4096 turn
    pub croc_angle: u16,
    /// Croc stands on the ground; false through a jump or a fall
    /// Unit: flag
    pub croc_on_ground: bool,
    /// Croc's horizontal speed along x, as his movement smooths it
    /// Unit: 1/4096 world unit per tick
    pub croc_speed_x: Fx20_12,
    /// Croc's horizontal speed along z, as his movement smooths it
    /// Unit: 1/4096 world unit per tick
    pub croc_speed_z: Fx20_12,
    /// How fast Croc's facing turns, as it builds up while a turn is held
    /// Unit: 1/65536 of 1/4096 turn per tick
    pub croc_turn_rate: Fx16_16,
    /// Croc's vertical speed: upward while rising, downward while falling
    /// Unit: world units per tick
    pub croc_vertical_speed: i32,
    /// How much higher a jump may still take Croc
    /// Unit: world units
    pub croc_jump_height_left: i32,
    /// How Croc is moving on his feet
    /// Unit: state
    pub croc_gait: Gait,
    /// What Croc is doing: 1 moving freely (on the ground or in a jump),
    /// 4 stomping, others not ported (attack, hurt, ...)
    /// Unit: state number
    pub croc_action: i32,
}

//! The port, as a library: [`Game`] is the whole game, and psx drives it
//! through [`Port`]. `tests/scenarios.rs` replays the original's
//! recordings against it; `src/main.rs` draws them for psx web
//! (`--replay`) and runs the game itself.
//!
//! The rules live in modules of their own, one per catalogue feature, named
//! after it; `Game` holds their state and runs them once a tick.

/// `Observed`: the glossary's quantities as the port has them, which
/// [`Game::observe`](Port::observe) fills for the scenarios. Generated from
/// `glossary/src/lib.rs` by `build.rs`.
pub mod observed {
    include!(concat!(env!("OUT_DIR"), "/observed.rs"));
}

use observed::Observed;
use psx_scenario::{Pad, Parameters, Port};

/// The original game's video region: its display rate, and with
/// [`FIELDS_PER_TICK`] the logic tick rate.
pub const REGION: Region = Region::Ntsc;

/// Video fields per logic tick: 1 when the original's main loop runs every
/// vblank, 2 when it waits for two (PAL: 25 Hz logic on a 50 Hz display).
pub const FIELDS_PER_TICK: u32 = 1;

/// A video region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// 60 Hz.
    Ntsc,
    /// 50 Hz.
    Pal,
}

impl Region {
    /// Fields (vblanks) a second.
    pub fn hz(self) -> f64 {
        match self {
            Region::Ntsc => 60.0,
            Region::Pal => 50.0,
        }
    }
}

/// Logic ticks a second.
pub fn tick_hz() -> f64 {
    REGION.hz() / f64::from(FIELDS_PER_TICK)
}

/// The whole game.
#[derive(Default)]
pub struct Game {}

impl Game {
    /// A game with every feature's rules and nothing on screen, as the
    /// scenarios and `--replay` use it.
    pub fn headless() -> Game {
        Game::default()
    }
}

impl Port for Game {
    type Observed = Observed;

    fn kinds(&self) -> Vec<String> {
        Vec::new()
    }

    fn launch(&mut self, kind: &str, _p: &Parameters) -> Result<(), String> {
        Err(format!("no launcher for scenario kind {kind}"))
    }

    fn tick(&mut self, _pad: &Pad) {}

    fn observe(&mut self) -> Result<Observed, String> {
        Ok(Observed::none())
    }
}

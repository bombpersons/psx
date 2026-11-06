//! Croc: Legend of the Gobbos, ported, as a library: [`Game`] is the whole
//! game, and psx drives it through [`Port`]: `src/main.rs` serves it to psx
//! (`--psx`), which runs the original's recordings against it and draws
//! them for psx web, and runs it in a window. `tests/scenarios.rs` runs the
//! recordings here, a quick check while working.
//!
//! Layout: the world and its levels ([`world`]), characters' bodies
//! ([`character`]) and Croc, the player, with his abilities ([`croc`]).
//! [`Game::tick`] runs them in the original's order.
pub mod character;
pub mod config;
pub mod croc;
pub mod world;

/// `Observed`: the glossary's quantities as the port has them, which
/// [`Game::observe`](Port::observe) fills for the scenarios. Generated from
/// `glossary/src/lib.rs` by `build.rs`.
pub mod observed {
    include!(concat!(env!("OUT_DIR"), "/observed.rs"));
}

use observed::Observed;
use psx_scenario::{Button, Pad, Parameters, Port};

use character::BodyTuning;
use character::vertical::VerticalTuning;
use config::load_tuning;
use croc::jump::JumpTuning;
use croc::running::RunningTuning;
use croc::stomp::StompTuning;
use world::{Level, World};

/// Window title.
pub const TITLE: &str = "Croc: Legend of the Gobbos";

/// The original's video region: the PAL release (SLES-00593).
pub const REGION: Region = Region::Pal;

/// Video fields per logic tick: the main loop adds 128 per field to the
/// accumulator at 0x80075240 and runs a logic step per 256 (0x80013CA8),
/// so 25 steps a second on PAL's 50 fields.
pub const FIELDS_PER_TICK: u32 = 2;

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

/// Every design number, from `assets/` (see [`config`]).
#[derive(Clone, Debug)]
pub struct Tunings {
    pub body: BodyTuning,
    pub vertical: VerticalTuning,
    pub running: RunningTuning,
    pub jump: JumpTuning,
    pub stomp: StompTuning,
}

impl Tunings {
    /// The game's own. Panics with the file and the error if one is
    /// invalid: a game without its tuning is not worth running.
    pub fn load() -> Tunings {
        Tunings {
            body: load_tuning(BodyTuning::PATH),
            vertical: load_tuning(VerticalTuning::PATH),
            running: load_tuning(RunningTuning::PATH),
            jump: load_tuning(JumpTuning::PATH),
            stomp: load_tuning(StompTuning::PATH),
        }
    }
}

/// The pad as the rules read it: what is held this tick, and what was
/// pressed (held now, not last tick).
#[derive(Clone, Debug, Default)]
pub struct Controls {
    held: Vec<Button>,
    previous: Vec<Button>,
}

impl Controls {
    /// This tick's pad.
    fn update(&mut self, pad: &Pad) {
        self.previous = std::mem::replace(&mut self.held, pad.buttons.clone());
    }

    pub fn held(&self, b: Button) -> bool {
        self.held.contains(&b)
    }

    pub fn pressed(&self, b: Button) -> bool {
        self.held(b) && !self.previous.contains(&b)
    }
}

/// The whole game.
pub struct Game {
    pub tunings: Tunings,
    pub world: World,
    pub controls: Controls,
}

impl Game {
    /// A game with nothing in its world yet, as the scenarios launch it.
    pub fn headless() -> Game {
        Game { tunings: Tunings::load(), world: World::default(), controls: Controls::default() }
    }

    /// The game at its start level ([`Level::START`]).
    pub fn start() -> Result<Game, String> {
        let mut game = Game::headless();
        game.world = Level::start()?.build()?;
        Ok(game)
    }
}

impl Port for Game {
    type Observed = Observed;

    fn kinds(&self) -> Vec<String> {
        vec![croc::FLAT_FLOOR.into()]
    }

    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
        match kind {
            croc::FLAT_FLOOR => {
                self.world = croc::flat_floor(p)?.build()?;
                Ok(())
            }
            _ => Err(format!("no launcher for scenario kind {kind}")),
        }
    }

    /// One logic tick, in the original's object loop's order (0x8002C790):
    /// the animation and the vertical motion before an object's script,
    /// collision after it.
    fn tick(&mut self, pad: &Pad) {
        self.controls.update(pad);
        let (t, pad) = (&self.tunings, &self.controls);
        let World { floors, croc } = &mut self.world;
        let Some(croc) = croc else { return };
        // Controllers turn the pad into what the body reads this tick.
        croc::jump::hold_rise(pad, croc);
        // Animations advance a frame; rising and falling.
        croc.body.clip.advance();
        character::vertical::move_vertically(&t.vertical, &mut croc.body);
        // Reactions to what the physics did (landing); feet follow the floor.
        croc::jump::land(&t.jump, croc);
        character::follow_floor(&t.body, &mut croc.body);
        // The action chosen last tick takes over.
        croc.action.latch();
        // The free action's control: MOVE first, TURN last, the jump and
        // stomp tests between them (script 0x2F9D, sub 0xA9C).
        croc::running::walk(pad, &t.running, croc);
        croc::jump::take_off(pad, &t.jump, croc);
        croc::stomp::start_stomp(pad, &t.stomp, croc);
        croc::running::turn(pad, &t.running, croc);
        // Other actions' bodies; countdowns run down.
        croc::stomp::stomp(pad, &t.stomp, croc);
        croc::jump::count_down(croc);
        // Collision: the floor for the next tick.
        world::collide(floors, &mut croc.body);
    }

    fn observe(&mut self) -> Result<Observed, String> {
        Ok(self.world.croc.as_ref().map(croc::observe).unwrap_or_else(Observed::none))
    }
}

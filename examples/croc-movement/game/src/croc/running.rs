//! Croc's running. Ground movement (the MOVE opcode, 0x80043234)
//! and turning (the TURN opcode, 0x8004425C), with the digital pad's tank
//! controls (control mode 0x8007529C = 1): forward and back move, left and
//! right turn on the ground and strafe in the air, L1 and R1 sidestep while
//! Croc moves on the ground and turn in the air.
//!
//! Not ported (no recording reaches them on the flat stand-in floor): slippery surfaces,
//! slides, walls, platforms carrying Croc, the analog stick, and the input
//! lock of cutscenes. Standing still, L1 and R1 do nothing here: the
//! original's standing sidestep is the animation's own motion, not imported yet.
use psx_fixed::Fixed;
use psx_gte::math::{rcos, rsin};
use psx_scenario::Button;
use serde::Deserialize;

use super::Croc;
use crate::Controls;
use crate::character::Facing;
use crate::config::Tuning;

/// A speed in world units per tick with 12 fraction bits (gp+0x1D0, gp+0x1D4).
pub type Speed = Fixed<i32, 12>;
/// A turn rate in 1/4096 turns per tick with 16 fraction bits (gp+0x698).
pub type TurnRate = Fixed<i32, 16>;

/// Design numbers, read from `assets/croc/running.ron`.
#[derive(Deserialize, Clone, Debug)]
pub struct RunningTuning {
    /// Forward target, world units per tick (0x23 in MOVE). Like the other
    /// targets it is what the smoothing adds each tick, so Croc's speed
    /// settles at target x 2^`smoothing_shift`: 35 x 4 = 140 per tick
    /// running (run_and_stop's top speed).
    pub forward: i32,
    /// Backward target (-0xF in MOVE; settles at -60 per tick).
    pub backward: i32,
    /// Sidestep target on the ground with L1/R1 while moving (0xC in MOVE).
    pub sidestep: i32,
    /// Strafe target in the air with left/right (0x11 in MOVE).
    pub air_strafe: i32,
    /// Smoothing: each tick `v += target - v / 2^k` (k = 2 in MOVE). The
    /// speed settles at target x 2^k, so raising k gives gentler starts and
    /// stops and also multiplies every top speed by 2 per step.
    pub smoothing_shift: u32,
    /// Ticks of forward input that count as walking before running
    /// (gp+0x768 counted up to 5 in MOVE).
    pub walk_ticks: u8,
    pub turn_standing: TurnTuning,
    pub turn_moving: TurnTuning,
    /// A turn rate at least this keeps the turn going when its button is
    /// released, so it can wind down (0x6667 in TURN, 0x8004448C).
    pub turn_coasting: TurnRate,
    /// Winding down stops below this (3.0 in TURN, 0x800445C4).
    pub turn_stop: TurnRate,
}

/// How the turn rate builds up while a turn is held (TURN, 0x800445B8..).
#[derive(Deserialize, Clone, Debug)]
pub struct TurnTuning {
    /// The rate a turn starts at.
    pub start: TurnRate,
    /// The most it reaches.
    pub cap: TurnRate,
    /// Let go, each tick the rate keeps 1/2^this of itself, so a larger
    /// shift stops the turn sooner (TURN: >> 2 standing, 0x80044690; >> 3
    /// moving, 0x80044704).
    pub wind_down_shift: u32,
}

impl RunningTuning {
    pub const PATH: &str = "croc/running.ron";
}

impl Tuning for RunningTuning {
    fn validate(&self) -> Result<(), String> {
        for (name, t) in [("turn_standing", &self.turn_standing), ("turn_moving", &self.turn_moving)] {
            if !(TurnRate::zero() < t.start && t.start <= t.cap) {
                return Err(format!("{name}: 0 < start <= cap"));
            }
            // Below 4 raw bits the moving growth w + w/4 adds nothing to a
            // left turn (a right one still grows by the floor's -1).
            if t.start.bits() < 4 {
                return Err(format!("{name}: start at least 4/65536"));
            }
            // The last growth step below the cap reaches up to 2.5 x cap
            // (standing): keep it inside the 16.16 i32.
            if t.cap.bits() > i32::MAX / 5 * 2 {
                return Err(format!("{name}: cap at most 13107"));
            }
            if t.wind_down_shift == 0 || t.wind_down_shift > 16 {
                // 0 never winds down; past 16 a coasting rate stops at once.
                return Err(format!("{name}: wind_down_shift in 1..=16"));
            }
        }
        if self.turn_coasting <= TurnRate::zero() || self.turn_stop < TurnRate::zero() {
            return Err("turn_coasting: above 0; turn_stop: not below 0".into());
        }
        // The speed settles at target << k: keep it inside i32 at 12 bits.
        let fastest = [self.forward, self.backward, self.sidestep, self.air_strafe].map(i32::unsigned_abs);
        let fastest = fastest.into_iter().max().unwrap_or(0) as u64;
        if self.smoothing_shift > 12 || (fastest * 2 * 4096) << self.smoothing_shift > i32::MAX as u64 {
            return Err("smoothing_shift: too large for these speeds".into());
        }
        Ok(())
    }
}

/// How Croc moves on his feet (gp+0x694, "MOVESTATE").
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gait {
    #[default]
    Standing,
    Walking,
    Running,
    Backing,
}

/// Croc's ground movement: his smoothed speed, gait and turn rate.
#[derive(Clone, Copy, Debug, Default)]
pub struct Motion {
    pub speed_x: Speed,
    pub speed_z: Speed,
    pub gait: Gait,
    /// Ticks forward has been held, up to [`RunningTuning::walk_ticks`].
    pub walk_ticks: u8,
    pub turn_rate: TurnRate,
}

impl Motion {
    /// Opcode 0xA0 (0x8004317C): stop dead.
    pub fn stop(&mut self) {
        *self = Motion::default();
    }
}

/// libgte's sine and cosine of a facing, 4096 to one.
fn sin_cos(facing: Facing) -> (Speed, Speed) {
    let units = facing.units() as i32;
    (Speed::from_bits(rsin(units)), Speed::from_bits(rcos(units)))
}

/// MOVE (0x80043234): the target speed from the pad, the speed smoothed
/// toward it, and the position moved by the speed's whole units.
pub fn walk(pad: &Controls, tuning: &RunningTuning, croc: &mut Croc) {
    let Croc { body, action, motion: m, .. } = croc;
    let (position, facing, vertical) = (&mut body.position, &body.facing, &body.vertical);
    if !action.is_free() {
        return;
    }
    // Sideways: strafing in the air, sidestepping while moving on the
    // ground; the gait is last tick's (0x800436CC..0x80043780).
    let mut side = 0;
    if !vertical.on_ground() {
        if pad.held(Button::Left) {
            side = tuning.air_strafe;
        }
        if pad.held(Button::Right) {
            side = -tuning.air_strafe;
        }
    } else if m.gait != Gait::Standing {
        if pad.held(Button::L1) {
            side = tuning.sidestep;
        }
        if pad.held(Button::R1) {
            side = -tuning.sidestep;
        }
    }

    // Forward and back (0x80043784..0x8004383C).
    let forward = if pad.held(Button::Up) {
        // MOVE tests == 5; >= behaves the same for every count MOVE
        // reaches and stays sane if the count is edited past the tuning.
        if m.walk_ticks >= tuning.walk_ticks {
            m.gait = Gait::Running;
        } else {
            m.gait = Gait::Walking;
            m.walk_ticks += 1;
        }
        tuning.forward
    } else {
        m.walk_ticks = 0;
        if pad.held(Button::Down) {
            m.gait = Gait::Backing;
            tuning.backward
        } else {
            m.gait = Gait::Standing;
            0
        }
    };

    // The target along the facing, then smoothing (0x8004406C) and the
    // move by whole units (0x80044208, 0x80044224). On a static floor the
    // ground's formula reduces to the air's.
    let (sin, cos) = sin_cos(*facing);
    let target_x = sin * forward + cos * side;
    let target_z = cos * forward - sin * side;
    let k = tuning.smoothing_shift;
    m.speed_x = m.speed_x + target_x - m.speed_x.div_pow2_floor(k);
    m.speed_z = m.speed_z + target_z - m.speed_z.div_pow2_floor(k);
    position.x += m.speed_x.floor_int();
    position.z += m.speed_z.floor_int();
}

/// Which way a turn is going this tick (TURN's a2): held one way, coasting
/// one way after its button was released, or not turning.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Steer {
    Left,
    Right,
    CoastLeft,
    CoastRight,
    None,
}

/// TURN (0x8004425C): the facing turns by last tick's rate, then the rate
/// builds up while a turn is held, up to a cap, and winds down when it is
/// let go.
pub fn turn(pad: &Controls, tuning: &RunningTuning, croc: &mut Croc) {
    let Croc { body, action, motion: m, .. } = croc;
    let (facing, vertical) = (&mut body.facing, &body.vertical);
    if !action.is_free() {
        return;
    }
    let w = m.turn_rate;
    let mut steer = if w >= tuning.turn_coasting {
        Steer::CoastLeft
    } else if w <= -tuning.turn_coasting {
        // slti w, -0x6666 (0x800444AC): one past the mirror of the left's.
        Steer::CoastRight
    } else {
        Steer::None
    };
    // Left and right turn on the ground; in the air L1 and R1 do
    // (0x800444E0..0x80044578). Right wins over left.
    let (left, right) = if vertical.on_ground() { (Button::Left, Button::Right) } else { (Button::L1, Button::R1) };
    if pad.held(left) && steer != Steer::CoastRight {
        steer = Steer::Left;
    }
    if pad.held(right) && steer != Steer::CoastLeft {
        steer = Steer::Right;
    }
    if steer == Steer::None {
        return;
    }

    // The facing moves by the rate's whole units, before the rate changes
    // (lh of its high half, 0x80044584).
    *facing = facing.turned(w.floor_int());

    // Standing or backing turns faster than walking or running
    // (0x800445A0).
    let standing = matches!(m.gait, Gait::Standing | Gait::Backing);
    let t = if standing { &tuning.turn_standing } else { &tuning.turn_moving };
    let grow = |w: TurnRate| {
        // x2.5 standing (0x80044600: (w << 1) + (w >> 1)), x1.25 moving
        // (0x80044750: w + (w >> 2)).
        if standing { w * 2 + w.div_pow2_floor(1) } else { w + w.div_pow2_floor(2) }
    };
    let wind_down = t.wind_down_shift;
    m.turn_rate = match steer {
        Steer::Left if w < t.start => t.start,
        Steer::Left if w < t.cap => grow(w),
        Steer::Left => t.cap,
        Steer::Right if w > -t.start => -t.start,
        Steer::Right if w > -t.cap => grow(w),
        Steer::Right => -t.cap,
        Steer::CoastLeft if w > tuning.turn_stop => w.div_pow2_floor(wind_down),
        Steer::CoastRight if w < -tuning.turn_stop => w.div_pow2_floor(wind_down),
        _ => TurnRate::zero(),
    };
}

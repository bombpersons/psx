//! Where Croc (PAL) keeps the glossary's quantities.
//!
//! Evidence (CROC.EXE, gp = 0x80074B38):
//! - The logic step: the main loop adds 128 per field to the accumulator
//!   at 0x80075240 (gp+0x708, store at 0x80014618) and runs one step of
//!   logic per 256, subtracting it at 0x80013CA8 (loop 0x80013BB0..0x80013CE8):
//!   25 steps a second on a 50 Hz display. find_tick put that store in the
//!   group stored every other field.
//! - Croc's object: the word at 0x8007528C points at it (0x800D8A1C in 1-1).
//!   Position x, y, z at +0x370, +0x374, +0x378: 0x80043234 adds the smoothed
//!   speed to x and z (0x80044208, 0x80044224); y is written at 0x8002AC7C.
//!   Facing at +0x36A, 12 bits (0x80043234 masks it with 0xFFF when it turns Croc). Flags at
//!   +0x178, bit 1 set while standing (0x80043234 tests it to choose ground or
//!   air movement).
//! - Turn rate gp+0x698 (0x800751D0), 16.16, added >> 16 to the facing by
//!   TURN 0x8004425C. Gait ("MOVESTATE") gp+0x694 (0x800751CC), set by MOVE.
//! - Vertical: +0x150 speed (JUMP 0x80028688 sets 261; rise 0x8002D8D4 and
//!   fall 0x8002D810 change it), +0x14C the jump's height left (JUMP sets it,
//!   rise subtracts the speed).
//! - Croc's action: his script's global 0 (0x80086498, 16.16; switch at
//!   script offset 0x221B): 1 normal, 4 stomp.
//! - The smoothed speed: gp+0x1D0 and gp+0x1D4 (0x80074D08, 0x80074D0C),
//!   added >> 12 to x and z each step (0x80043234's end).

use glossary::{Gait, Quantities};
use psx_capture::prelude::*;

mod glue {
    include!(concat!(env!("OUT_DIR"), "/glue.rs"));
}

/// The word holding Croc's object's address.
const CROC: u32 = 0x8007_528C;
/// Croc's smoothed speed along x and z: gp+0x1D0, gp+0x1D4.
const SPEED_X: u32 = 0x8007_4D08;
const SPEED_Z: u32 = 0x8007_4D0C;
/// The turn rate, gp+0x698.
const TURN_RATE: u32 = 0x8007_51D0;
/// The gait, gp+0x694.
const GAIT: u32 = 0x8007_51CC;
/// The player script's global 0, 16.16.
const ACTION: u32 = 0x8008_6498;

struct Croc;

impl Glossary for Croc {
    type Glue = glue::Glossary;
    const TITLE: &str = "Croc - Legend of the Gobbos (Europe) SLES-00593";
    const IMAGE_SHA256: &str = "b30a07f5d5f9681c3943c21af5b9926d111ee2ba1d49024794369aa970b77aad";

    fn tick(&self, t: &mut Tick) {
        t.store(0x8007_5240, 4).by(0x8001_3CA8);
    }

    fn quantities(&self, m: &Machine) -> Quantities {
        let croc = m.u32(CROC);
        Quantities {
            croc_x: m.i32(croc + 0x370),
            croc_y: m.i32(croc + 0x374),
            croc_z: m.i32(croc + 0x378),
            croc_angle: m.u16(croc + 0x36A),
            croc_on_ground: m.u32(croc + 0x178) & 2 != 0,
            croc_speed_x: m.i32(SPEED_X),
            croc_speed_z: m.i32(SPEED_Z),
            croc_turn_rate: m.i32(TURN_RATE),
            croc_vertical_speed: m.i32(croc + 0x150),
            croc_jump_height_left: m.i32(croc + 0x14C),
            croc_gait: match m.u32(GAIT) {
                0 => Gait::Standing,
                1 => Gait::Walking,
                2 => Gait::Running,
                3 => Gait::Backing,
                4 => Gait::Sliding,
                5 => Gait::SlidingSteep,
                other => panic!("gait {other}: not a state MOVE (0x80043234) sets"),
            },
            croc_action: m.i32(ACTION) >> 16,
        }
    }
}

psx_capture::export_glossary!(Croc);

//! F-0003: Croc's stomp. A second press of jump, high enough in a jump,
//! stops Croc in the air for the length of his stomp animation, then drops
//! him; after landing he settles and recovers before he can move again.
//! The script's action 4 (script 0x2732), with the opcodes 0x76 (hang,
//! 0x80028734), 0x73 (drop, 0x800286F4) and 0xA0 (stop, 0x8004317C).
//!
//! Not ported: the grunts and the landing sound, the camera wobble, and the
//! attack lockout after a stomp (no tail attack in the port yet).
//!
//! Provisional: the hang and the settling last as long as two of Croc's
//! animations (CROCSMP1, CROCSMP3), whose lengths stand in [`StompTuning`]
//! until his animations are imported (F-0004 needs them too). Stomping
//! again from the settle (jump held, still high above the floor) needs the
//! floor to drop away under Croc, so no flat-floor recording reaches it: it
//! is read from the script, not verified. That second stomp has no hang:
//! the script's settle stops the animation and goes back to its hang step,
//! whose check, later in the same tick, finds the animation over and drops
//! him.
use psx_scenario::Button;
use serde::Deserialize;

use super::{ActionKind, Croc};
use crate::Controls;
use crate::character::Clip;
use crate::config::Tuning;

/// Design numbers, read from `assets/croc/stomp.ron`.
#[derive(Deserialize, Clone, Debug)]
pub struct StompTuning {
    /// A stomp needs Croc more than this above the floor (0x3D7, script 0x18C0).
    pub min_height: i32,
    /// The hang: frames of the stomp's first animation (CROCSMP1), one a
    /// tick: 15, from the recording stomp_at_threshold. Stands in for the
    /// animation's own length until animations are imported.
    pub hang_frames: u16,
    /// The drop's starting speed, world units per tick (0x180, opcode 0x73).
    pub drop_speed: i32,
    /// Frames of the landing animation (CROCSMP3): 2, from the recordings.
    /// Stands in for the animation's length, as `hang_frames` does.
    pub land_frames: u16,
    /// The recovery's last count. The settle tick starts the count at 0; it
    /// goes up by one each tick after that, and the tick it reaches this
    /// value the stomp ends, so Croc's stomp holds him for this + 1 ticks
    /// after the settle tick and he moves on the next (11: free on the 13th).
    /// The script counts its step from 5 to 16 (script 0x2732).
    pub recovery_ticks: u8,
    /// Holding jump as he settles stomps again from this high above the
    /// floor (0x83, script 0x27C6).
    pub again_min_height: i32,
}

impl StompTuning {
    pub const PATH: &str = "croc/stomp.ron";
}

impl Tuning for StompTuning {
    fn validate(&self) -> Result<(), String> {
        if self.drop_speed <= 0 {
            return Err("drop_speed: above 0".into());
        }
        if self.hang_frames == 0 || self.land_frames == 0 {
            return Err("hang_frames, land_frames: at least 1".into());
        }
        Ok(())
    }
}

/// Where a stomp is (script local L104): the step of [`ActionKind::Stomp`].
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stomp {
    /// Stop and hang (L104 0).
    #[default]
    Begin,
    /// Hanging until the animation ends (1).
    Hang,
    /// Dropping (2).
    Drop,
    /// Just landed (3).
    Landed,
    /// The landing animation (4).
    Settle,
    /// Ticks recovering (5..16).
    Recover(u8),
}

/// The stomp test (script 0x18B9): a press in a jump, high enough, starts a
/// stomp from the next tick.
/// It runs after the jump test in the same sub (script 0x18B9), as it
/// clears the press the jump test kept.
pub fn start_stomp(pad: &Controls, tuning: &StompTuning, croc: &mut Croc) {
    let Croc { body, action, jump, .. } = croc;
    if action.is_free()
        && jump.from_jump
        && pad.pressed(Button::Cross)
        && body.ground.clearance(body.position.y) > tuning.min_height
    {
        jump.buffer = 0;
        jump.from_jump = false;
        action.start(ActionKind::Stomp(Stomp::Begin));
    }
}

/// The stomp's body (the script's action 4, 0x2732): one step a tick.
pub fn stomp(pad: &Controls, tuning: &StompTuning, croc: &mut Croc) {
    let Croc { body, action, motion, .. } = croc;
    let (v, clip, position, ground, landed) =
        (&mut body.vertical, &mut body.clip, &mut body.position, &body.ground, body.landed);
    let ActionKind::Stomp(step) = &mut action.current else { return };
    match *step {
        Stomp::Begin => {
            v.hang();
            motion.stop();
            *clip = Clip::play(tuning.hang_frames);
            *step = Stomp::Hang;
        }
        Stomp::Hang if !clip.playing => {
            v.fall(tuning.drop_speed);
            *step = Stomp::Drop;
        }
        Stomp::Drop if landed => {
            position.y = ground.height;
            *step = Stomp::Landed;
        }
        Stomp::Landed => {
            *clip = Clip::play(tuning.land_frames);
            *step = Stomp::Settle;
        }
        // Jump held and still high: stomp again, straight into the drop
        // with no hang (the one step that moves twice in a tick).
        Stomp::Settle
            if !clip.playing && pad.held(Button::Cross) && ground.clearance(position.y) > tuning.again_min_height =>
        {
            clip.stop();
            v.fall(tuning.drop_speed);
            *step = Stomp::Drop;
        }
        Stomp::Settle if !clip.playing => *step = Stomp::Recover(0),
        Stomp::Recover(ticks) if ticks == tuning.recovery_ticks => action.start(ActionKind::Free),
        Stomp::Recover(ticks) => *step = Stomp::Recover(ticks + 1),
        // Waiting for the animation, or for the landing.
        Stomp::Hang | Stomp::Drop | Stomp::Settle => {}
    }
}

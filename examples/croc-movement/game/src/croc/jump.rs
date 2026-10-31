//! F-0002: Croc's jump. A press of jump starts it (script sub 0x1803 and
//! the JUMP opcode 0x80028688), a rise that the body's [`crate::character::vertical`] carries:
//! it slows near the top, and sooner when jump is let go, then falls and
//! lands. The script's landing handler (0x119E) resets the jump. A press
//! while Croc cannot jump yet is kept for a few ticks.
//!
//! Not ported: bounce pads (script global 69: the jump test jumps 4600
//! with jump pressed, else 3450), and the bounce off a surface type 4
//! (lava) floor, a fixed JUMP 2300 that is the only thing turning the
//! variable height off (script 0x1A76; wont_reach 0x8002D940). Nor is the
//! jump test's other condition: it is skipped while Croc slides on a
//! surface of type 8 (MOVESTATE 5, `Gait::SlidingSteep`; sub 0x1803), so
//! sliding neither jumps nor keeps a press. No such surface on the flat
//! floor. The fall's own unported cases are listed there.
use bevy::prelude::*;
use psx_bevy::controller::Button;
use psx_bevy::prelude::*;
use serde::Deserialize;

use super::running::RunningSet;
use super::{Action, Croc};
use crate::character::{Clip, Ground, Landed, Position, RiseHeld, Vertical};
use crate::config::{Tuning, TuningPlugin};
use crate::tick::TickPhase;

/// Design numbers, read from `assets/croc/jump.ron`.
#[derive(Resource, Deserialize, Clone, Debug)]
pub struct JumpTuning {
    /// How high a jump may take Croc, world units (script 0x188F: JUMP 2300).
    pub height: i32,
    /// Rising speed, world units per tick (JUMP: 0x105, stored at 0x800286B8).
    pub speed: i32,
    /// What the kept-press counter is set to when jump is pressed while Croc
    /// cannot jump (script 0x18B0: 5). It counts down at the end of every
    /// tick, the one it is set in too, so the press is kept for this many
    /// ticks less one.
    pub buffer_ticks: u8,
    /// What the no-jump counter is set to on landing (script 0x11C5: 1).
    /// It counts down at the end of the landing tick, so 1 only stops a
    /// jump on the landing tick itself.
    pub landing_lockout: u8,
}

impl JumpTuning {
    pub const PATH: &str = "croc/jump.ron";
}

impl Tuning for JumpTuning {
    fn validate(&self) -> Result<(), String> {
        if self.height <= 0 || self.speed <= 0 {
            return Err("height, speed: above 0".into());
        }
        Ok(())
    }
}

/// What the jump keeps between ticks: the script's locals for it.
#[derive(Component, Reflect, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct JumpState {
    /// No jump while non-zero: set on landing (L151).
    pub lockout: u8,
    /// A jump pressed while Croc could not jump, kept for a few ticks (L152).
    pub buffer: u8,
    /// Releasing jump cuts the rise short (+0x174 bit 0x2000000, set on
    /// landing by script 0x11B9; on in 1-1).
    pub variable_height: bool,
    /// This time off the ground began with a jump, so it may become a stomp
    /// (script local L103: set by a jump, cleared on landing).
    pub from_jump: bool,
}

impl Default for JumpState {
    fn default() -> Self {
        JumpState { lockout: 0, buffer: 0, variable_height: true, from_jump: false }
    }
}

/// The jump test in [`TickPhase::Control`], after MOVE (script sub 0x1803).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct JumpTest;

pub struct JumpPlugin;

impl Plugin for JumpPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            FixedUpdate,
            JumpTest.in_set(TickPhase::Control).after(RunningSet::Walk).before(RunningSet::Turn),
        )
        .add_plugins(TuningPlugin::<JumpTuning>::new(JumpTuning::PATH))
        .add_systems(
            FixedUpdate,
            (
                hold_rise.in_set(TickPhase::Input),
                land.in_set(TickPhase::Reactions),
                take_off.in_set(JumpTest),
                count_down.in_set(TickPhase::Timers),
            ),
        );
    }
}

/// Holding jump keeps the rise going; let go, a variable-height jump is cut
/// short near its top (0x8002D8D4 tests the pad and +0x174 bit 0x2000000).
fn hold_rise(pad: Res<ControllerState>, mut q: Query<(&JumpState, &mut RiseHeld), With<Croc>>) {
    for (jump, mut held) in &mut q {
        held.0 = !jump.variable_height || pad.held(Button::Cross);
    }
}

/// The landing's handler (script 0x119E), the tick Croc lands, before the
/// script resumes.
fn land(tuning: Res<JumpTuning>, mut q: Query<(&mut JumpState, &mut Position, &Ground), With<Landed>>) {
    for (mut jump, mut position, ground) in &mut q {
        jump.variable_height = true;
        position.y = ground.height;
        jump.lockout = tuning.landing_lockout;
        jump.from_jump = false;
    }
}

/// The jump test (script sub 0x1803, after MOVE): a press, or one kept
/// from before, jumps when Croc is neither rising nor falling (the script
/// tests the JUMPING and FALLING flags, not on-ground) and not just landed;
/// otherwise a press is kept. Any later state that leaves Croc off the
/// ground in the free action without rising or falling could jump from it.
fn take_off(
    pad: Res<ControllerState>,
    tuning: Res<JumpTuning>,
    mut q: Query<(&Action, &mut Vertical, &mut JumpState, &mut Clip), With<Croc>>,
) {
    for (action, mut v, mut jump, mut clip) in &mut q {
        if !action.is_free() {
            continue;
        }
        let pressed = pad.pressed(Button::Cross);
        if !pressed && jump.buffer == 0 {
            continue;
        }
        if !v.rising_or_falling() && jump.lockout == 0 {
            v.jump(tuning.height, tuning.speed);
            jump.from_jump = true;
            clip.stop();
        } else if pressed {
            jump.buffer = tuning.buffer_ticks;
        }
    }
}

/// The countdowns at the end of every tick (script 0x3649..0x3687).
fn count_down(mut q: Query<&mut JumpState>) {
    for mut jump in &mut q {
        jump.lockout = jump.lockout.saturating_sub(1);
        jump.buffer = jump.buffer.saturating_sub(1);
    }
}

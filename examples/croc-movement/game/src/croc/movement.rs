//! Croc's movement rules: walking, turning, jumping and falling.
//!
//! Every rule was measured on the PAL release (SLES-00593) in level 1-1 with the
//! psx toolkit; the write-up with all evidence is `research/case-study-croc.md`
//! in the psx repository. Comments cite the original's RAM variable or function
//! address so each number can be re-checked.
//!
//! Conventions:
//! - Lengths are in the original's world units ("Croc units"), converted to Bevy
//!   units with [`units`]. Croc's own coordinates are +Y up with +Z forward at
//!   heading 0; here that becomes Bevy's +Y up with `Transform::forward()`
//!   (-Z) forward, i.e. the same space rotated 180° about Y.
//! - Angles are radians, built with [`turns_4096`] from the original's
//!   4096-per-revolution values. A larger yaw turns Croc to his left.
//! - Rates are per game tick. Croc's logic runs once per two PAL vblanks
//!   (25 Hz); see [`VBLANKS_PER_TICK`]. There is no delta time in the original: a slow
//!   frame slows the game down, so the rules here are per tick, not per second.
use bevy::prelude::*;
use engine::controller::{Button, ControllerState};
use std::f32::consts::TAU;

/// PAL vblanks per game tick (25 Hz). The main loop (0x800145D8) waits until two vblanks
/// have passed since the previous frame; every player variable is written on
/// alternate PAL frames in all recorded runs.
pub const VBLANKS_PER_TICK: u32 = 2;

/// Display scale: Croc units per Bevy unit. Arbitrary, chosen so Croc is
/// roughly a metre tall on screen; the rules do not depend on it.
pub const UNITS_PER_BEVY: f32 = 512.0;

/// A length in Croc units, as Bevy units.
pub const fn units(croc_units: f32) -> f32 {
    croc_units / UNITS_PER_BEVY
}

/// An angle in the original's 4096-per-turn units, as radians.
pub const fn turns_4096(value: f32) -> f32 {
    value * TAU / 4096.0
}

// --- Walking (player control, 0x80043234) ----------------------------------
// Planar velocity lives in gp+0x1D0/0x1D4 (0x80074D08/0x80074D0C, ×4096).
// Each tick: v ← v + (target − v)/4, then position += v.

/// Top forward speed: the control code asks for 35 (0x800437F8) and blends a
/// quarter of the gap each tick, which settles at 4 × 35 = 140 units/tick.
pub const RUN_SPEED: f32 = units(140.0);
/// Walking backwards with Down: 4 × 15 (0x80043828).
pub const BACK_SPEED: f32 = units(60.0);
/// Sidestep with L1/R1 while walking: 4 × 12 (0x800438A4).
pub const SIDESTEP_SPEED: f32 = units(48.0);
/// Left/Right in the air steer sideways instead of turning: 4 × 17 (0x800438C0).
pub const AIR_STRAFE_SPEED: f32 = units(68.0);
/// Fraction of the gap to the target velocity closed per tick (`v >> 2`).
pub const VELOCITY_BLEND: f32 = 0.25;

// --- Turning (0x8004425C) ---------------------------------------------------
// Turn rate lives in gp+0x698 (0x800751D0, 16.16). Holding a direction starts
// the turn at a fixed rate and multiplies it each tick until it passes a cap;
// the cap is checked before multiplying, so the first tick above it overshoots.
// Letting go divides the rate until it falls below 3, then it stops.

/// Turning profile; Croc turns faster on the spot than while walking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurnProfile {
    pub start: f32,
    pub growth: f32,
    pub cap: f32,
    pub release_divisor: f32,
}

/// Standing still or walking backwards (gait 0 or 3): 3, ×2.5, cap 63, ÷4.
pub const STANDING_TURN: TurnProfile =
    TurnProfile { start: turns_4096(3.0), growth: 2.5, cap: turns_4096(63.0), release_divisor: 4.0 };
/// Walking forward: 11, ×1.25, cap 52, ÷8.
pub const WALKING_TURN: TurnProfile =
    TurnProfile { start: turns_4096(11.0), growth: 1.25, cap: turns_4096(52.0), release_divisor: 8.0 };
/// Below this the turn rate snaps to zero once the button is released.
pub const TURN_STOP: f32 = turns_4096(3.0);
/// A leftover turn rate below this does not keep turning (0.4 in 16.16: 26215).
pub const TURN_COAST_MIN: f32 = turns_4096(0.4);

// --- Jumping and falling (0x80028688, 0x8002D8D4, 0x8002D810) ---------------
// Vertical speed is object+0x150, the remaining rise budget object+0x14C.

/// Rise speed at take-off (0x800286B4).
pub const JUMP_SPEED: f32 = units(261.0);
/// Height budget of one jump (object+0x14C; 2300 in every recorded jump,
/// supplied by Croc's behaviour script through 0x8002F168).
pub const JUMP_HEIGHT: f32 = units(2300.0);
/// With less than this budget left the rise slows: v ← v − v/4 − 8.
pub const JUMP_EASE_BUDGET: f32 = units(700.0);
pub const JUMP_EASE_DRAG: f32 = units(8.0);
/// With less than this left and Cross released, the rise speed halves each
/// tick, which is what makes a tap jump lower than a held one.
pub const JUMP_RELEASE_BUDGET: f32 = units(1400.0);
/// The rise ends when the budget or the speed drops below this.
pub const JUMP_END: f32 = units(4.0);
/// Falling: v ← v + v/4 + 8, capped at 576 units/tick.
pub const FALL_GROWTH: f32 = 1.25;
pub const FALL_KICK: f32 = units(8.0);
pub const MAX_FALL_SPEED: f32 = units(576.0);
/// Croc's behaviour script puts him on the floor once he is falling and less
/// than this above it (constant 131 in the script bytecode at 0x800DA743;
/// observed snaps at 7–29 units, none at 223 or more).
pub const GROUND_SNAP: f32 = units(131.0);

/// The player.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
#[require(Transform, Velocity, Facing, Gait, Vertical, Ground)]
pub struct Croc;

/// Horizontal velocity in Bevy units per tick.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component)]
pub struct Velocity(pub Vec3);

/// Yaw (radians, larger = further left) and its current turn rate per tick.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component)]
pub struct Facing {
    pub yaw: f32,
    pub turn_rate: f32,
}

/// What the legs are doing; selects speed and turning profile (gp+0x694,
/// 0x800751CC: 0 idle, 1/2 forward, 3 backwards).
#[derive(Component, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[reflect(Component)]
pub enum Gait {
    #[default]
    Idle,
    Forward,
    Backward,
}

impl Gait {
    fn turn_profile(self) -> TurnProfile {
        match self {
            Gait::Forward => WALKING_TURN,
            Gait::Idle | Gait::Backward => STANDING_TURN,
        }
    }
}

/// Vertical state. Speeds are positive in the direction of travel.
#[derive(Component, Reflect, Debug, Default, Clone, Copy, PartialEq)]
#[reflect(Component)]
pub enum Vertical {
    #[default]
    Grounded,
    Rising {
        speed: f32,
        budget: f32,
    },
    Falling {
        speed: f32,
    },
}

impl Vertical {
    pub fn grounded(&self) -> bool {
        matches!(self, Vertical::Grounded)
    }
}

/// Height of the floor under Croc (object+0x38). The demo's level is flat.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component)]
pub struct Ground {
    pub height: f32,
}

/// Movement systems in the order the original runs them within a tick:
/// vertical motion, walking, turning, then the behaviour script (ground snap,
/// jump start). Walking therefore uses last tick's heading, and a jump pressed
/// this tick starts rising next tick.
pub struct CrocMovementPlugin;

impl Plugin for CrocMovementPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            FixedUpdate,
            (fall_and_rise, walk, turn, snap_to_ground, start_jump).chain().in_set(engine::shell::GameSystems::Game),
        );
    }
}

/// +1 for the left-hand button, -1 for the right-hand one; the right-hand
/// button wins when both are held, as in the original's test order.
fn left_or_right(pad: &ControllerState, left: Button, right: Button) -> f32 {
    if pad.held(right) {
        -1.0
    } else if pad.held(left) {
        1.0
    } else {
        0.0
    }
}

/// Rise while the jump budget lasts, then fall faster and faster until the
/// floor (0x8002D8D4 rising, 0x8002D810 falling).
fn fall_and_rise(pad: Res<ControllerState>, mut q: Query<(&mut Transform, &mut Vertical, &Ground), With<Croc>>) {
    for (mut transform, mut vertical, ground) in &mut q {
        match *vertical {
            Vertical::Grounded => {}
            Vertical::Rising { mut speed, mut budget } => {
                transform.translation.y += speed;
                budget -= speed;
                if budget < JUMP_EASE_BUDGET {
                    speed -= speed * 0.25 + JUMP_EASE_DRAG;
                }
                if budget < JUMP_RELEASE_BUDGET && !pad.held(Button::Cross) {
                    speed *= 0.5;
                }
                *vertical = if budget < JUMP_END || speed < JUMP_END {
                    Vertical::Falling { speed: 0.0 }
                } else {
                    Vertical::Rising { speed, budget }
                };
            }
            Vertical::Falling { speed } => {
                transform.translation.y -= speed;
                *vertical = if transform.translation.y <= ground.height {
                    transform.translation.y = ground.height;
                    Vertical::Grounded
                } else {
                    Vertical::Falling { speed: (speed * FALL_GROWTH + FALL_KICK).min(MAX_FALL_SPEED) }
                };
            }
        }
    }
}

/// Up walks forward, Down backs off; L1/R1 sidestep while walking, and
/// Left/Right steer sideways in the air. Velocity eases towards the target.
fn walk(
    pad: Res<ControllerState>,
    mut q: Query<(&mut Transform, &mut Velocity, &mut Gait, &Facing, &Vertical), With<Croc>>,
) {
    for (mut transform, mut velocity, mut gait, facing, vertical) in &mut q {
        // Sidestepping checks last tick's gait (gp+0x694 is read before Up is).
        let sideways = if !vertical.grounded() {
            AIR_STRAFE_SPEED * left_or_right(&pad, Button::Left, Button::Right)
        } else if *gait != Gait::Idle {
            SIDESTEP_SPEED * left_or_right(&pad, Button::L1, Button::R1)
        } else {
            0.0
        };
        *gait = if pad.held(Button::Up) {
            Gait::Forward
        } else if pad.held(Button::Down) {
            Gait::Backward
        } else {
            Gait::Idle
        };
        let forward_speed = match *gait {
            Gait::Forward => RUN_SPEED,
            Gait::Backward => -BACK_SPEED,
            Gait::Idle => 0.0,
        };
        let heading = Quat::from_rotation_y(facing.yaw);
        let target = heading * (Vec3::NEG_Z * forward_speed + Vec3::NEG_X * sideways);
        velocity.0 = velocity.0.lerp(target, VELOCITY_BLEND);
        transform.translation += velocity.0;
    }
}

/// Left/Right turn on the ground; in the air L1/R1 do. Holding accelerates the
/// turn up to a cap, letting go lets it run down.
fn turn(pad: Res<ControllerState>, mut q: Query<(&mut Transform, &mut Facing, &Gait, &Vertical), With<Croc>>) {
    for (mut transform, mut facing, gait, vertical) in &mut q {
        let (left, right) = if vertical.grounded() { (Button::Left, Button::Right) } else { (Button::L1, Button::R1) };
        let coasting = if facing.turn_rate.abs() >= TURN_COAST_MIN { facing.turn_rate.signum() } else { 0.0 };
        // A button only counts when the turn is not still running the other way.
        let mut steer = 0.0;
        if pad.held(left) && coasting >= 0.0 {
            steer = 1.0;
        }
        if pad.held(right) && coasting <= 0.0 {
            steer = -1.0;
        }
        if steer == 0.0 && coasting == 0.0 {
            continue;
        }
        facing.yaw = (facing.yaw + facing.turn_rate).rem_euclid(TAU);
        let profile = gait.turn_profile();
        let rate = facing.turn_rate;
        facing.turn_rate = if steer != 0.0 {
            let speed = rate * steer;
            steer
                * if speed < profile.start {
                    profile.start
                } else if speed < profile.cap {
                    speed * profile.growth
                } else {
                    profile.cap
                }
        } else if rate.abs() > TURN_STOP {
            rate / profile.release_divisor
        } else {
            0.0
        };
        transform.rotation = Quat::from_rotation_y(facing.yaw);
    }
}

/// The behaviour script drops a falling Croc onto the floor when he is close.
fn snap_to_ground(mut q: Query<(&mut Transform, &Vertical, &Ground), With<Croc>>) {
    for (mut transform, vertical, ground) in &mut q {
        if matches!(vertical, Vertical::Falling { .. }) && transform.translation.y - ground.height < GROUND_SNAP {
            transform.translation.y = ground.height;
        }
    }
}

/// Pressing Cross on the ground starts a jump; it rises from the next tick.
fn start_jump(pad: Res<ControllerState>, mut q: Query<&mut Vertical, With<Croc>>) {
    for mut vertical in &mut q {
        if vertical.grounded() && pad.pressed(Button::Cross) {
            *vertical = Vertical::Rising { speed: JUMP_SPEED, budget: JUMP_HEIGHT };
        }
    }
}

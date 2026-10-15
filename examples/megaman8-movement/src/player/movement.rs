//! Mega Man's ground and air movement, as measured on Mega Man 8 (PAL, SLES-01064).
//!
//! Units: stage pixels (x right, y down, as in the level data) and simulation
//! ticks. The game's logic runs once per PAL field (50 Hz), and its speeds are
//! 16.16 fixed-point pixels per tick, so the constants below are those exact
//! values (all are multiples of 1/256 and therefore exact in `f32`). The
//! per-second equivalents are given for reference only: the behaviour (e.g. how
//! many ticks the jump lasts, where it lands) depends on the per-tick values.
//!
//! Evidence: research/case-study-megaman8.md. RAM addresses refer to the
//! player object at 0x8015E89C; code addresses are in SLES_010.64.
use super::{Facing, Player, PlayerState, Velocity};
use crate::{
    controller::{Button, ControllerState},
    shell::GameSystems,
    stage::{Solid, StagePos},
};
use bevy::prelude::*;

/// Walk and air-control speed: 0x00018000 = 1.5 px/tick (75 px/s).
/// Written by 0x8010C914 (walk) and 0x8010F6F0 (air control).
pub const WALK_SPEED: f32 = 1.5;
/// Initial upward jump speed: 0x0005C900 = 5.78515625 px/tick (289.3 px/s).
/// Written by 0x8010C9C4 on the first tick of the jump state.
pub const JUMP_SPEED: f32 = 5.785_156_3;
/// Gravity: 0x4000 = 0.25 px/tick² (625 px/s²), copied from the object's
/// +0x54 to +0x20 by the air states (0x8010F71C / 0x8010F7B0).
pub const GRAVITY: f32 = 0.25;
/// Terminal fall speed: 0xFFFA0000 = -6 px/tick upward, i.e. 6 px/tick down
/// (300 px/s), object +0x50, clamped after moving by 0x8010F7E4.
pub const MAX_FALL_SPEED: f32 = 6.0;
/// Ticks of the "inching" step before walking, with its 1 px nudge
/// (0x8010C7D8 sets the timer to 5; 0x8010C7F4 nudges x by ±1).
pub const STEP_TICKS: u8 = 5;
pub const STEP_NUDGE: f32 = 1.0;
/// After stopping or landing, a direction pressed within this many ticks skips
/// the step and walks at once (object +0x95, set to 5 by 0x8010C900 / 0x8010CAC4).
pub const WALK_GRACE_TICKS: u8 = 5;
/// The landing pose runs for 11 ticks after the landing tick (animation-driven
/// in the original, object +0x2E; measured, not read from the animation data).
pub const LAND_TICKS: u8 = 11;
/// Distance from the player's origin (sprite centre) to the first row below
/// the feet: a 48 px tall body centred on the origin, plus one.
pub const FEET_OFFSET: f32 = 25.0;

/// The jump button (the game accepts Cross or Circle: held-mask 0x60 at 0x8010F740).
fn jump_held(pad: &ControllerState) -> bool {
    pad.held(Button::Cross) || pad.held(Button::Circle)
}
fn jump_pressed(pad: &ControllerState) -> bool {
    pad.pressed(Button::Cross) || pad.pressed(Button::Circle)
}
/// -1, 0 or +1. Left wins when both are held (the game tests 0x8000 first).
fn horizontal(pad: &ControllerState) -> f32 {
    if pad.held(Button::Left) {
        -1.0
    } else if pad.held(Button::Right) {
        1.0
    } else {
        0.0
    }
}

/// Grounded flag (object +0x66 bit 7): set when the floor stopped a fall this
/// tick, or when a ground state is still standing on something.
#[derive(Component, Reflect, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Component)]
pub struct Grounded(pub bool);

/// Ticks left in which a direction press walks without the step (object +0x95).
#[derive(Component, Reflect, Default, Debug, Clone, Copy)]
#[reflect(Component)]
pub struct WalkGrace(pub u8);

/// The movement pipeline; run camera and other followers `.after(PlayerMovement)`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlayerMovement;

pub struct MovementPlugin;

impl Plugin for MovementPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            FixedUpdate,
            (control, integrate, after_move, collide_with_floor)
                .chain()
                .in_set(PlayerMovement)
                .in_set(GameSystems::Game),
        );
    }
}

type ControlledPlayer = (
    &'static mut PlayerState,
    &'static mut Velocity,
    &'static mut StagePos,
    &'static mut Facing,
    &'static Grounded,
    &'static mut WalkGrace,
);

/// Per-state input handling before the move: state changes, velocity, gravity.
/// A state change takes effect on the next tick, as in the original state
/// machine (the state byte at +0x02 is dispatched once per tick from 0x8010C58C).
fn control(pad: Res<ControllerState>, mut players: Query<ControlledPlayer, With<Player>>) {
    let dir = horizontal(&pad);
    for (mut state, mut vel, mut pos, mut facing, grounded, mut grace) in &mut players {
        if dir != 0.0 {
            *facing = if dir < 0.0 { Facing::Left } else { Facing::Right };
        }
        let mut next = *state;
        match *state {
            // 0x8010C67C: count the grace timer down, start walking on a direction.
            PlayerState::Stand | PlayerState::Land { .. } => {
                vel.0 = Vec2::ZERO;
                grace.0 = grace.0.saturating_sub(1);
                if let PlayerState::Land { ticks_left } = *state {
                    // 0x8010CB08: the landing pose ends on its own.
                    next = if ticks_left <= 1 {
                        PlayerState::Stand
                    } else {
                        PlayerState::Land { ticks_left: ticks_left - 1 }
                    };
                }
                if dir != 0.0 {
                    next = if grace.0 > 0 { PlayerState::Walk } else { PlayerState::Step { ticks_left: 0 } };
                }
            }
            // 0x8010C798: on entry nudge 1 px, then wait STEP_TICKS before walking.
            PlayerState::Step { ticks_left } => {
                vel.0 = Vec2::ZERO;
                let mut left = ticks_left;
                if left == 0 {
                    pos.0.x += STEP_NUDGE * facing.sign();
                    left = STEP_TICKS;
                }
                left -= 1;
                next = if left == 0 { PlayerState::Walk } else { PlayerState::Step { ticks_left: left } };
                if dir == 0.0 {
                    next = PlayerState::Stand;
                }
            }
            // 0x8010C89C: constant speed while a direction is held.
            PlayerState::Walk => {
                vel.0 = Vec2::new(dir * WALK_SPEED, 0.0);
                if dir == 0.0 {
                    next = PlayerState::Stand;
                    grace.0 = WALK_GRACE_TICKS;
                }
            }
            // 0x8010C978 + 0x8010F6FC: launch on the first tick, then rise under gravity.
            PlayerState::Jump { launched } => {
                if !launched {
                    vel.0.y = -JUMP_SPEED;
                    next = PlayerState::Jump { launched: true };
                }
                vel.0.y += GRAVITY;
                vel.0.x = dir * WALK_SPEED; // 0x8010F6C0: full air control, no inertia
            }
            // 0x8010CA58 + 0x8010F790.
            PlayerState::Fall => {
                vel.0.y += GRAVITY;
                vel.0.x = dir * WALK_SPEED;
            }
        }
        if state.on_ground() {
            // 0x8010F8C8: jump on a press while grounded; 0x8010F908: walk off a ledge.
            if !grounded.0 {
                next = PlayerState::Fall;
            } else if jump_pressed(&pad) {
                next = PlayerState::Jump { launched: false };
            }
        }
        if next != *state {
            *state = next;
        }
    }
}

/// x += vx, y += vy (0x80105018 / 0x80104FF4).
fn integrate(mut bodies: Query<(&mut StagePos, &Velocity)>) {
    for (mut pos, vel) in &mut bodies {
        pos.0 += vel.0;
    }
}

/// Rules the original applies after moving: variable jump height, the apex,
/// terminal speed, and landing (which uses the grounded flag from the previous tick).
fn after_move(
    pad: Res<ControllerState>,
    mut players: Query<(&mut PlayerState, &mut Velocity, &Grounded, &mut WalkGrace), With<Player>>,
) {
    for (mut state, mut vel, grounded, mut grace) in &mut players {
        match *state {
            PlayerState::Jump { .. } => {
                // 0x8010F738..0x8010F774: releasing jump, or passing the apex, stops the
                // rise at once (vy = 0); the fall then starts from rest.
                if !jump_held(&pad) || vel.0.y > 0.0 {
                    vel.0.y = 0.0;
                    *state = PlayerState::Fall;
                }
            }
            PlayerState::Fall => {
                vel.0.y = vel.0.y.min(MAX_FALL_SPEED); // 0x8010F7CC..0x8010F7E4
                if grounded.0 {
                    // 0x8010CA9C: keep walking if a direction is held, else land.
                    vel.0 = Vec2::ZERO;
                    if horizontal(&pad) != 0.0 {
                        *state = PlayerState::Walk;
                    } else {
                        *state = PlayerState::Land { ticks_left: LAND_TICKS };
                        grace.0 = WALK_GRACE_TICKS;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Floor collision. A fall that sinks into a floor is pushed back out by
/// replacing the integer part of y only; the sub-pixel part is kept
/// (0x80106E40 writes the high halfword of y). Standing states stay grounded
/// while the row under the feet is solid.
fn collide_with_floor(
    solids: Query<&Solid>,
    mut players: Query<(&mut StagePos, &PlayerState, &mut Grounded), With<Player>>,
) {
    for (mut pos, state, mut grounded) in &mut players {
        let feet = pos.0.y.floor() + FEET_OFFSET;
        let floor =
            solids.iter().filter(|s| s.0.min.x <= pos.0.x && pos.0.x < s.0.max.x).map(|s| s.0.min.y).reduce(f32::min);
        grounded.0 = false;
        let Some(top) = floor else { continue };
        if feet > top && !matches!(state, PlayerState::Jump { .. }) {
            pos.0.y = top - FEET_OFFSET + pos.0.y.fract();
            grounded.0 = true;
        } else if state.on_ground() && feet >= top {
            grounded.0 = true;
        }
    }
}

#[cfg(test)]
mod tests {
    //! Replays the emulator's inputs and compares against its RAM trajectory
    //! (tests/data/*.csv, from `psx run --watch` on the player object; see
    //! tools/mm8_trajectories.py). `VERBOSE=1 cargo test -- --nocapture` prints both.
    use super::*;
    use crate::player::PlayerState;

    fn spawn(app: &mut App) -> Entity {
        app.world_mut().spawn(Solid(Rect::new(0.0, 466.0, 4096.0, 512.0)));
        app.world_mut()
            .spawn((
                Player,
                StagePos(Vec2::new(416.0, 441.0)),
                Velocity::default(),
                PlayerState::Stand,
                Facing::Right,
                Grounded(true),
                WalkGrace::default(),
            ))
            .id()
    }

    fn parse_script(script: &str) -> Vec<(u32, u16)> {
        script
            .split_whitespace()
            .map(|tok| {
                let (f, b) = tok.split_once(':').unwrap();
                let bits = b.split('+').filter(|s| !s.is_empty()).fold(0u16, |acc, name| {
                    acc | match name {
                        "RIGHT" => Button::Right.mask(),
                        "LEFT" => Button::Left.mask(),
                        "CROSS" => Button::Cross.mask(),
                        "CIRCLE" => Button::Circle.mask(),
                        other => panic!("button {other}"),
                    }
                });
                (f.parse().unwrap(), bits)
            })
            .collect()
    }

    /// Emulator state byte → our state's number (for comparing the state sequence).
    fn state_id(s: PlayerState) -> u8 {
        match s {
            PlayerState::Stand => 0,
            PlayerState::Step { .. } => 1,
            PlayerState::Walk => 2,
            PlayerState::Jump { .. } => 3,
            PlayerState::Fall => 4,
            PlayerState::Land { .. } => 5,
        }
    }

    fn check(name: &str) -> f32 {
        let path = format!("{}/tests/data/{name}.csv", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        let script = text.lines().next().unwrap().trim_start_matches("# input:").trim().to_string();
        let events = parse_script(&script);
        let mut app = App::new();
        app.init_resource::<ControllerState>()
            .configure_sets(FixedUpdate, GameSystems::Game)
            .add_plugins(MovementPlugin)
            .add_plugins(crate::stage::CameraFollowPlugin);
        app.insert_resource(crate::stage::StageCamera { pos: Vec2::new(256.0, 256.0), min_x: 256.0 });
        let player = spawn(&mut app);
        let verbose = std::env::var("VERBOSE").is_ok();
        let (mut max_err, mut held) = (0.0f32, 0u16);
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let c: Vec<f32> = line.split(',').map(|v| v.parse().unwrap()).collect();
            let tick = c[0] as u32;
            if let Some((_, bits)) = events.iter().rev().find(|(f, _)| *f <= tick) {
                held = *bits;
            }
            app.world_mut().resource_mut::<ControllerState>().update(held, Vec2::ZERO, Vec2::ZERO);
            app.world_mut().run_schedule(FixedUpdate);
            let pos = app.world().get::<StagePos>(player).unwrap().0;
            let state = *app.world().get::<PlayerState>(player).unwrap();
            let cam = app.world().resource::<crate::stage::StageCamera>().pos.x;
            let err = (pos.x - c[1]).abs().max((pos.y - c[2]).abs());
            max_err = max_err.max(err);
            if verbose {
                println!(
                    "{name} t={tick:3} emu x={:8.4} y={:9.5} st={} cam={} | ours x={:8.4} y={:9.5} st={} cam={} | err {err:.6}",
                    c[1],
                    c[2],
                    c[5],
                    c[6],
                    pos.x,
                    pos.y,
                    state_id(state),
                    cam
                );
            }
            assert_eq!(state_id(state), c[5] as u8, "{name}: state differs at tick {tick}");
            assert_eq!(cam, c[6], "{name}: camera differs at tick {tick}");
        }
        println!(
            "{name}: max |error| {max_err} px over {} ticks",
            text.lines().filter(|l| !l.starts_with('#')).count()
        );
        max_err
    }

    #[test]
    fn matches_emulator_trajectories() {
        // `mixed` was recorded after the model was written and never used to fit it.
        for name in ["walk", "walk_regrab", "walk_left", "jump_hold", "jump_tap", "run_jump", "mixed"] {
            assert_eq!(check(name), 0.0, "{name}");
        }
    }

    #[test]
    fn full_jump_height_and_duration() {
        // Closed form: rising ticks n while JUMP_SPEED - n*GRAVITY > 0 → 23 ticks.
        let n = (JUMP_SPEED / GRAVITY).floor() as i32;
        let rise: f32 = (1..=n).map(|k| JUMP_SPEED - k as f32 * GRAVITY).sum();
        assert_eq!(n, 23);
        assert!((rise - 64.058_59).abs() < 1e-3, "{rise}");
    }
}

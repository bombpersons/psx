//! Replays the original's inputs and compares against its RAM trajectory
//! (tests/data/*.csv, from `psx run --watch` on the player object; see
//! tools/mm8_trajectories.py), one 50 Hz tick per `run_tick`.
//! `VERBOSE=1 cargo test -- --nocapture` prints both side by side.
use bevy::prelude::*;
use game::controller::Button;
use game::player::movement::{GRAVITY, Grounded, JUMP_SPEED, MovementPlugin, WalkGrace};
use game::player::{Facing, Player, PlayerState, Velocity};
use game::stage::{CameraFollowPlugin, Solid, StageCamera, StagePos};
use game::testing::{TickApp, headless_app};

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

/// `# input:` line: `tick:BUTTON+BUTTON ...`, each entry held from that tick on.
fn parse_script(script: &str) -> Vec<(u32, Vec<Button>)> {
    script
        .split_whitespace()
        .map(|tok| {
            let (f, b) = tok.split_once(':').unwrap();
            let buttons = b
                .split('+')
                .filter(|s| !s.is_empty())
                .map(|name| match name {
                    "RIGHT" => Button::Right,
                    "LEFT" => Button::Left,
                    "CROSS" => Button::Cross,
                    "CIRCLE" => Button::Circle,
                    other => panic!("button {other}"),
                })
                .collect();
            (f.parse().unwrap(), buttons)
        })
        .collect()
}

/// The original's state byte numbering, for comparing the state sequence.
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
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins((MovementPlugin, CameraFollowPlugin))
        .insert_resource(StageCamera { pos: Vec2::new(256.0, 256.0), min_x: 256.0 });
    let player = spawn(&mut app);
    let verbose = std::env::var("VERBOSE").is_ok();
    let mut max_err = 0.0f32;
    let mut held: &[Button] = &[];
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let c: Vec<f32> = line.split(',').map(|v| v.parse().unwrap()).collect();
        let tick = c[0] as u32;
        if let Some((_, buttons)) = events.iter().rev().find(|(f, _)| *f <= tick) {
            held = buttons;
        }
        app.run_tick(held);
        let pos = app.world().get::<StagePos>(player).unwrap().0;
        let state = *app.world().get::<PlayerState>(player).unwrap();
        let cam = app.world().resource::<StageCamera>().pos.x;
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
    println!("{name}: max |error| {max_err} px over {} ticks", text.lines().filter(|l| !l.starts_with('#')).count());
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

#[test]
fn camera_clamps_at_section_start() {
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(CameraFollowPlugin).insert_resource(StageCamera { pos: Vec2::new(256.0, 256.0), min_x: 256.0 });
    let p = app.world_mut().spawn((Player, StagePos(Vec2::new(394.0, 441.0)))).id();
    app.run_tick(&[]);
    assert_eq!(app.world().resource::<StageCamera>().pos.x, 256.0);
    app.world_mut().get_mut::<StagePos>(p).unwrap().0.x = 453.5;
    app.run_tick(&[]);
    assert_eq!(app.world().resource::<StageCamera>().pos.x, 293.0);
}

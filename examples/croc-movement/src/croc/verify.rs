//! Replays the inputs of trajectories recorded from the original game and
//! compares the demo's movement with them, tick by tick.
//!
//! Fixtures (`tests/fixtures/*.csv`) come from `tools/record_trajectory.py`:
//! Croc's position (0x800D8D8C/90/94) and heading (0x800D8D86) after every
//! game tick in level 1-1 of the PAL release, run on the psx native core from a
//! save state at the level start. Run `cargo test -- --nocapture` to see the
//! per-scenario errors.
use super::movement::{Croc, CrocMovementPlugin, Facing, Ground, UNITS_PER_BEVY, turns_4096};
use crate::controller::{Button, ControllerState};
use bevy::prelude::*;
use std::f32::consts::TAU;

const FIXTURES: &[(&str, &str)] = &[
    ("walk", include_str!("../../tests/fixtures/walk.csv")),
    ("turn", include_str!("../../tests/fixtures/turn.csv")),
    ("jump_tap", include_str!("../../tests/fixtures/jump_tap.csv")),
    ("jump_hold", include_str!("../../tests/fixtures/jump_hold.csv")),
    ("back_and_turn", include_str!("../../tests/fixtures/back_and_turn.csv")),
    ("air_strafe", include_str!("../../tests/fixtures/air_strafe.csv")),
    ("sidestep", include_str!("../../tests/fixtures/sidestep.csv")),
    ("combo", include_str!("../../tests/fixtures/combo.csv")),
];

/// The floor height at the level 1-1 start (object+0x38), in Croc units.
const LEVEL_START_FLOOR: f32 = -225.0;

struct Row {
    buttons: u16,
    /// Croc units, the original's axes.
    position: Vec3,
    /// 4096 units per turn.
    heading: f32,
}

fn parse(csv: &str) -> Vec<Row> {
    csv.lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("tick"))
        .map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            let buttons = f[1]
                .split('|')
                .filter(|b| !b.is_empty())
                .map(|b| match b {
                    "UP" => Button::Up,
                    "DOWN" => Button::Down,
                    "LEFT" => Button::Left,
                    "RIGHT" => Button::Right,
                    "CROSS" => Button::Cross,
                    "L1" => Button::L1,
                    "R1" => Button::R1,
                    other => panic!("unexpected button {other}"),
                })
                .fold(0, |bits, b| bits | b.mask());
            let n = |i: usize| f[i].parse::<f32>().unwrap();
            Row { buttons, position: Vec3::new(n(2), n(3), n(4)), heading: n(5) }
        })
        .collect()
}

/// The original's axes (+Z forward at heading 0) to Bevy's (-Z forward).
fn to_bevy(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, -p.z) / UNITS_PER_BEVY
}

fn to_original(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, -p.z) * UNITS_PER_BEVY
}

struct Errors {
    ticks: usize,
    max_position: f32,
    max_heading: f32,
    final_position: f32,
    path_length: f32,
    /// Ticks where exactly one of the two is standing on the floor.
    grounded_mismatches: usize,
}

fn replay(rows: &[Row], mut print: impl FnMut(usize, &Row, Vec3, f32)) -> Errors {
    let mut app = App::new();
    app.init_resource::<ControllerState>().add_plugins(CrocMovementPlugin);
    let start = &rows[0];
    let croc = app
        .world_mut()
        .spawn((
            Croc,
            Transform::from_translation(to_bevy(start.position)),
            Facing { yaw: turns_4096(start.heading), turn_rate: 0.0 },
            Ground { height: LEVEL_START_FLOOR / UNITS_PER_BEVY },
        ))
        .id();
    let mut errors = Errors {
        ticks: 0,
        max_position: 0.0,
        max_heading: 0.0,
        final_position: 0.0,
        path_length: 0.0,
        grounded_mismatches: 0,
    };
    for (tick, pair) in rows.windows(2).enumerate() {
        let row = &pair[1];
        errors.path_length += (row.position - pair[0].position).length();
        app.world_mut().resource_mut::<ControllerState>().update(row.buttons, Vec2::ZERO, Vec2::ZERO);
        app.world_mut().run_schedule(FixedUpdate);
        let position = to_original(app.world().get::<Transform>(croc).unwrap().translation);
        let heading = app.world().get::<Facing>(croc).unwrap().yaw / TAU * 4096.0;
        let position_error = (position - row.position).length();
        let heading_error = ((heading - row.heading + 2048.0).rem_euclid(4096.0) - 2048.0).abs();
        errors.max_position = errors.max_position.max(position_error);
        errors.max_heading = errors.max_heading.max(heading_error);
        errors.final_position = position_error;
        let on_floor = |y: f32| (y - LEVEL_START_FLOOR).abs() < 0.5;
        if on_floor(position.y) != on_floor(row.position.y) {
            errors.grounded_mismatches += 1;
        }
        errors.ticks += 1;
        print(tick + 1, row, position, heading);
    }
    errors
}

#[test]
fn matches_the_original_trajectories() {
    println!(
        "scenario        ticks  path(u)  max pos err(u)  final pos err(u)  max heading err(1/4096)  floor mismatches"
    );
    for (name, csv) in FIXTURES {
        let rows = parse(csv);
        let e = replay(&rows, |_, _, _, _| {});
        println!(
            "{name:<14} {:>6} {:>8.0} {:>15.1} {:>17.1} {:>24.2} {:>17}",
            e.ticks, e.path_length, e.max_position, e.final_position, e.max_heading, e.grounded_mismatches
        );
        // The original works in whole units: it drops the fraction of each
        // tick's step (about 0.5 unit/tick at full speed), rounds speed
        // changes down (falls accelerate slightly slower) and turns in whole
        // 1/4096 steps. The demo keeps fractions, so it drifts by a few units
        // per second. Bounds: under half a tick of running (140 units) and
        // half a degree, and take-off and landing on exactly the same ticks.
        assert_eq!(e.grounded_mismatches, 0, "{name}: leaves or reaches the floor on a different tick");
        assert!(e.max_heading < 6.0, "{name}: heading off by {} / 4096", e.max_heading);
        assert!(e.max_position < 64.0, "{name}: position off by {} units", e.max_position);
    }
}

/// Prints every scenario side by side: `cargo test print_trajectories -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_trajectories() {
    for (name, csv) in FIXTURES {
        println!("== {name}\ntick  original (x, y, z, heading)        demo (x, y, z, heading)");
        replay(&parse(csv), |tick, row, position, heading| {
            println!(
                "{tick:>4}  {:>6.0} {:>6.0} {:>6.0} {:>5.0}    {:>8.1} {:>8.1} {:>8.1} {:>7.1}",
                row.position.x,
                row.position.y,
                row.position.z,
                row.heading,
                position.x,
                position.y,
                position.z,
                heading
            );
        });
    }
}

//! The intro stage: stage space, collision, the scrolling camera and a backdrop.
//!
//! Stage space is the original's level coordinate system: pixels, x right,
//! y down. Bevy's 2D world is the same with y flipped, so a stage position
//! (x, y) is drawn at (x, -y) and the camera looks at the 320×240 window whose
//! top-left is `StageCamera::pos`.
//!
//! Collision here is a single hand-placed floor matching what the player
//! stands on in the capture. A real rebuild loads the stage's own collision
//! and tile data instead (research/case-study-megaman8.md, "Rebuilding the stage").
use crate::{
    GameSystems,
    player::{Player, movement::PlayerMovement},
    psx::{PsxSceneElement, PsxSceneRoot, PsxView},
};
use bevy::prelude::*;

/// Position in stage pixels (y down). Rendering rounds down to whole pixels,
/// like the original, which draws at the integer part of its 16.16 position.
#[derive(Component, Default, Debug, Clone, Copy)]
pub struct StagePos(pub Vec2);

/// A solid rectangle in stage pixels (`min.y` is its top row).
#[derive(Component, Debug, Clone, Copy)]
pub struct Solid(pub Rect);

/// The visible window's top-left in stage pixels.
#[derive(Resource, Debug, Clone, Copy)]
pub struct StageCamera {
    pub pos: Vec2,
    /// Left scroll limit of this section.
    pub min_x: f32,
}

/// Screen column the camera keeps the player on (cam_x = x - 160; the camera
/// x at 0x8016F26C, copied from 0x801D2FC6 by 0x80101218, follows that rule
/// and stops at 256 on the left in the measured runs).
const FOLLOW_X: f32 = 160.0;
/// The floor under the start position: the first solid row below the feet.
const FLOOR_TOP: f32 = 466.0;
/// The captured frame's camera (0x8016F26C / 0x8016F270 at the save state).
const CAPTURE_CAMERA: Vec2 = Vec2::new(256.0, 256.0);
/// GPU commands that drew Mega Man in the capture (gpu_frame.json 668–673),
/// hidden so the live player replaces him.
const CAPTURED_PLAYER: std::ops::RangeInclusive<u32> = 668..=673;
const BACKDROP: &str = "captures/mm8/scene.psxscene";

pub struct CameraFollowPlugin;

impl Plugin for CameraFollowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(FixedUpdate, follow_player.after(PlayerMovement).in_set(GameSystems::Game));
    }
}

pub struct StagePlugin;

impl Plugin for StagePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(StageCamera { pos: CAPTURE_CAMERA, min_x: 256.0 })
            .insert_resource(ClearColor(Color::srgb_u8(96, 160, 232)))
            .add_plugins(CameraFollowPlugin)
            .add_systems(Startup, setup)
            .add_systems(Update, hide_captured_player)
            .add_systems(PostUpdate, (place_stage_entities, place_camera).before(TransformSystems::Propagate));
    }
}

/// True when the user has put their own extracted file under assets/.
pub fn user_asset(path: &str) -> bool {
    let root = std::env::var("BEVY_ASSET_ROOT")
        .or_else(|_| std::env::var("CARGO_MANIFEST_DIR"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    root.join("assets").join(path).exists()
}

fn setup(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn((Camera2d, PsxView, Transform::default()));
    commands.spawn((Name::new("Floor"), Solid(Rect::new(0.0, FLOOR_TOP, 4096.0, 512.0))));

    if user_asset(BACKDROP) {
        // Temporary backdrop: the captured frame, pinned where it was on screen.
        // It is reference scaffolding, not the level; it covers one screen.
        let centre = CAPTURE_CAMERA + Vec2::new(160.0, 120.0);
        commands.spawn((
            PsxSceneRoot(assets.load(BACKDROP)),
            Transform::from_xyz(centre.x, -centre.y, -10.0),
            Visibility::default(),
        ));
    } else {
        // Placeholder scenery: grass strip and dirt below the floor.
        let width = 4096.0;
        for (top, height, color) in [(FLOOR_TOP, 6.0, Color::srgb_u8(96, 200, 64)), (FLOOR_TOP + 6.0, 64.0, Color::srgb_u8(200, 176, 120))] {
            commands.spawn((
                Sprite::from_color(color, Vec2::new(width, height)),
                Transform::from_xyz(width / 2.0, -(top + height / 2.0), -5.0),
            ));
        }
        for x in (0..4096).step_by(64) {
            commands.spawn((
                Sprite::from_color(Color::srgb_u8(64, 150, 64), Vec2::new(32.0, 24.0)),
                Transform::from_xyz(x as f32, -(FLOOR_TOP - 12.0), -6.0),
            ));
        }
    }
}

fn hide_captured_player(mut elements: Query<(&PsxSceneElement, &mut Visibility), Added<PsxSceneElement>>) {
    for (element, mut visibility) in &mut elements {
        if CAPTURED_PLAYER.contains(&element.command) {
            *visibility = Visibility::Hidden;
        }
    }
}

/// Scroll: keep the player on column 160, never left of the section start.
fn follow_player(mut camera: ResMut<StageCamera>, players: Query<&StagePos, With<Player>>) {
    if let Ok(pos) = players.single() {
        camera.pos.x = (pos.0.x.floor() - FOLLOW_X).max(camera.min_x);
    }
}

fn place_stage_entities(mut q: Query<(&StagePos, &mut Transform)>) {
    for (pos, mut t) in &mut q {
        t.translation.x = pos.0.x.floor();
        t.translation.y = -pos.0.y.floor();
        t.translation.z = 1.0;
    }
}

fn place_camera(camera: Res<StageCamera>, mut cams: Query<&mut Transform, (With<Camera2d>, With<PsxView>)>) {
    for mut t in &mut cams {
        t.translation = Vec3::new(camera.pos.x + 160.0, -(camera.pos.y + 120.0), t.translation.z);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_clamps_at_section_start() {
        let mut app = App::new();
        app.configure_sets(FixedUpdate, GameSystems::Game).add_plugins(CameraFollowPlugin);
        app.insert_resource(StageCamera { pos: CAPTURE_CAMERA, min_x: 256.0 });
        let p = app.world_mut().spawn((Player, StagePos(Vec2::new(394.0, 441.0)))).id();
        app.world_mut().run_schedule(FixedUpdate);
        assert_eq!(app.world().resource::<StageCamera>().pos.x, 256.0);
        app.world_mut().get_mut::<StagePos>(p).unwrap().0.x = 453.5;
        app.world_mut().run_schedule(FixedUpdate);
        assert_eq!(app.world().resource::<StageCamera>().pos.x, 293.0);
    }
}

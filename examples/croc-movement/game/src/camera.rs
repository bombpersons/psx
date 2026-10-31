//! A stand-in camera: it follows a target from behind and above until the
//! original's camera is ported.
use bevy::prelude::*;

/// The camera follows the entity with this.
#[derive(Component, Default)]
pub struct CameraTarget;

/// A camera that follows the [`CameraTarget`], metres.
#[derive(Component, Clone, Copy, Debug)]
pub struct FollowCamera {
    /// How far behind the target.
    pub distance: f32,
    /// How far above the target's feet.
    pub height: f32,
    /// The height on the target it looks at.
    pub look_height: f32,
}

impl Default for FollowCamera {
    fn default() -> Self {
        FollowCamera { distance: 4.0, height: 2.0, look_height: 0.8 }
    }
}

/// Moves follow cameras after their targets have been placed.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraFollow;

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, follow.in_set(CameraFollow));
    }
}

fn follow(
    target: Query<&Transform, With<CameraTarget>>,
    mut cameras: Query<(&FollowCamera, &mut Transform), Without<CameraTarget>>,
) {
    let Ok(target) = target.single() else { return };
    for (rig, mut camera) in &mut cameras {
        let behind = target.rotation * Vec3::new(0.0, 0.0, -rig.distance);
        camera.translation = target.translation + behind + Vec3::Y * rig.height;
        camera.look_at(target.translation + Vec3::Y * rig.look_height, Vec3::Y);
    }
}

//! `--scene PATH`: shows a psx-toolkit frame capture (`.psxscene`) on its own.
//! A development tool for comparing against the original, not game code.
use crate::psx::{PsxSceneCamera, PsxSceneDepth, PsxSceneRoot, PsxView, SCENE_BACKGROUND_LAYER};
use bevy::{camera::visibility::RenderLayers, prelude::*};

pub struct SceneViewerPlugin {
    /// Path under `assets/`.
    pub path: String,
    pub depth: PsxSceneDepth,
}

impl Plugin for SceneViewerPlugin {
    fn build(&self, app: &mut App) {
        let (path, depth) = (self.path.clone(), self.depth);
        app.add_systems(Startup, move |mut commands: Commands, assets: Res<AssetServer>| {
            // Background 2D (drawn before any 3D), lifted 3D geometry, then 2D on top (HUD).
            commands.spawn((
                Camera2d,
                PsxView,
                Camera { order: -1, ..default() },
                RenderLayers::layer(SCENE_BACKGROUND_LAYER),
            ));
            commands.spawn((
                Camera3d::default(),
                PsxView,
                PsxSceneCamera,
                Camera { order: 0, clear_color: ClearColorConfig::None, ..default() },
            ));
            commands.spawn((Camera2d, PsxView, Camera { order: 1, clear_color: ClearColorConfig::None, ..default() }));
            commands.spawn((
                PsxSceneRoot(assets.load(path.clone())),
                depth,
                Transform::default(),
                Visibility::default(),
            ));
        })
        .add_systems(Update, toggle_scene_depth);
    }
}

/// P switches between depth-tested and painter's-order 3D.
fn toggle_scene_depth(keys: Res<ButtonInput<KeyCode>>, mut roots: Query<&mut PsxSceneDepth>) {
    if !keys.just_pressed(KeyCode::KeyP) {
        return;
    }
    for mut depth in &mut roots {
        *depth = match *depth {
            PsxSceneDepth::Tested => PsxSceneDepth::Painter,
            PsxSceneDepth::Painter => PsxSceneDepth::Tested,
        };
    }
}

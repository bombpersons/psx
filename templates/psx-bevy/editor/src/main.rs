//! The editor executable: `cargo run -p editor -- [file.ron]`.
use bevy::prelude::*;
use bevy::window::WindowResolution;
use editor::EditorPlugin;

fn main() {
    let open = std::env::args_os().nth(1).map(Into::into);
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Editor".into(),
                resolution: WindowResolution::new(1440, 900),
                ..default()
            }),
            // Closing asks about unsaved changes first (`EditorPlugin`).
            close_when_requested: false,
            ..default()
        }))
        .add_plugins(EditorPlugin { open })
        .run();
}

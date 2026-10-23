//! `PsxScreen::set_size` at runtime: a green 3D quad (half the view's height)
//! behind a 20×20 red sprite at the centre. Whatever the size, the 3D view
//! keeps its vertical field of view and widens (Hor+), and 2D stays one unit
//! per pixel.
//! cargo run --example screen_size, then F2 for the display menu.
//! For screenshot tests: cargo build --features dev-mcp --example screen_size
//! then run with --mcp --headless --software. Edit the reflected
//! `ScreenControls` component through game_set_component while paused.
//! The 3D camera's projection is touched on every resize, as a game fitting
//! its view to the screen would, which must not desync the shared depth.
use bevy::prelude::*;
use psx_bevy::{
    launcher::{self, Game},
    psx::{PsxLook, PsxMaterial, PsxScreen, PsxView, Region},
};

#[derive(Component, Reflect)]
#[reflect(Component)]
struct ScreenControls {
    width: u32,
    height: u32,
}

fn main() {
    // Flat colours compared exactly: snapping, dither and 15-bit output off.
    launcher::run(Game {
        name: "psx-screen-size",
        title: "Screen size",
        region: Region::Ntsc,
        fields_per_tick: 1,
        screen: UVec2::new(320, 240),
        add: |app| {
            app.insert_resource(PsxLook { vertex_snap: false, dither: false, quantize: false })
                .add_systems(Startup, setup)
                .add_systems(Update, (sync_controls, fit_view).chain());
        },
    });
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<PsxMaterial>>) {
    commands.spawn((Name::new("ScreenControls"), ScreenControls { width: 320, height: 240 }));
    commands.spawn((
        Camera3d::default(),
        PsxView,
        Camera { clear_color: Color::BLACK.into(), ..default() },
        Projection::Perspective(PerspectiveProjection { fov: std::f32::consts::FRAC_PI_2, ..default() }),
        Transform::default(),
    ));
    commands.spawn((Camera2d, PsxView, Camera { order: 1, clear_color: ClearColorConfig::None, ..default() }));
    // At depth 1 with a 90° vertical fov the view is 2 units high: 1 unit is half of it.
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(1.0, 1.0))),
        MeshMaterial3d(materials.add(PsxMaterial { color: Color::srgb(0.0, 1.0, 0.0), lit: false, ..default() })),
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));
    commands.spawn(Sprite::from_color(Color::srgb(1.0, 0.0, 0.0), Vec2::splat(20.0)));
}

fn sync_controls(controls: Query<&ScreenControls, Changed<ScreenControls>>, mut screen: ResMut<PsxScreen>) {
    for control in &controls {
        screen.set_size(UVec2::new(control.width, control.height));
    }
}

/// Same field of view, but a projection change on the resize frame.
fn fit_view(screen: Res<PsxScreen>, mut projections: Query<&mut Projection, With<Camera3d>>) {
    if screen.is_changed() {
        for mut projection in &mut projections {
            projection.set_changed();
        }
    }
}

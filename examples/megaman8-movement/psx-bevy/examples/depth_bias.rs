//! `PsxMaterial::depth_bias` and a non-default screen size: a red and a green
//! copy of the same slanted wall, from depth 2 (left) to 20 (right), on a
//! 368x240 screen. The copy with `PsxDepthBias::BEHIND` must lose everywhere.
//! cargo run --example depth_bias
//! For screenshot tests: cargo build --features dev-mcp --example depth_bias
//! then run with --mcp --headless --software. Edit the reflected
//! `DepthBiasControls` component through game_set_component while paused.
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
};
use psx_bevy::{
    launcher::{self, Game},
    psx::{PsxDepthBias, PsxLook, PsxMaterial, PsxView, Region},
};

#[derive(Component, Reflect)]
#[reflect(Component)]
struct DepthBiasControls {
    /// Bias the red copy behind; otherwise the green one.
    red_behind: bool,
}

#[derive(Resource)]
struct Copies {
    red: Handle<PsxMaterial>,
    green: Handle<PsxMaterial>,
}

fn main() {
    // Flat colours compared exactly: snapping, dither and 15-bit output off.
    launcher::run(Game {
        name: "psx-depth-bias",
        title: "Depth bias",
        region: Region::Ntsc,
        fields_per_tick: 1,
        screen: UVec2::new(368, 240),
        add: |app| {
            app.insert_resource(PsxLook { vertex_snap: false, dither: false, quantize: false })
                .add_systems(Startup, setup)
                .add_systems(Update, sync_controls);
        },
    });
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<PsxMaterial>>) {
    commands.spawn((Name::new("DepthBiasControls"), DepthBiasControls { red_behind: true }));
    commands.spawn((
        Name::new("Camera"),
        Camera3d::default(),
        PsxView,
        Camera { clear_color: Color::BLACK.into(), ..default() },
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_2,
            near: 0.1,
            far: 100.0,
            ..default()
        }),
        Transform::default(),
    ));
    // Corners at ±0.9 NDC; x scaled by depth and the 368:240 aspect.
    let aspect = 368.0 / 240.0;
    let corner = |x: f32, y: f32, depth: f32| [x * depth * aspect, y * depth, -depth];
    let positions =
        vec![corner(-0.9, 0.9, 2.0), corner(0.9, 0.9, 20.0), corner(-0.9, -0.9, 2.0), corner(0.9, -0.9, 20.0)];
    let mesh = meshes.add(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_indices(Indices::U32(vec![0, 2, 1, 1, 2, 3])),
    );
    let copy = |color| PsxMaterial { color, lit: false, double_sided: true, ..default() };
    let copies = Copies {
        red: materials.add(copy(Color::srgb(1.0, 0.0, 0.0))),
        green: materials.add(copy(Color::srgb(0.0, 1.0, 0.0))),
    };
    for material in [&copies.red, &copies.green] {
        commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
    }
    commands.insert_resource(copies);
}

fn sync_controls(
    controls: Query<&DepthBiasControls, Changed<DepthBiasControls>>,
    copies: Res<Copies>,
    mut materials: ResMut<Assets<PsxMaterial>>,
) {
    for control in &controls {
        let (behind, front) =
            if control.red_behind { (&copies.red, &copies.green) } else { (&copies.green, &copies.red) };
        materials.get_mut(behind).unwrap().depth_bias = PsxDepthBias::BEHIND;
        materials.get_mut(front).unwrap().depth_bias = PsxDepthBias::NONE;
    }
}

//! A narrow corridor under bright fog. The floor is one long quad that starts
//! behind the camera, so its triangles cross the near plane: their pixels
//! beside the camera must stay unfogged.
//! cargo run --example corridor
//! For screenshot tests: cargo build --features dev-mcp --example corridor
//! then run with --mcp --headless --software.
use bevy::{
    asset::RenderAssetUsages,
    mesh::{Indices, PrimitiveTopology},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
};
use psx_bevy::{
    launcher::{self, Game},
    psx::{PsxLook, PsxMaterial, PsxView, Region, rgba_image},
};

const FOG: Color = Color::srgb(1.0, 0.0, 1.0);

fn main() {
    // Flat colours compared exactly: snapping, dither and 15-bit output off.
    launcher::run(Game {
        name: "psx-corridor",
        title: "Corridor",
        region: Region::Ntsc,
        fields_per_tick: 1,
        screen: UVec2::new(320, 240),
        add: |app| {
            app.insert_resource(PsxLook { vertex_snap: false, dither: false, quantize: false })
                .add_systems(Startup, setup);
        },
    });
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<PsxMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    commands.spawn((
        Name::new("Camera"),
        Camera3d::default(),
        PsxView,
        Camera { clear_color: FOG.into(), ..default() },
        DistanceFog { color: FOG, falloff: FogFalloff::Linear { start: 4.0, end: 30.0 }, ..default() },
        Transform::from_xyz(0.0, 1.0, 0.0).looking_at(Vec3::new(0.0, 0.8, -10.0), Vec3::Y),
    ));
    let mut rgba = Vec::new();
    for y in 0..8 {
        for x in 0..8 {
            let c: [u8; 4] = if (x / 4 + y / 4) % 2 == 0 { [90, 90, 110, 255] } else { [60, 60, 70, 255] };
            rgba.extend_from_slice(&c);
        }
    }
    let texture = images.add(rgba_image(8, 8, rgba));
    let material = materials.add(PsxMaterial { lit: false, double_sided: true, ..PsxMaterial::textured(texture) });

    // Walls and ceiling in 1x1 quads from z = 4 to z = -40; the floor whole.
    let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    let mut quad = |a: Vec3, u: Vec3, v: Vec3| {
        let base = positions.len() as u32;
        for (p, uv) in [(a, [0.0, 0.0]), (a + u, [1.0, 0.0]), (a + v, [0.0, 1.0]), (a + u + v, [1.0, 1.0])] {
            positions.push(p.to_array());
            uvs.push(uv);
        }
        indices.extend([base, base + 2, base + 1, base + 1, base + 2, base + 3]);
    };
    for z in -40..4 {
        let z = z as f32;
        for i in [0.0, 1.0] {
            quad(Vec3::new(-1.0 + i, 2.0, z), Vec3::X, Vec3::Z);
            quad(Vec3::new(-1.0, i, z), Vec3::Y, Vec3::Z);
            quad(Vec3::new(1.0, i, z), Vec3::Y, Vec3::Z);
        }
    }
    quad(Vec3::new(-1.0, 0.0, 4.0), Vec3::X * 2.0, Vec3::Z * -44.0);
    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_indices(Indices::U32(indices));
    commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material)));
}

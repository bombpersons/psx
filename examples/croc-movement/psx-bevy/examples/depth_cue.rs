//! Authored depth-cue comparison: three equal-size panels at depths 20, 40,
//! 100, and a large wall sloping from depth 20 (left) to 100 (right).
//! cargo run --example depth_cue
//! For screenshot tests: cargo build --features dev-mcp --example depth_cue
//! then run with --mcp --headless --software. Edit the reflected
//! `DepthCueControls` component through game_set_component while paused.
use bevy::{
    asset::RenderAssetUsages,
    camera::ScalingMode,
    mesh::{Indices, PrimitiveTopology},
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
};
use psx_bevy::{
    launcher::{self, Game},
    psx::{PsxDepthCue, PsxLook, PsxMaterial, PsxView, Region, rgba_image},
};

#[derive(Component, Reflect)]
#[reflect(Component)]
struct DepthCueControls {
    enabled: bool,
    bias: f32,
    scale: f32,
    far_color: Vec3,
    surface_color: Vec3,
    fog: bool,
    camera_fog: bool,
    orthographic: bool,
    textured: bool,
}

#[derive(Component)]
struct Panel {
    ndc: [[f32; 2]; 4],
    depths: [f32; 4],
}

impl Panel {
    fn positions(&self, orthographic: bool) -> Vec<[f32; 3]> {
        self.ndc
            .iter()
            .zip(self.depths)
            .map(|([x, y], depth)| {
                let size = if orthographic { 1.0 } else { depth };
                [x * size * 4.0 / 3.0, y * size, -depth]
            })
            .collect()
    }
}

#[derive(Resource)]
struct ProbeAssets {
    material: Handle<PsxMaterial>,
    texture: Handle<Image>,
}

fn main() {
    // The authored scene is lit flat and compared pixel for pixel, so the
    // snapping, dither and 15-bit effects are off.
    launcher::run(Game {
        name: "psx-depth-cue",
        title: "Reciprocal depth cue",
        region: Region::Ntsc,
        fields_per_tick: 1,
        screen: UVec2::new(320, 240),
        add: |app| {
            app.insert_resource(PsxLook { vertex_snap: false, dither: false, quantize: false })
                .add_systems(Startup, setup)
                .add_systems(Update, sync_controls);
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
        Name::new("DepthCueControls"),
        DepthCueControls {
            enabled: true,
            bias: 1.25,
            scale: -25.0,
            far_color: Vec3::ZERO,
            surface_color: Vec3::ONE,
            fog: true,
            camera_fog: false,
            orthographic: false,
            textured: false,
        },
    ));
    commands.spawn((
        Name::new("Camera"),
        Camera3d::default(),
        PsxView,
        Camera { clear_color: Color::srgb(0.05, 0.05, 0.15).into(), ..default() },
        Projection::Perspective(PerspectiveProjection {
            fov: std::f32::consts::FRAC_PI_2,
            near: 0.1,
            far: 200.0,
            ..default()
        }),
        Transform::default(),
    ));
    let material = materials.add(PsxMaterial { lit: false, double_sided: true, ..default() });
    let texture = images.add(rgba_image(1, 1, vec![128, 128, 128, 255]));
    for (center, depth) in [(-0.62, 20.0), (0.0, 40.0), (0.62, 100.0)] {
        spawn_panel(
            &mut commands,
            &mut meshes,
            &material,
            Panel {
                ndc: [[center - 0.25, 0.85], [center + 0.25, 0.85], [center - 0.25, 0.15], [center + 0.25, 0.15]],
                depths: [depth; 4],
            },
        );
    }
    spawn_panel(
        &mut commands,
        &mut meshes,
        &material,
        Panel {
            ndc: [[-0.87, -0.15], [0.87, -0.15], [-0.87, -0.85], [0.87, -0.85]],
            depths: [20.0, 100.0, 20.0, 100.0],
        },
    );
    commands.insert_resource(ProbeAssets { material, texture });
}

fn spawn_panel(commands: &mut Commands, meshes: &mut Assets<Mesh>, material: &Handle<PsxMaterial>, panel: Panel) {
    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, panel.positions(false))
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; 4])
        .with_inserted_indices(Indices::U32(vec![0, 2, 1, 1, 2, 3]));
    commands.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone()), Transform::default(), panel));
}

fn sync_controls(
    controls: Query<&DepthCueControls, Changed<DepthCueControls>>,
    assets: Res<ProbeAssets>,
    mut materials: ResMut<Assets<PsxMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    panels: Query<(&Panel, &Mesh3d)>,
    mut cameras: Query<(Entity, &mut Projection), With<Camera3d>>,
    mut commands: Commands,
) {
    for control in &controls {
        let mut material = materials.get_mut(&assets.material).unwrap();
        let color = |v: Vec3| Color::srgb(v.x, v.y, v.z);
        material.color = color(control.surface_color);
        material.fog = control.fog;
        material.depth_cue = control.enabled.then_some(PsxDepthCue {
            bias: control.bias,
            scale: control.scale,
            far_color: color(control.far_color),
        });
        material.texture = control.textured.then(|| assets.texture.clone());
        for (entity, mut projection) in &mut cameras {
            *projection = if control.orthographic {
                Projection::Orthographic(OrthographicProjection {
                    scaling_mode: ScalingMode::FixedVertical { viewport_height: 2.0 },
                    near: 0.1,
                    far: 200.0,
                    ..OrthographicProjection::default_3d()
                })
            } else {
                Projection::Perspective(PerspectiveProjection {
                    fov: std::f32::consts::FRAC_PI_2,
                    near: 0.1,
                    far: 200.0,
                    ..default()
                })
            };
            if control.camera_fog {
                commands.entity(entity).insert(DistanceFog {
                    color: Color::BLACK,
                    falloff: FogFalloff::Linear { start: 0.0, end: 1.0 },
                    ..default()
                });
            } else {
                commands.entity(entity).remove::<DistanceFog>();
            }
        }
        for (panel, mesh) in &panels {
            meshes
                .get_mut(&mesh.0)
                .unwrap()
                .insert_attribute(Mesh::ATTRIBUTE_POSITION, panel.positions(control.orthographic));
        }
    }
}

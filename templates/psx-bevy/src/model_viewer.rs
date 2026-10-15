//! `--model PATH.tmd [--texture PATH.tim]... [--cull]`: look at an extracted
//! original model (`--cull`: [`TmdSettings::cull_back_faces`]). The camera frames it once loaded; the model turns in `FixedUpdate`
//! (so `game_step` turns it too), L1/R1 turn it by hand, Triangle toggles the
//! PS1 effects.
use bevy::{
    pbr::{DistanceFog, FogFalloff},
    prelude::*,
};

use crate::{
    controller::{Button, ControllerState},
    psx::{PsxLook, PsxModel, PsxModelRoot, PsxView, TmdSettings},
    shell::GameSystems,
};

pub struct ModelViewerPlugin {
    pub path: String,
    /// TIMs to upload to the virtual VRAM (empty: `<model>.tim` if present).
    pub textures: Vec<String>,
    pub cull: bool,
}

#[derive(Resource, Clone)]
struct ViewerConfig {
    path: String,
    textures: Vec<String>,
    cull: bool,
}

#[derive(Component)]
struct Turntable;

#[derive(Component)]
struct Framed;

const BACKGROUND: Color = Color::srgb(0.12, 0.14, 0.2);

impl Plugin for ModelViewerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ViewerConfig { path: self.path.clone(), textures: self.textures.clone(), cull: self.cull })
            .add_systems(Startup, setup)
            .add_systems(FixedUpdate, (turn, toggle_look).in_set(GameSystems::Game))
            .add_systems(Update, frame_camera);
    }
}

fn setup(mut commands: Commands, assets: Res<AssetServer>, config: Res<ViewerConfig>) {
    commands.spawn((
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::Custom(BACKGROUND), ..default() },
        PsxView,
        Transform::from_xyz(0.0, 1.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight { illuminance: 800.0, ..default() },
        Transform::from_xyz(2.0, 4.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 450.0, ..default() });
    let (textures, cull) = (config.textures.clone(), config.cull);
    let model: Handle<PsxModel> = assets
        .load_builder()
        .with_settings(move |s: &mut TmdSettings| {
            s.textures = textures.clone();
            s.cull_back_faces = cull;
        })
        .load(config.path.clone());
    commands.spawn((PsxModelRoot(model), Transform::default(), Visibility::default(), Turntable));
}

/// Once the model has loaded, aim the camera at its bounds and set fog behind it.
fn frame_camera(
    mut commands: Commands,
    roots: Query<&PsxModelRoot, With<Turntable>>,
    models: Res<Assets<PsxModel>>,
    camera: Query<Entity, (With<Camera3d>, Without<Framed>)>,
) {
    let (Ok(root), Ok(camera)) = (roots.single(), camera.single()) else { return };
    let Some(model) = models.get(&root.0) else { return };
    let [min, max] = model.bounds;
    let centre = (min + max) / 2.0;
    let radius = ((max - min).length() / 2.0).max(0.1);
    let eye = centre + Vec3::new(0.0, 0.35, 1.0).normalize() * radius * 2.6;
    commands.entity(camera).insert((
        Transform::from_translation(eye).looking_at(centre, Vec3::Y),
        DistanceFog {
            color: BACKGROUND,
            falloff: FogFalloff::Linear { start: radius * 3.0, end: radius * 6.0 },
            ..default()
        },
        Framed,
    ));
    info!("model: {} objects, bounds {min} .. {max}, {} untextured polygons", model.objects.len(), model.untextured);
}

fn turn(pad: Res<ControllerState>, mut q: Query<&mut Transform, With<Turntable>>) {
    let manual = f32::from(u8::from(pad.held(Button::L1))) - f32::from(u8::from(pad.held(Button::R1)));
    for mut t in &mut q {
        t.rotate_y(if manual != 0.0 { 0.04 * manual } else { 0.01 });
    }
}

fn toggle_look(pad: Res<ControllerState>, mut look: ResMut<PsxLook>) {
    if pad.pressed(Button::Triangle) {
        let on = !look.vertex_snap;
        look.vertex_snap = on;
        look.dither = on;
    }
}

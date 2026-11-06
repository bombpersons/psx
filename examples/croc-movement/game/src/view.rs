//! What the player sees: the floor, Croc as a stand-in shape until his
//! model is imported, and a camera that follows him from behind until the
//! original's camera is ported. It draws [`game::Game`]'s state; no rules
//! here.
use game::Game;
use game::character::Character;
use game::world::METRES_PER_UNIT;
use renderer::{
    Alpha, Camera, Fog, Font, Instance, Light, Mat4, Mesh, ModelId, Primitive, Renderer, Scene, Vec3, Vertex,
};

const SKY: [f32; 3] = [0.45, 0.65, 0.9];

/// The stand-in floor's size, metres: a flat floor has no edge, so it is
/// drawn this wide, centred on the world's middle (x, z 12288 units).
const FLOOR_SIZE: f32 = 400.0;
const FLOOR_TILES: i32 = 40;
const WORLD_MIDDLE: f32 = 12288.0 * METRES_PER_UNIT;

/// The follow camera, metres: how far behind Croc, how far above his feet,
/// and the height on him it looks at.
const CAMERA_DISTANCE: f32 = 4.0;
const CAMERA_HEIGHT: f32 = 2.0;
const CAMERA_LOOK_HEIGHT: f32 = 0.8;

pub struct View {
    floor: ModelId,
    body: ModelId,
}

impl View {
    pub fn new(renderer: &mut Renderer) -> Self {
        // Tiles, as the PS1 maps textures affinely and games cut large
        // polygons up; untextured here, but the same mesh takes a texture.
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let tile = FLOOR_SIZE / FLOOR_TILES as f32;
        for tz in 0..FLOOR_TILES {
            for tx in 0..FLOOR_TILES {
                let base = vertices.len() as u32;
                for (dx, dz) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                    vertices.push(Vertex {
                        position: [
                            (tx + dx) as f32 * tile - FLOOR_SIZE / 2.0,
                            0.0,
                            (tz + dz) as f32 * tile - FLOOR_SIZE / 2.0,
                        ],
                        normal: [0.0, 1.0, 0.0],
                        uv: [dx as f32, dz as f32],
                        color: [0.3, 0.55, 0.25, 1.0],
                    });
                }
                indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
        let floor = renderer.model(&Mesh {
            primitives: vec![Primitive { vertices, indices, texture: None, alpha: Alpha::Opaque, lit: true }],
        });
        let body = renderer.model(&renderer::cube(1.0, [[0.2, 0.7, 0.25]; 6]));
        Self { floor, body }
    }

    pub fn scene(&self, game: &Game, font: Option<&Font>) -> Scene {
        let mut scene = Scene { clear: SKY, ambient: [0.6; 3], ..Default::default() };
        scene.lights.push(Light { towards: Vec3::new(2.0, 4.0, 3.0), color: [0.6; 3] });
        scene.fog = Some(Fog { color: SKY, near: 20.0, far: 60.0 });
        for floor in &game.world.floors {
            let y = floor.height as f32 * METRES_PER_UNIT;
            let place = Mat4::from_translation(Vec3::new(WORLD_MIDDLE, y, WORLD_MIDDLE));
            scene.models.push(Instance::new(self.floor, place));
        }
        if let Some(croc) = &game.world.croc {
            let place = placement(&croc.body);
            // A body and a snout, so his facing shows.
            let body = Mat4::from_translation(Vec3::new(0.0, 0.6, 0.0)) * Mat4::from_scale(Vec3::new(0.7, 1.2, 0.7));
            let snout = Mat4::from_translation(Vec3::new(0.0, 0.9, 0.4)) * Mat4::from_scale(Vec3::new(0.3, 0.25, 0.5));
            scene.models.push(Instance::new(self.body, place * body));
            scene.models.push(Instance::new(self.body, place * snout));
            let at = place.transform_point3(Vec3::ZERO);
            let eye = place.transform_point3(Vec3::new(0.0, CAMERA_HEIGHT, -CAMERA_DISTANCE));
            scene.camera = Camera {
                view: renderer::glam::camera::rh::view::look_at_mat4(eye, at + Vec3::Y * CAMERA_LOOK_HEIGHT, Vec3::Y),
                fov_y: 50f32.to_radians(),
                near: 0.1,
                far: 100.0,
            };
        }
        if let Some(font) = font {
            font.text(
                &mut scene,
                "Cross jumps; again in the air stomps. F1 controls, F2 display",
                [4.0, 4.0],
                1.0,
                [1.0; 4],
            );
        }
        scene
    }
}

/// Where a character's body is drawn: its position in metres, turned to
/// its facing.
fn placement(body: &Character) -> Mat4 {
    let p = body.position;
    let at = Vec3::new(p.x as f32, p.y as f32, p.z as f32) * METRES_PER_UNIT;
    Mat4::from_translation(at) * Mat4::from_rotation_y(body.facing.radians())
}

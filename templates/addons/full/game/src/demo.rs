//! A stand-in for drawing the game: a cube on a floor that the pad moves,
//! with the first glTF model the import wrote, if any, beside it. Replace it
//! with the game's own drawing of [`game::Game`]'s state.
use psx_scenario::{Button, Pad};
use renderer::{Alpha, Camera, Font, Instance, Light, Mat4, Mesh, ModelId, Primitive, Renderer, Scene, Vec3, Vertex};
use std::path::{Path, PathBuf};

pub struct Demo {
    cube: ModelId,
    floor: ModelId,
    imported: Option<(ModelId, Mat4)>,
    position: Vec3,
    heading: f32,
}

impl Demo {
    /// Builds the demo's models, and loads one model from `imported` (the
    /// import's folder) when it has any.
    pub fn new(renderer: &mut Renderer, imported: &Path) -> Self {
        let cube = renderer.model(&renderer::cube(
            1.0,
            [[0.9, 0.3, 0.2], [0.9, 0.3, 0.2], [1.0, 0.8, 0.3], [0.5, 0.2, 0.1], [0.8, 0.4, 0.2], [0.8, 0.4, 0.2]],
        ));
        let checker: Vec<u8> = (0..64)
            .flat_map(|i| if (i % 8 + i / 8) % 2 == 0 { [90, 110, 90, 255] } else { [70, 90, 70, 255] })
            .collect();
        let texture = renderer.texture(8, 8, &checker);
        // Tiles of 2x2 units, one checker texture each: the PS1 maps
        // textures affinely, so large polygons warp; games cut them up.
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for tz in -10..10 {
            for tx in -10..10 {
                let base = vertices.len() as u32;
                for (dx, dz) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                    vertices.push(Vertex {
                        position: [(tx + dx) as f32 * 2.0, 0.0, (tz + dz) as f32 * 2.0],
                        normal: [0.0, 1.0, 0.0],
                        uv: [dx as f32, dz as f32],
                        color: [1.0; 4],
                    });
                }
                indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
            }
        }
        let floor = renderer.model(&Mesh {
            primitives: vec![Primitive { vertices, indices, texture: Some(texture), alpha: Alpha::Opaque, lit: true }],
        });
        let imported = first_model(imported).and_then(|path| match renderer.load_gltf(&path) {
            Ok(model) => {
                // Two units tall, standing on the floor beside the start.
                let (lo, hi) = renderer.bounds(model)?;
                let scale = 2.0 / (hi - lo).max_element().max(1e-3);
                let base = Vec3::new((lo.x + hi.x) / 2.0, lo.y, (lo.z + hi.z) / 2.0);
                let place = Mat4::from_translation(Vec3::new(3.0, 0.0, -3.0))
                    * Mat4::from_scale(Vec3::splat(scale))
                    * Mat4::from_translation(-base);
                Some((model, place))
            }
            Err(e) => {
                eprintln!("{e}");
                None
            }
        });
        Self { cube, floor, imported, position: Vec3::new(0.0, 0.5, 0.0), heading: 0.0 }
    }

    /// One tick: the d-pad (or the left stick in analog mode) moves the cube.
    pub fn tick(&mut self, pad: &Pad) {
        let axis = |minus, plus| f32::from(u8::from(pad.held(plus))) - f32::from(u8::from(pad.held(minus)));
        let mut step = Vec3::new(axis(Button::Left, Button::Right), 0.0, axis(Button::Up, Button::Down));
        if pad.analog {
            let stick = |b: u8| (f32::from(b) - 127.5) / 127.5;
            step += Vec3::new(stick(pad.sticks[0]), 0.0, stick(pad.sticks[1]));
        }
        if step.length() > 0.1 {
            let step = step.clamp_length_max(1.0) * 0.08;
            self.position = (self.position + step).clamp(Vec3::new(-19.5, 0.5, -19.5), Vec3::new(19.5, 0.5, 19.5));
            self.heading = step.x.atan2(step.z);
        }
    }

    pub fn scene(&self, font: Option<&Font>) -> Scene {
        let mut scene = Scene { clear: [0.35, 0.5, 0.75], ambient: [0.55; 3], ..Default::default() };
        let eye = self.position + Vec3::new(0.0, 3.0, 6.0);
        scene.camera = Camera {
            view: renderer::glam::camera::rh::view::look_at_mat4(eye, self.position, Vec3::Y),
            fov_y: 55f32.to_radians(),
            near: 0.1,
            far: 100.0,
        };
        scene.lights.push(Light { towards: Vec3::new(0.3, 1.0, 0.5), color: [0.6; 3] });
        scene.fog = Some(renderer::Fog { color: [0.35, 0.5, 0.75], near: 15.0, far: 40.0 });
        scene.models.push(Instance::new(self.floor, Mat4::IDENTITY));
        let cube = Mat4::from_translation(self.position) * Mat4::from_rotation_y(self.heading);
        scene.models.push(Instance::new(self.cube, cube));
        if let Some((model, place)) = self.imported {
            scene.models.push(Instance::new(model, place));
        }
        if let Some(font) = font {
            font.text(&mut scene, "Demo: the d-pad moves. F1 controls, F2 display", [4.0, 4.0], 1.0, [1.0; 4]);
        }
        scene
    }
}

/// The first `.gltf` in `dir` or its subfolders, by name.
fn first_model(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    let found = entries.iter().find(|p| p.extension().is_some_and(|e| e == "gltf")).cloned();
    found.or_else(|| entries.iter().filter(|p| p.is_dir()).find_map(|d| first_model(d)))
}

//! Shows a glTF model (as `psx assets extract` or the importers write them)
//! turning in front of the camera, in either look:
//!
//! ```text
//! cargo run -p renderer --example view -- MODEL.gltf
//! ```
//!
//! F2 switches the look, F3 the aspect (the game's 4:3 or the window's
//! shape), F11 or Alt+Enter fullscreen; left and right turn the model, up and
//! down move the camera. Without a model it shows a cube.
//!
//! `--capture DIR` draws it headless instead, in both looks at 4:3 and at
//! 16:9, as DIR/{ps1,modern}-{4x3,16x9}.png. `--seconds N` closes the window
//! after N seconds.
use renderer::*;
use shell::{App, Button, Config, Input, Shell};
use std::path::{Path, PathBuf};

struct View {
    path: Option<PathBuf>,
    seconds: Option<f64>,
    ticks: u64,
    renderer: Option<Renderer>,
    font: Option<Font>,
    model: Option<ModelId>,
    center: Vec3,
    radius: f32,
    turn: f32,
    distance: f32,
}

impl View {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            seconds: None,
            ticks: 0,
            renderer: None,
            font: None,
            model: None,
            center: Vec3::ZERO,
            radius: 1.0,
            turn: 0.6,
            distance: 2.2,
        }
    }

    fn load(&mut self, r: &mut Renderer) -> Result<(), String> {
        let model = match &self.path {
            Some(path) => r.load_gltf(path)?,
            None => r.model(&cube(
                2.0,
                [[0.9, 0.3, 0.2], [0.2, 0.8, 0.3], [0.3, 0.4, 0.9], [0.9, 0.9, 0.3], [0.8, 0.8, 0.8], [0.5, 0.2, 0.7]],
            )),
        };
        let (lo, hi) = r.bounds(model).unwrap_or((Vec3::splat(-1.0), Vec3::splat(1.0)));
        self.center = (lo + hi) / 2.0;
        self.radius = ((hi - lo).length() / 2.0).max(1e-3);
        self.model = Some(model);
        self.font = Some(Font::builtin(r));
        Ok(())
    }

    fn scene(&self, r: &Renderer) -> Scene {
        let mut scene = Scene { clear: [0.12, 0.12, 0.16], ambient: [0.5; 3], ..Default::default() };
        let eye = self.center + Vec3::new(0.0, self.radius * 0.6, self.radius * self.distance);
        scene.camera = Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, self.center, Vec3::Y),
            fov_y: 50f32.to_radians(),
            near: self.radius * 0.05,
            far: self.radius * 20.0,
        };
        scene.lights.push(Light { towards: Vec3::new(0.4, 1.0, 0.6), color: [0.6; 3] });
        if let Some(model) = self.model {
            let spin = Mat4::from_translation(self.center)
                * Mat4::from_rotation_y(self.turn)
                * Mat4::from_translation(-self.center);
            scene.models.push(Instance::new(model, spin));
        }
        if let Some(font) = &self.font {
            let d = r.display();
            let label = format!("{:?} {:?}  F2 look  F3 aspect  F11 fullscreen", d.look, d.aspect);
            font.text(&mut scene, &label, [4.0, 4.0], 1.0, [1.0; 4]);
        }
        scene
    }
}

impl App for View {
    fn start(&mut self, shell: &mut Shell) -> Result<(), String> {
        let options = Options { game: [320, 240], ..Default::default() };
        let mut r = Renderer::new(shell.window().clone(), shell.size(), options)?;
        self.load(&mut r)?;
        self.renderer = Some(r);
        Ok(())
    }

    fn tick(&mut self, shell: &mut Shell) {
        self.ticks += 1;
        if self.seconds.is_some_and(|s| self.ticks as f64 >= s * 60.0) {
            shell.exit();
        }
        let Some(r) = &mut self.renderer else { return };
        let mut d = r.display();
        for input in shell.pressed().to_vec() {
            match input {
                Input::Key(k) if k == "F2" => {
                    d.look = if d.look == Look::Ps1 { Look::Modern } else { Look::Ps1 };
                }
                Input::Key(k) if k == "F3" => {
                    d.aspect = if d.aspect == Aspect::Fixed { Aspect::Window } else { Aspect::Fixed };
                }
                Input::Key(k) if k == "Escape" => shell.exit(),
                _ => {}
            }
        }
        r.set_display(d);
        let pad = shell.pad();
        let axis = |minus, plus| f32::from(u8::from(pad.held(plus))) - f32::from(u8::from(pad.held(minus)));
        self.turn += axis(Button::Left, Button::Right) * 0.03;
        self.distance = (self.distance + axis(Button::Up, Button::Down) * 0.03).clamp(1.0, 8.0);
    }

    fn draw(&mut self, _shell: &mut Shell, _blend: f32) {
        let Some(r) = &self.renderer else { return };
        let scene = self.scene(r);
        let r = self.renderer.as_mut().expect("renderer");
        if let Err(e) = r.render(&scene) {
            eprintln!("{e}");
        }
    }

    fn resized(&mut self, _shell: &mut Shell, size: [u32; 2]) {
        if let Some(r) = &mut self.renderer {
            r.resize(size[0], size[1]);
        }
    }
}

fn capture(view: &mut View, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut r = Renderer::headless(Options { game: [320, 240], ..Default::default() })?;
    view.load(&mut r)?;
    for look in [Look::Ps1, Look::Modern] {
        for (name, window, aspect) in [("4x3", [960, 720], Aspect::Fixed), ("16x9", [1280, 720], Aspect::Window)] {
            r.set_display(Display { look, aspect, ..Default::default() });
            r.resize(window[0], window[1]);
            let frame = r.capture(&view.scene(&r))?;
            let path = dir.join(format!("{}-{name}.png", format!("{look:?}").to_lowercase()));
            frame.save_png(&path)?;
            println!("{} ({}x{})", path.display(), frame.width, frame.height);
        }
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let path = args.iter().find(|a| a.ends_with(".gltf") || a.ends_with(".glb")).map(PathBuf::from);
    let mut view = View::new(path);
    view.seconds = value("--seconds").and_then(|s| s.parse().ok());
    if let Some(dir) = value("--capture") {
        return capture(&mut view, Path::new(&dir));
    }
    let config = Config { name: "renderer-view".into(), title: "renderer view".into(), ..Default::default() };
    shell::run(config, view)
}

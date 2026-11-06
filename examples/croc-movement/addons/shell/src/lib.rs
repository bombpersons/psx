//! The window and the loop around the game: a fixed logic tick at the
//! original's rate, drawing as often as the display allows, the pad from the
//! keyboard and gamepads (bound as `psx play` binds them, saved in the same
//! `controls.json` format), and windowed or borderless fullscreen (F11 or
//! Alt+Enter, remembered).
//!
//! ```ignore
//! shell::run(Config { name: "game".into(), title: "Game".into(), tick_hz: 60.0, ..Default::default() }, MyApp::default())
//! ```
//!
//! The shell calls [`App::start`] once the window exists (create the
//! renderer there), [`App::tick`] at `tick_hz` with [`Shell::pad`] as the
//! tick's input, and [`App::draw`] once per displayed frame.
pub mod bindings;
pub mod settings;

pub use bindings::{Bindings, Device};
pub use psx_scenario::{Button, Pad};
pub use winit::window::Window;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, WindowId};

/// What the shell needs at the start.
#[derive(Clone, Debug)]
pub struct Config {
    /// The folder name for the player's settings (controls.json,
    /// window.json) under the config directory.
    pub name: String,
    pub title: String,
    /// Logic ticks a second.
    pub tick_hz: f64,
    /// The window's first inner size in pixels.
    pub size: [u32; 2],
}

impl Default for Config {
    fn default() -> Self {
        Self { name: "game".into(), title: "Game".into(), tick_hz: 60.0, size: [960, 720] }
    }
}

/// An input that went down since the last tick, for menus and rebinding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// A keyboard key, named as in controls.json.
    Key(String),
    /// A gamepad button, named as in controls.json.
    Button(String),
}

/// The game, as the shell runs it.
pub trait App {
    /// The window exists: create the renderer, load what the game needs.
    /// An error closes the window and [`run`] returns it.
    fn start(&mut self, shell: &mut Shell) -> Result<(), String>;

    /// One logic tick; [`Shell::pad`] is its input.
    fn tick(&mut self, shell: &mut Shell);

    /// One displayed frame. `blend` (0..1) is how far between the last tick
    /// and the next the display is, for smoothing motion if wanted.
    fn draw(&mut self, shell: &mut Shell, blend: f32);

    /// The window's inner size changed, in pixels.
    fn resized(&mut self, _shell: &mut Shell, _size: [u32; 2]) {}

    /// A file was dropped on the window.
    fn dropped(&mut self, _shell: &mut Shell, _path: PathBuf) {}
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct WindowSettings {
    fullscreen: bool,
}

/// What the game sees of the shell.
pub struct Shell {
    config: Config,
    window: Arc<Window>,
    bindings: Bindings,
    bindings_error: Option<String>,
    gilrs: Option<gilrs::Gilrs>,
    keys: BTreeSet<String>,
    buttons: BTreeSet<String>,
    pressed: Vec<Input>,
    analog: bool,
    alt: bool,
    exit: bool,
}

impl Shell {
    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }

    /// The window's inner size in pixels.
    pub fn size(&self) -> [u32; 2] {
        let s = self.window.inner_size();
        [s.width.max(1), s.height.max(1)]
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The pad as the held keys, gamepad buttons and sticks press it. A pad
    /// not in analog mode gets the left stick as the d-pad.
    pub fn pad(&mut self) -> Pad {
        let sticks = self.sticks();
        let mut buttons = BTreeSet::new();
        for (device, held) in [(Device::Keyboard, &self.keys), (Device::Gamepad, &self.buttons)] {
            for input in held {
                if let Some(b) =
                    self.bindings.action_of(device, input).and_then(|a| Button::parse(bindings::ACTIONS[a]))
                {
                    buttons.insert(b);
                }
            }
        }
        if self.analog {
            return Pad { buttons: buttons.into_iter().collect(), analog: true, sticks };
        }
        // Half travel, so a diagonal does not press both directions too early.
        let [x, y, ..] = sticks;
        for (on, b) in
            [(x < 0x40, Button::Left), (x > 0xC0, Button::Right), (y < 0x40, Button::Up), (y > 0xC0, Button::Down)]
        {
            if on {
                buttons.insert(b);
            }
        }
        Pad { buttons: buttons.into_iter().collect(), analog: false, sticks: [0x80; 4] }
    }

    fn sticks(&self) -> [u8; 4] {
        use gilrs::Axis;
        let dz = self.bindings.stick_deadzone;
        let mut out = [0x80u8; 4];
        let Some(gilrs) = &self.gilrs else { return out };
        // The first gamepad with a stick off centre.
        for (_, pad) in gilrs.gamepads() {
            let v = [Axis::LeftStickX, Axis::LeftStickY, Axis::RightStickX, Axis::RightStickY].map(|a| pad.value(a));
            let s = [
                bindings::stick_byte(v[0], dz, false),
                bindings::stick_byte(v[1], dz, true),
                bindings::stick_byte(v[2], dz, false),
                bindings::stick_byte(v[3], dz, true),
            ];
            if s != [0x80; 4] {
                out = s;
                break;
            }
        }
        out
    }

    /// Whether the pad is in analog mode: the ANALOG button toggles it, and
    /// the game may set it as the original did.
    pub fn analog(&self) -> bool {
        self.analog
    }

    pub fn set_analog(&mut self, on: bool) {
        self.analog = on;
    }

    /// The inputs that went down since the last tick, in order.
    pub fn pressed(&self) -> &[Input] {
        &self.pressed
    }

    /// Forgets this tick's presses, so the game does not also act on what a
    /// menu took.
    pub fn consume_pressed(&mut self) {
        self.pressed.clear();
    }

    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// controls.json's problem, when it could not be read (the defaults are
    /// used meanwhile).
    pub fn bindings_error(&self) -> Option<&str> {
        self.bindings_error.as_deref()
    }

    /// Replaces the bindings and saves them to controls.json.
    pub fn set_bindings(&mut self, bindings: Bindings) -> Result<(), String> {
        bindings.validate()?;
        self.bindings = bindings;
        let path = self.settings_path("controls.json").ok_or("no config folder")?;
        bindings::save(&path, &self.bindings)
    }

    /// `<config dir>/<name>/<file>`, where the player's settings live.
    pub fn settings_path(&self, file: &str) -> Option<PathBuf> {
        settings::config_path(&self.config.name, file)
    }

    pub fn fullscreen(&self) -> bool {
        self.window.fullscreen().is_some()
    }

    /// Borderless fullscreen on the window's monitor, or windowed;
    /// remembered in window.json.
    pub fn set_fullscreen(&mut self, on: bool) {
        self.window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
        if let Some(path) = self.settings_path("window.json")
            && let Ok(json) = serde_json::to_vec_pretty(&WindowSettings { fullscreen: on })
        {
            let _ = settings::write(&path, &json);
        }
    }

    /// Closes the window and returns from [`run`] after this frame.
    pub fn exit(&mut self) {
        self.exit = true;
    }

    fn key(&mut self, code: KeyCode, down: bool, repeat: bool) {
        if matches!(code, KeyCode::AltLeft | KeyCode::AltRight) {
            self.alt = down;
        }
        if down && !repeat && (code == KeyCode::F11 || (code == KeyCode::Enter && self.alt)) {
            self.set_fullscreen(!self.fullscreen());
            return;
        }
        let Some(name) = bindings::key_name(code) else { return };
        if !down {
            self.keys.remove(&name);
            return;
        }
        if repeat {
            return;
        }
        if self.bindings.action_of(Device::Keyboard, &name) == Some(bindings::ANALOG) {
            self.analog = !self.analog;
        }
        self.keys.insert(name.clone());
        self.pressed.push(Input::Key(name));
    }

    fn poll_gamepads(&mut self) {
        let Some(gilrs) = &mut self.gilrs else { return };
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::ButtonPressed(b, code) => {
                    let name = bindings::button_name(b, code);
                    if self.bindings.action_of(Device::Gamepad, &name) == Some(bindings::ANALOG) {
                        self.analog = !self.analog;
                    }
                    self.buttons.insert(name.clone());
                    self.pressed.push(Input::Button(name));
                }
                gilrs::EventType::ButtonReleased(b, code) => {
                    self.buttons.remove(&bindings::button_name(b, code));
                }
                gilrs::EventType::Disconnected => self.buttons.clear(),
                _ => {}
            }
        }
    }
}

/// At most this many ticks run to catch up after a stall; the rest are
/// dropped rather than running the game fast.
const MAX_CATCH_UP: u32 = 8;

struct Runner<A: App> {
    config: Config,
    app: A,
    shell: Option<Shell>,
    error: Option<String>,
    last: Instant,
    behind: Duration,
}

impl<A: App> Runner<A> {
    fn step(&mut self) {
        let Some(shell) = &mut self.shell else { return };
        shell.poll_gamepads();
        let tick = Duration::from_secs_f64(1.0 / self.config.tick_hz);
        let now = Instant::now();
        self.behind += now - self.last;
        self.last = now;
        let mut ran = 0;
        while self.behind >= tick && ran < MAX_CATCH_UP {
            self.app.tick(shell);
            shell.pressed.clear();
            self.behind -= tick;
            ran += 1;
        }
        if ran == MAX_CATCH_UP {
            self.behind = Duration::ZERO;
        }
        shell.window.request_redraw();
    }
}

impl<A: App> ApplicationHandler for Runner<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shell.is_some() {
            return;
        }
        let fullscreen = settings::config_path(&self.config.name, "window.json")
            .and_then(|p| settings::read(&p).ok().flatten())
            .and_then(|b| serde_json::from_slice::<WindowSettings>(&b).ok())
            .unwrap_or_default()
            .fullscreen;
        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(winit::dpi::PhysicalSize::new(self.config.size[0], self.config.size[1]))
            .with_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.error = Some(format!("window: {e}"));
                event_loop.exit();
                return;
            }
        };
        let (bindings, bindings_error) = match settings::config_path(&self.config.name, "controls.json") {
            Some(path) => match bindings::load(&path) {
                Ok(b) => (b.unwrap_or_default(), None),
                Err(e) => (Bindings::default(), Some(e)),
            },
            None => (Bindings::default(), None),
        };
        let mut shell = Shell {
            config: self.config.clone(),
            window,
            bindings,
            bindings_error,
            // No gamepads (no udev, say) leaves the keyboard.
            gilrs: gilrs::Gilrs::new().ok(),
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            pressed: Vec::new(),
            analog: false,
            alt: false,
            exit: false,
        };
        if let Err(e) = self.app.start(&mut shell) {
            self.error = Some(e);
            event_loop.exit();
            return;
        }
        self.last = Instant::now();
        self.shell = Some(shell);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(shell) = &mut self.shell else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let size = [size.width.max(1), size.height.max(1)];
                self.app.resized(shell, size);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    shell.key(code, event.state == ElementState::Pressed, event.repeat);
                }
            }
            WindowEvent::Focused(false) => {
                shell.keys.clear();
                shell.alt = false;
            }
            WindowEvent::DroppedFile(path) => self.app.dropped(shell, path),
            WindowEvent::RedrawRequested => {
                let tick = 1.0 / self.config.tick_hz;
                let blend = (self.behind.as_secs_f64() / tick).clamp(0.0, 1.0) as f32;
                self.app.draw(shell, blend);
            }
            _ => {}
        }
        if shell.exit {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.step();
        if self.shell.as_ref().is_some_and(|s| s.exit) {
            event_loop.exit();
        }
    }
}

/// Opens the window and runs `app` until the window closes or
/// [`Shell::exit`].
pub fn run<A: App>(config: Config, app: A) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("no display: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut runner = Runner { config, app, shell: None, error: None, last: Instant::now(), behind: Duration::ZERO };
    event_loop.run_app(&mut runner).map_err(|e| e.to_string())?;
    match runner.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

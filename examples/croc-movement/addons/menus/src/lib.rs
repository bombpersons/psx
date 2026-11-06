//! Two menus over the game: F1 controls (rebind keyboard and gamepad, saved
//! to controls.json) and F2 display (look, aspect, fullscreen and the PS1
//! effects, previewed as they change, saved to display.json). Arrows or the
//! d-pad move, Enter or South picks, Escape or East closes without saving.
//!
//! ```ignore
//! // start: renderer.set_display(menus.load_display());
//! // tick:  if menus.tick(shell, renderer) { return; } // the menu took the input
//! // draw:  menus.draw(&mut scene, &font, &renderer, shell);
//! ```
use renderer::{Aspect, Display, Font, Look, Renderer, Scene};
use shell::bindings::{ACTIONS, RESERVED_KEYS};
use shell::{Bindings, Device, Input, Shell};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    Controls,
    Display,
}

/// The menu's own inputs, from keys or gamepad buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nav {
    Up,
    Down,
    Left,
    Right,
    Pick,
    Back,
}

fn nav(input: &Input) -> Option<Nav> {
    let name = match input {
        Input::Key(k) | Input::Button(k) => k.as_str(),
    };
    Some(match name {
        "Up" | "DPadUp" => Nav::Up,
        "Down" | "DPadDown" => Nav::Down,
        "Left" | "DPadLeft" => Nav::Left,
        "Right" | "DPadRight" => Nav::Right,
        "Enter" | "South" => Nav::Pick,
        "Escape" | "East" => Nav::Back,
        _ => return None,
    })
}

const DISPLAY_ROWS: usize = 7;

pub struct Menus {
    name: String,
    open: Option<Open>,
    row: usize,
    /// Controls: the device column.
    device: Device,
    /// Controls: waiting for the input to bind to the row's action.
    listening: bool,
    bindings: Bindings,
    /// Display: as it was when the menu opened, restored on Escape.
    display_before: Option<(Display, bool)>,
    message: Option<String>,
}

impl Menus {
    /// `name` is the game's settings folder, as in [`shell::Config::name`].
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            open: None,
            row: 0,
            device: Device::Keyboard,
            listening: false,
            bindings: Bindings::default(),
            display_before: None,
            message: None,
        }
    }

    /// display.json, or the defaults.
    pub fn load_display(&self) -> Display {
        shell::settings::config_path(&self.name, "display.json")
            .and_then(|p| shell::settings::read(&p).ok().flatten())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_display(&self, display: &Display) -> Result<(), String> {
        let path = shell::settings::config_path(&self.name, "display.json").ok_or("no config folder")?;
        let json = serde_json::to_vec_pretty(display).map_err(|e| e.to_string())?;
        shell::settings::write(&path, &json)
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn open(&mut self, which: Open, shell: &Shell, renderer: &Renderer) {
        self.open = Some(which);
        self.row = 0;
        self.listening = false;
        self.message = shell.bindings_error().map(str::to_string);
        self.bindings = shell.bindings().clone();
        self.display_before = Some((renderer.display(), shell.fullscreen()));
    }

    fn close(&mut self, shell: &mut Shell, renderer: &mut Renderer, keep: bool) {
        if !keep && let Some((display, fullscreen)) = self.display_before {
            renderer.set_display(display);
            if shell.fullscreen() != fullscreen {
                shell.set_fullscreen(fullscreen);
            }
        }
        self.open = None;
        self.listening = false;
    }

    /// Handles this tick's presses: F1 and F2 open and close the menus, and
    /// an open menu takes every press (the pad is then for the menu, not the
    /// game). True while a menu is open: skip the game's tick.
    pub fn tick(&mut self, shell: &mut Shell, renderer: &mut Renderer) -> bool {
        for input in shell.pressed().to_vec() {
            if self.listening {
                self.listen(&input);
                continue;
            }
            let toggle = match &input {
                Input::Key(k) if k == "F1" => Some(Open::Controls),
                Input::Key(k) if k == "F2" => Some(Open::Display),
                _ => None,
            };
            if let Some(which) = toggle {
                if self.open == Some(which) {
                    self.close(shell, renderer, false);
                } else {
                    if self.open.is_some() {
                        self.close(shell, renderer, false);
                    }
                    self.open(which, shell, renderer);
                }
                continue;
            }
            let Some(n) = nav(&input) else { continue };
            match self.open {
                Some(Open::Controls) => self.controls(n, shell, renderer),
                Some(Open::Display) => self.display(n, shell, renderer),
                None => {}
            }
        }
        let open = self.is_open();
        if open {
            shell.consume_pressed();
        }
        open
    }

    fn listen(&mut self, input: &Input) {
        self.listening = false;
        let (device, name) = match input {
            Input::Key(k) => (Device::Keyboard, k),
            Input::Button(b) => (Device::Gamepad, b),
        };
        if device == Device::Keyboard && name == "Escape" {
            return;
        }
        if device == Device::Keyboard && matches!(name.as_str(), "Backspace" | "Delete") {
            self.bindings.clear(self.row, self.device);
            return;
        }
        if device != self.device {
            self.message =
                Some(format!("Press a {} input", if self.device == Device::Keyboard { "keyboard" } else { "gamepad" }));
            return;
        }
        if RESERVED_KEYS.contains(&name.as_str()) && device == Device::Keyboard {
            self.message = Some(format!("{name} is kept for the menus"));
            return;
        }
        match self.bindings.assign(self.row, device, name) {
            Ok(Some(other)) => self.message = Some(format!("{name} moved from {}", ACTIONS[other])),
            Ok(None) => self.message = None,
            Err(e) => self.message = Some(e),
        }
    }

    fn controls(&mut self, n: Nav, shell: &mut Shell, renderer: &mut Renderer) {
        let rows = ACTIONS.len() + 2;
        match n {
            Nav::Up => self.row = (self.row + rows - 1) % rows,
            Nav::Down => self.row = (self.row + 1) % rows,
            Nav::Left | Nav::Right => {
                self.device = if self.device == Device::Keyboard { Device::Gamepad } else { Device::Keyboard }
            }
            Nav::Back => self.close(shell, renderer, false),
            Nav::Pick if self.row < ACTIONS.len() => {
                self.listening = true;
                self.message = Some("Press the input to bind; Backspace clears, Escape keeps".into());
            }
            Nav::Pick if self.row == ACTIONS.len() => match shell.set_bindings(self.bindings.clone()) {
                Ok(()) => self.close(shell, renderer, true),
                Err(e) => self.message = Some(e),
            },
            Nav::Pick => {
                self.bindings = Bindings::default();
                self.message = Some("Defaults restored; Save keeps them".into());
            }
        }
    }

    fn display(&mut self, n: Nav, shell: &mut Shell, renderer: &mut Renderer) {
        let mut d = renderer.display();
        match n {
            Nav::Up => self.row = (self.row + DISPLAY_ROWS - 1) % DISPLAY_ROWS,
            Nav::Down => self.row = (self.row + 1) % DISPLAY_ROWS,
            Nav::Back => self.close(shell, renderer, false),
            Nav::Pick if self.row == DISPLAY_ROWS - 1 => match self.save_display(&d) {
                Ok(()) => self.close(shell, renderer, true),
                Err(e) => self.message = Some(e),
            },
            Nav::Left | Nav::Right | Nav::Pick => {
                match self.row {
                    0 => d.look = if d.look == Look::Ps1 { Look::Modern } else { Look::Ps1 },
                    1 => d.aspect = if d.aspect == Aspect::Fixed { Aspect::Window } else { Aspect::Fixed },
                    2 => shell.set_fullscreen(!shell.fullscreen()),
                    3 => d.ps1.vertex_snap = !d.ps1.vertex_snap,
                    4 => d.ps1.dither = !d.ps1.dither,
                    5 => d.ps1.quantize = !d.ps1.quantize,
                    _ => {}
                }
                renderer.set_display(d);
            }
        }
    }

    /// The open menu over the scene.
    pub fn draw(&self, scene: &mut Scene, font: &Font, renderer: &Renderer, shell: &Shell) {
        let Some(open) = self.open else { return };
        let (white, size, display) = (renderer.white(), renderer.layout_size(), renderer.display());
        let [w, h] = [size[0] as f32, size[1] as f32];
        let line = font.height(1.0) + 2.0;
        let cell = font.cell[0] as f32;
        scene.rect(white, [0.0, 0.0, w, h], [0.02, 0.02, 0.05, 1.0]);
        let white_text = [1.0; 4];
        let dim = [0.6, 0.6, 0.65, 1.0];
        let hi = [1.0, 0.85, 0.3, 1.0];
        let x = 6.0;
        let mut y = 4.0;
        let title = match open {
            Open::Controls => "Controls (F1)",
            Open::Display => "Display (F2)",
        };
        font.text(scene, title, [x, y], 1.0, white_text);
        y += line + 2.0;
        match open {
            Open::Controls => {
                let (kx, gx) = (x + cell * 10.0, x + cell * 24.0);
                for (i, action) in ACTIONS.iter().enumerate() {
                    let selected = i == self.row;
                    if selected {
                        scene.rect(white, [0.0, y - 1.0, w, line], [0.15, 0.15, 0.25, 1.0]);
                    }
                    font.text(scene, action, [x, y], 1.0, if selected { hi } else { white_text });
                    for (device, cx) in [(Device::Keyboard, kx), (Device::Gamepad, gx)] {
                        let bound = self.bindings.slots(device)[i].as_deref().unwrap_or("-");
                        let text = if selected && self.listening && device == self.device { "..." } else { bound };
                        let color = if selected && device == self.device { hi } else { dim };
                        font.text(scene, text, [cx, y], 1.0, color);
                    }
                    y += line;
                }
                for (i, label) in ["Save", "Reset to defaults"].iter().enumerate() {
                    let selected = self.row == ACTIONS.len() + i;
                    font.text(scene, label, [x, y], 1.0, if selected { hi } else { white_text });
                    y += line;
                }
            }
            Open::Display => {
                let on = |b: bool| if b { "on" } else { "off" };
                let rows = [
                    ("Look", (if display.look == Look::Ps1 { "PS1" } else { "modern" }).to_string()),
                    ("Aspect", (if display.aspect == Aspect::Fixed { "4:3" } else { "window" }).to_string()),
                    ("Fullscreen", on(shell.fullscreen()).to_string()),
                    ("Vertex snap", on(display.ps1.vertex_snap).to_string()),
                    ("Dither", on(display.ps1.dither).to_string()),
                    ("15-bit colour", on(display.ps1.quantize).to_string()),
                    ("Save", String::new()),
                ];
                for (i, (label, value)) in rows.iter().enumerate() {
                    let color = if i == self.row { hi } else { white_text };
                    font.text(scene, label, [x, y], 1.0, color);
                    font.text(scene, value, [x + cell * 16.0, y], 1.0, if i == self.row { hi } else { dim });
                    y += line;
                }
            }
        }
        if let Some(m) = &self.message {
            font.text(scene, m, [x, h - line - 2.0], 1.0, [1.0, 0.7, 0.5, 1.0]);
        }
    }
}

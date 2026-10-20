//! The controls menu: one row per PS1 button with its keyboard and gamepad
//! input, then the stick deadzone, defaults, save and close. Like the
//! template's menu, a conflicting input swaps with the action that had it.
//!
//! Driven with the keyboard (arrows, Enter, Escape, Delete) or a gamepad
//! (d-pad, South to choose, East to go back, North to clear). Changes apply
//! at once; Save writes them to the controls file for later handoffs.

use super::bindings::{Bindings, Device, ACTIONS};

const DEADZONE_ROW: usize = ACTIONS.len();
const DEFAULTS_ROW: usize = DEADZONE_ROW + 1;
const SAVE_ROW: usize = DEFAULTS_ROW + 1;
const CLOSE_ROW: usize = SAVE_ROW + 1;
const ROWS: usize = CLOSE_ROW + 1;
/// Ticks (frames) to wait for an input before listening gives up.
const LISTEN_TICKS: u32 = 5 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Browse,
    /// Waiting for an input for the selected row; armed once everything is released,
    /// so the press that started listening is not taken as the binding.
    Listen { armed: bool, ticks: u32 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// The caller writes the bindings and reports the result with [`Menu::set_status`].
    Save,
}

pub struct Menu {
    pub row: usize,
    pub device: Device,
    mode: Mode,
    status: String,
}

enum Nav {
    Up,
    Down,
    Left,
    Right,
    Accept,
    Back,
    Clear,
}

fn nav(keys: &[&str], pad: &[&str]) -> Option<Nav> {
    let key = keys.iter().find_map(|k| match *k {
        "Up" => Some(Nav::Up),
        "Down" => Some(Nav::Down),
        "Left" => Some(Nav::Left),
        "Right" => Some(Nav::Right),
        "Enter" | "Space" => Some(Nav::Accept),
        "Escape" | "F1" => Some(Nav::Back),
        "Delete" | "Backspace" => Some(Nav::Clear),
        _ => None,
    });
    key.or_else(|| {
        pad.iter().find_map(|b| match *b {
            "DPadUp" => Some(Nav::Up),
            "DPadDown" => Some(Nav::Down),
            "DPadLeft" => Some(Nav::Left),
            "DPadRight" => Some(Nav::Right),
            "South" => Some(Nav::Accept),
            "East" => Some(Nav::Back),
            "North" => Some(Nav::Clear),
            _ => None,
        })
    })
}

impl Default for Menu {
    fn default() -> Self {
        Menu { row: 0, device: Device::Keyboard, mode: Mode::Browse, status: String::new() }
    }
}

impl Menu {
    pub fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }

    pub fn listening(&self) -> bool {
        matches!(self.mode, Mode::Listen { .. })
    }

    /// One frame of the menu. `keys`/`pad` are inputs newly pressed this frame;
    /// `held` is whether any key or gamepad button is still down.
    pub fn tick(&mut self, b: &mut Bindings, keys: &[&str], pad: &[&str], held: bool) -> Outcome {
        if let Mode::Listen { armed, ticks } = self.mode {
            return self.listen(b, keys, pad, held, armed, ticks);
        }
        let Some(n) = nav(keys, pad) else { return Outcome::Stay };
        let action_row = self.row < ACTIONS.len();
        match n {
            Nav::Up => self.row = (self.row + ROWS - 1) % ROWS,
            Nav::Down => self.row = (self.row + 1) % ROWS,
            Nav::Left | Nav::Right if self.row == DEADZONE_ROW => {
                let step = if matches!(n, Nav::Left) { -0.05 } else { 0.05 };
                b.stick_deadzone = ((b.stick_deadzone + step) * 20.0).round().clamp(0.0, 19.0) / 20.0;
            }
            Nav::Left | Nav::Right if action_row => {
                self.device = if self.device == Device::Keyboard { Device::Gamepad } else { Device::Keyboard }
            }
            Nav::Left | Nav::Right => {}
            Nav::Accept if action_row => {
                self.mode = Mode::Listen { armed: false, ticks: 0 };
                self.status = format!("Press a {} input for {} (Escape cancels)", self.device.label(), ACTIONS[self.row]);
            }
            Nav::Accept => match self.row {
                DEFAULTS_ROW => {
                    *b = Bindings::default();
                    self.status = "Defaults restored (not saved yet)".into();
                }
                SAVE_ROW => return Outcome::Save,
                CLOSE_ROW => return Outcome::Close,
                _ => {}
            },
            Nav::Back => return Outcome::Close,
            Nav::Clear if action_row => {
                b.clear(self.row, self.device);
                self.status = format!("{} {} unbound", ACTIONS[self.row], self.device.label());
            }
            Nav::Clear => {}
        }
        Outcome::Stay
    }

    fn listen(&mut self, b: &mut Bindings, keys: &[&str], pad: &[&str], held: bool, armed: bool, ticks: u32) -> Outcome {
        if keys.contains(&"Escape") {
            self.mode = Mode::Browse;
            self.status = "Cancelled".into();
            return Outcome::Stay;
        }
        if ticks >= LISTEN_TICKS {
            self.mode = Mode::Browse;
            self.status = "No input; cancelled".into();
            return Outcome::Stay;
        }
        let armed = armed || !held;
        let input = match self.device {
            Device::Keyboard => keys.first(),
            Device::Gamepad => pad.first(),
        };
        match input.filter(|_| armed) {
            Some(input) => {
                self.mode = Mode::Browse;
                self.status = match b.assign(self.row, self.device, input) {
                    Ok(Some(other)) => format!("{} = {input}; {} now has the old input", ACTIONS[self.row], ACTIONS[other]),
                    Ok(None) => format!("{} = {input}", ACTIONS[self.row]),
                    Err(e) => e,
                };
            }
            None => self.mode = Mode::Listen { armed, ticks: ticks + 1 },
        }
        Outcome::Stay
    }

    /// Text lines to draw, with the selected one marked.
    pub fn lines(&self, b: &Bindings, gamepads: &[String]) -> Vec<(String, bool)> {
        let mut out = vec![
            ("CONTROLS".to_string(), false),
            ("Arrows/d-pad move, Enter/South choose,".to_string(), false),
            ("Delete/North clear, Esc/East close.".to_string(), false),
            (format!("Gamepads: {}", if gamepads.is_empty() { "none found".to_string() } else { gamepads.join(", ") }), false),
            (String::new(), false),
            (format!("{:<9}{:<17}{:<17}", "BUTTON", "KEYBOARD", "GAMEPAD"), false),
        ];
        let listening = self.listening();
        for (i, action) in ACTIONS.iter().enumerate() {
            let cell = |d: Device| {
                let name = b.slots(d)[i].clone().unwrap_or_else(|| "-".into());
                let chosen = self.row == i && self.device == d;
                if chosen && listening {
                    "[press...]".to_string()
                } else if chosen {
                    format!("[{name}]")
                } else {
                    format!(" {name}")
                }
            };
            out.push((format!("{action:<9}{:<17}{:<17}", cell(Device::Keyboard), cell(Device::Gamepad)), self.row == i));
        }
        out.push((format!("Stick deadzone  < {:.2} >", b.stick_deadzone), self.row == DEADZONE_ROW));
        out.push(("Restore defaults".into(), self.row == DEFAULTS_ROW));
        out.push(("Save for future handoffs".into(), self.row == SAVE_ROW));
        out.push(("Close".into(), self.row == CLOSE_ROW));
        out.push((String::new(), false));
        out.push((self.status.clone(), false));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebinding_waits_for_release_then_takes_the_next_press() {
        let (mut m, mut b) = (Menu::default(), Bindings::default());
        m.row = 4; // CROSS
        assert_eq!(m.tick(&mut b, &["Enter"], &[], true), Outcome::Stay);
        assert!(m.listening());
        // Enter still held: a new press is not taken until everything is released.
        m.tick(&mut b, &["V"], &[], true);
        assert!(m.listening());
        m.tick(&mut b, &[], &[], false);
        m.tick(&mut b, &["V"], &[], true);
        assert!(!m.listening());
        assert_eq!(b.keyboard[4].as_deref(), Some("V"));
    }

    #[test]
    fn gamepad_column_binds_gamepad_buttons_and_swaps() {
        let (mut m, mut b) = (Menu::default(), Bindings::default());
        m.row = 4;
        m.tick(&mut b, &[], &["DPadRight"], true);
        assert_eq!(m.device, Device::Gamepad);
        m.tick(&mut b, &[], &["South"], true);
        m.tick(&mut b, &[], &[], false);
        // A key does nothing in the gamepad column.
        m.tick(&mut b, &["V"], &[], true);
        assert!(m.listening());
        m.tick(&mut b, &[], &["East"], true);
        assert_eq!(b.gamepad[4].as_deref(), Some("East"));
        assert_eq!(b.gamepad[5].as_deref(), Some("South"));
    }

    #[test]
    fn escape_cancels_and_rows_wrap() {
        let (mut m, mut b) = (Menu::default(), Bindings::default());
        m.tick(&mut b, &["Enter"], &[], false);
        m.tick(&mut b, &["Escape"], &[], true);
        assert!(!m.listening());
        assert_eq!(b, Bindings::default());
        m.tick(&mut b, &["Up"], &[], true);
        assert_eq!(m.row, CLOSE_ROW);
        assert_eq!(m.tick(&mut b, &["Enter"], &[], true), Outcome::Close);
        m.row = SAVE_ROW;
        assert_eq!(m.tick(&mut b, &[], &["South"], true), Outcome::Save);
        m.row = DEADZONE_ROW;
        m.tick(&mut b, &["Right"], &[], true);
        assert!((b.stick_deadzone - 0.25).abs() < 1e-6);
    }
}

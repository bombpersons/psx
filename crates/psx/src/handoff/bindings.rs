//! Which keyboard keys and gamepad buttons press which PS1 button.
//!
//! Physical inputs are names, so this module needs no window or gamepad
//! library: keyboard keys are minifb `Key` names (`X`, `Up`, `Enter`),
//! gamepad buttons are gilrs `Button` names (`South`, `DPadUp`), or `code:N`
//! for a button the gamepad mapping database does not name.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// PS1 buttons in menu order (the d-pad and face buttons first), then the
/// DualShock's ANALOG mode button.
pub const ACTIONS: [&str; 17] = [
    "UP", "DOWN", "LEFT", "RIGHT", "CROSS", "CIRCLE", "SQUARE", "TRIANGLE", "L1", "R1", "L2", "R2", "START", "SELECT",
    "L3", "R3", "ANALOG",
];

/// Keys the handoff window keeps for itself; they cannot be bound.
pub const RESERVED_KEYS: [&str; 14] =
    ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "Tab", "Escape"];

/// The input bit of an action: the pad's (see `psx_core::sio0::button`), or
/// [`crate::replay::ANALOG`].
pub fn bit(action: usize) -> u32 {
    match psx_core::sio0::button_from_name(ACTIONS[action]) {
        Some(b) => b.into(),
        None => crate::replay::ANALOG,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Keyboard,
    Gamepad,
}

impl Device {
    pub fn label(self) -> &'static str {
        match self {
            Device::Keyboard => "keyboard",
            Device::Gamepad => "gamepad",
        }
    }
}

/// One optional physical input per action and device; no input presses two actions.
#[derive(Clone, Debug, PartialEq)]
pub struct Bindings {
    pub keyboard: [Option<String>; 17],
    pub gamepad: [Option<String>; 17],
    /// Stick travel (0..1) ignored around the centre.
    pub stick_deadzone: f32,
}

impl Default for Bindings {
    fn default() -> Self {
        let some = |names: [&str; 17]| names.map(|n| (!n.is_empty()).then(|| n.to_string()));
        Bindings {
            // The defaults a port's shell copies, so a person plays the port with the same keys.
            keyboard: some([
                "Up",
                "Down",
                "Left",
                "Right",
                "X",
                "C",
                "Z",
                "S",
                "Q",
                "E",
                "Key1",
                "Key3",
                "Enter",
                "RightShift",
                "",
                "",
                "A",
            ]),
            gamepad: some([
                "DPadUp",
                "DPadDown",
                "DPadLeft",
                "DPadRight",
                "South",
                "East",
                "West",
                "North",
                "LeftTrigger",
                "RightTrigger",
                "LeftTrigger2",
                "RightTrigger2",
                "Start",
                "Select",
                "LeftThumb",
                "RightThumb",
                "Mode",
            ]),
            stick_deadzone: 0.2,
        }
    }
}

impl Bindings {
    pub fn slots(&self, device: Device) -> &[Option<String>; 17] {
        match device {
            Device::Keyboard => &self.keyboard,
            Device::Gamepad => &self.gamepad,
        }
    }

    fn slots_mut(&mut self, device: Device) -> &mut [Option<String>; 17] {
        match device {
            Device::Keyboard => &mut self.keyboard,
            Device::Gamepad => &mut self.gamepad,
        }
    }

    /// The action `input` is bound to on `device`.
    pub fn action_of(&self, device: Device, input: &str) -> Option<usize> {
        self.slots(device).iter().position(|s| s.as_deref() == Some(input))
    }

    /// Bind `input` to `action`. An action already using it gets this action's
    /// previous input (possibly none) instead, and is returned.
    pub fn assign(&mut self, action: usize, device: Device, input: &str) -> Result<Option<usize>, String> {
        if device == Device::Keyboard && RESERVED_KEYS.contains(&input) {
            return Err(format!("{input} is a handoff window key"));
        }
        let other = self.action_of(device, input).filter(|&o| o != action);
        let slots = self.slots_mut(device);
        match other {
            Some(o) => slots.swap(action, o),
            None => slots[action] = Some(input.to_string()),
        }
        Ok(other)
    }

    pub fn clear(&mut self, action: usize, device: Device) {
        self.slots_mut(device)[action] = None;
    }

    /// The pad bits pressed by the held keys and gamepad buttons.
    pub fn buttons<'a>(&self, keys: impl IntoIterator<Item = &'a str>, pad: impl IntoIterator<Item = &'a str>) -> u32 {
        let mut bits = 0;
        for k in keys {
            if let Some(a) = self.action_of(Device::Keyboard, k) {
                bits |= bit(a);
            }
        }
        for b in pad {
            if let Some(a) = self.action_of(Device::Gamepad, b) {
                bits |= bit(a);
            }
        }
        bits
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..1.0).contains(&self.stick_deadzone) {
            return Err("stick_deadzone must be within 0..1".into());
        }
        for device in [Device::Keyboard, Device::Gamepad] {
            for (i, input) in self.slots(device).iter().enumerate() {
                let Some(input) = input else { continue };
                if self.slots(device)[..i].iter().any(|s| s.as_deref() == Some(input)) {
                    return Err(format!("{} {input} is bound twice", device.label()));
                }
                if device == Device::Keyboard && RESERVED_KEYS.contains(&input.as_str()) {
                    return Err(format!("{input} is a handoff window key"));
                }
            }
        }
        Ok(())
    }
}

/// `controls.json`: `{"version":1, "keyboard":{"CROSS":"X",...}, "gamepad":{...}, "stick_deadzone":0.2}`.
/// Actions left out are unbound.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    keyboard: std::collections::BTreeMap<String, String>,
    gamepad: std::collections::BTreeMap<String, String>,
    stick_deadzone: f32,
}

/// The per-user controls file (shared by every project: the bindings belong to the person playing).
pub fn default_path() -> Option<PathBuf> {
    crate::bios::data_dir().map(|d| d.join("controls.json"))
}

/// Read a controls file; `Ok(None)` when it does not exist.
pub fn load(path: &Path) -> Result<Option<Bindings>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let doc: Document = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if doc.version != 1 {
        return Err(format!("{}: unsupported controls version {}", path.display(), doc.version));
    }
    let mut b =
        Bindings { keyboard: Default::default(), gamepad: Default::default(), stick_deadzone: doc.stick_deadzone };
    for (device, map) in [(Device::Keyboard, &doc.keyboard), (Device::Gamepad, &doc.gamepad)] {
        for (action, input) in map {
            let i = ACTIONS
                .iter()
                .position(|a| a.eq_ignore_ascii_case(action))
                .ok_or_else(|| format!("{}: unknown button {action:?}", path.display()))?;
            b.slots_mut(device)[i] = Some(input.clone());
        }
    }
    b.validate().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(b))
}

pub fn save(path: &Path, b: &Bindings) -> Result<(), String> {
    b.validate()?;
    let map = |slots: &[Option<String>; 17]| {
        slots.iter().enumerate().filter_map(|(i, s)| s.as_ref().map(|s| (ACTIONS[i].to_string(), s.clone()))).collect()
    };
    let doc =
        Document { version: 1, keyboard: map(&b.keyboard), gamepad: map(&b.gamepad), stick_deadzone: b.stick_deadzone };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    crate::write_atomic(path, &serde_json::to_vec_pretty(&doc).unwrap()).map_err(|e| format!("{}: {e}", path.display()))
}

/// A frame's pad from held keys, gamepad buttons and the gamepad's sticks
/// (-1..1, +y up). A digital pad (or a DualShock the game has not switched
/// to analog) gets the left stick as the d-pad instead.
pub fn pad_state(b: &Bindings, analog: bool, keys: &[String], pad: &[String], sticks: [f32; 4]) -> (u32, [u8; 4]) {
    let mut buttons = b.buttons(keys.iter().map(String::as_str), pad.iter().map(String::as_str));
    let [lx, ly, rx, ry] = sticks;
    let dz = b.stick_deadzone;
    let sticks =
        [stick_byte(lx, dz, false), stick_byte(ly, dz, true), stick_byte(rx, dz, false), stick_byte(ry, dz, true)];
    if analog {
        (buttons, sticks)
    } else {
        buttons |= u32::from(stick_dpad(sticks[0], sticks[1]));
        (buttons, [0x80; 4])
    }
}

/// A stick axis (-1..1, +y up as gamepads report it) as a PS1 stick byte (0 = left/up, 255 = right/down).
pub fn stick_byte(v: f32, deadzone: f32, invert: bool) -> u8 {
    let v = if invert { -v } else { v };
    if !v.is_finite() || v.abs() < deadzone {
        return 0x80;
    }
    // Rescale past the deadzone so the full range is still reachable.
    let t = ((v.abs() - deadzone) / (1.0 - deadzone)).min(1.0) * v.signum();
    (127.5 + t * 127.5).round().clamp(0.0, 255.0) as u8
}

/// The d-pad bits a stick (bytes as [`stick_byte`] makes them) presses on a digital pad.
pub fn stick_dpad(x: u8, y: u8) -> u16 {
    use psx_core::sio0::button::*;
    let mut bits = 0;
    // Half travel, so a diagonal does not press both directions too early.
    if x < 0x40 {
        bits |= LEFT;
    } else if x > 0xC0 {
        bits |= RIGHT;
    }
    if y < 0x40 {
        bits |= UP;
    } else if y > 0xC0 {
        bits |= DOWN;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_cover_the_template_keys() {
        let b = Bindings::default();
        b.validate().unwrap();
        assert_eq!(b.action_of(Device::Keyboard, "X"), Some(4));
        assert_eq!(b.buttons(["X", "Up"], ["East"]), bit(4) | bit(0) | bit(5));
    }

    #[test]
    fn assigning_a_used_input_swaps() {
        let mut b = Bindings::default();
        // CROSS takes C (CIRCLE's key); CIRCLE gets X.
        assert_eq!(b.assign(4, Device::Keyboard, "C").unwrap(), Some(5));
        assert_eq!(b.keyboard[4].as_deref(), Some("C"));
        assert_eq!(b.keyboard[5].as_deref(), Some("X"));
        b.validate().unwrap();
        assert!(b.assign(4, Device::Keyboard, "F12").is_err());
        assert_eq!(b.assign(14, Device::Gamepad, "code:300").unwrap(), None);
    }

    #[test]
    fn file_round_trips() {
        let dir = std::env::temp_dir().join(format!("psx-controls-{}", std::process::id()));
        let path = dir.join("controls.json");
        let mut b = Bindings::default();
        b.clear(0, Device::Gamepad);
        b.stick_deadzone = 0.3;
        save(&path, &b).unwrap();
        assert_eq!(load(&path).unwrap(), Some(b));
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(load(Path::new("/nonexistent/controls.json")).unwrap(), None);
    }

    #[test]
    fn sticks_map_to_bytes_and_dpad() {
        assert_eq!(stick_byte(0.1, 0.2, false), 0x80);
        assert_eq!(stick_byte(1.0, 0.2, false), 255);
        assert_eq!(stick_byte(-1.0, 0.2, false), 0);
        // Gamepad up (+1) is PS1 up (0).
        assert_eq!(stick_byte(1.0, 0.2, true), 0);
        assert_eq!(stick_dpad(0, 0x80), psx_core::sio0::button::LEFT);
        assert_eq!(stick_dpad(0x80, 0x80), 0);
    }
}

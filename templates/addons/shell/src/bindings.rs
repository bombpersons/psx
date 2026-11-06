//! Which keyboard keys and gamepad buttons press which PS1 button, in the
//! same `controls.json` format and with the same defaults as `psx play`, so
//! the port plays with the keys the original was played with.
//!
//! Inputs are names: keyboard keys as `psx play` names them (`X`, `Up`,
//! `Enter`, `Key1`, `LeftShift`), gamepad buttons as gilrs does (`South`,
//! `DPadUp`), or `code:N` for one the gamepad database does not name.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// PS1 buttons in menu order, then the DualShock's ANALOG mode button.
pub const ACTIONS: [&str; 17] = [
    "UP", "DOWN", "LEFT", "RIGHT", "CROSS", "CIRCLE", "SQUARE", "TRIANGLE", "L1", "R1", "L2", "R2", "START", "SELECT",
    "L3", "R3", "ANALOG",
];

/// The ANALOG action's index.
pub const ANALOG: usize = 16;

/// Keys the shell and menus keep for themselves; they cannot be bound.
pub const RESERVED_KEYS: [&str; 14] =
    ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "Tab", "Escape"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Keyboard,
    Gamepad,
}

/// One optional input per action and device; no input presses two actions.
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

    /// Binds `input` to `action`. An action already using it gets this
    /// action's previous input (possibly none) instead, and is returned.
    pub fn assign(&mut self, action: usize, device: Device, input: &str) -> Result<Option<usize>, String> {
        if device == Device::Keyboard && RESERVED_KEYS.contains(&input) {
            return Err(format!("{input} is kept for the menus"));
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

    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..1.0).contains(&self.stick_deadzone) {
            return Err("stick_deadzone must be within 0..1".into());
        }
        for device in [Device::Keyboard, Device::Gamepad] {
            for (i, input) in self.slots(device).iter().enumerate() {
                let Some(input) = input else { continue };
                if self.slots(device)[..i].iter().any(|s| s.as_deref() == Some(input)) {
                    return Err(format!("{input} is bound twice"));
                }
                if device == Device::Keyboard && RESERVED_KEYS.contains(&input.as_str()) {
                    return Err(format!("{input} is kept for the menus"));
                }
            }
        }
        Ok(())
    }
}

/// `controls.json`: `{"version":1, "keyboard":{"CROSS":"X",...},
/// "gamepad":{...}, "stick_deadzone":0.2}`. Actions left out are unbound.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    keyboard: BTreeMap<String, String>,
    gamepad: BTreeMap<String, String>,
    stick_deadzone: f32,
}

/// Reads a controls file; `Ok(None)` when it does not exist.
pub fn load(path: &Path) -> Result<Option<Bindings>, String> {
    let Some(bytes) = crate::settings::read(path).map_err(|e| format!("{}: {e}", path.display()))? else {
        return Ok(None);
    };
    let doc: Document = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
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
    let json = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    crate::settings::write(path, &json).map_err(|e| format!("{}: {e}", path.display()))
}

/// A stick axis (-1..1, +y up as gamepads report it) as a PS1 stick byte
/// (0 left or up, 255 right or down), rescaled past the deadzone.
pub fn stick_byte(v: f32, deadzone: f32, invert: bool) -> u8 {
    let v = if invert { -v } else { v };
    if !v.is_finite() || v.abs() < deadzone {
        return 0x80;
    }
    let t = ((v.abs() - deadzone) / (1.0 - deadzone)).min(1.0) * v.signum();
    (127.5 + t * 127.5).round().clamp(0.0, 255.0) as u8
}

/// A winit key as `psx play` names it, or None for keys it has no name for.
pub fn key_name(code: winit::keyboard::KeyCode) -> Option<String> {
    use winit::keyboard::KeyCode as K;
    let name = match code {
        K::ShiftLeft => "LeftShift",
        K::ShiftRight => "RightShift",
        K::ControlLeft => "LeftCtrl",
        K::ControlRight => "RightCtrl",
        K::AltLeft => "LeftAlt",
        K::AltRight => "RightAlt",
        K::SuperLeft => "LeftSuper",
        K::SuperRight => "RightSuper",
        K::Quote => "Apostrophe",
        K::BracketLeft => "LeftBracket",
        K::BracketRight => "RightBracket",
        K::ContextMenu => "Menu",
        K::NumpadDecimal => "NumPadDot",
        K::NumpadDivide => "NumPadSlash",
        K::NumpadMultiply => "NumPadAsterisk",
        K::NumpadSubtract => "NumPadMinus",
        K::NumpadAdd => "NumPadPlus",
        K::NumpadEnter => "NumPadEnter",
        _ => {
            let debug = format!("{code:?}");
            if let Some(letter) = debug.strip_prefix("Key") {
                return Some(letter.to_string());
            }
            if let Some(digit) = debug.strip_prefix("Digit") {
                return Some(format!("Key{digit}"));
            }
            if let Some(arrow) = debug.strip_prefix("Arrow") {
                return Some(arrow.to_string());
            }
            if let Some(n) = debug.strip_prefix("Numpad").filter(|n| n.len() == 1) {
                return Some(format!("NumPad{n}"));
            }
            const PLAIN: [&str; 23] = [
                "Backquote",
                "Backslash",
                "Comma",
                "Equal",
                "Minus",
                "Period",
                "Semicolon",
                "Slash",
                "Backspace",
                "Delete",
                "End",
                "Enter",
                "Escape",
                "Home",
                "Insert",
                "PageDown",
                "PageUp",
                "Pause",
                "Space",
                "Tab",
                "NumLock",
                "CapsLock",
                "ScrollLock",
            ];
            let f_key = debug.strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=15).contains(&n)));
            return (PLAIN.contains(&debug.as_str()) || f_key).then_some(debug);
        }
    };
    Some(name.to_string())
}

/// A gilrs button as `psx play` names it: its name, or `code:N`.
pub fn button_name(button: gilrs::Button, code: gilrs::ev::Code) -> String {
    match button {
        gilrs::Button::Unknown => format!("code:{}", code.into_u32()),
        b => format!("{b:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode as K;

    #[test]
    fn defaults_are_valid() {
        Bindings::default().validate().unwrap();
    }

    #[test]
    fn assigning_a_used_input_swaps() {
        let mut b = Bindings::default();
        assert_eq!(b.assign(4, Device::Keyboard, "C").unwrap(), Some(5));
        assert_eq!(b.keyboard[5].as_deref(), Some("X"));
        assert!(b.assign(4, Device::Keyboard, "F1").is_err());
    }

    #[test]
    fn keys_are_named_as_psx_play_names_them() {
        let name = |k| key_name(k).unwrap();
        assert_eq!(name(K::KeyX), "X");
        assert_eq!(name(K::Digit1), "Key1");
        assert_eq!(name(K::ArrowUp), "Up");
        assert_eq!(name(K::ShiftRight), "RightShift");
        assert_eq!(name(K::Enter), "Enter");
        assert_eq!(name(K::Numpad7), "NumPad7");
        assert_eq!(name(K::F12), "F12");
        assert_eq!(name(K::Space), "Space");
    }

    #[test]
    fn file_round_trips() {
        let dir = std::env::temp_dir().join(format!("shell-controls-{}", std::process::id()));
        let path = dir.join("controls.json");
        let mut b = Bindings::default();
        b.clear(0, Device::Gamepad);
        save(&path, &b).unwrap();
        assert_eq!(load(&path).unwrap(), Some(b));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

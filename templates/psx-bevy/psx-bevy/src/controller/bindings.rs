//! Physical mappings and explicit, validated settings-file persistence.
use super::Button;
use crate::settings;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Reflect)]
pub enum BindingDevice {
    #[default]
    Keyboard,
    Gamepad,
    Mouse,
}
impl BindingDevice {
    pub const ALL: [Self; 3] = [Self::Keyboard, Self::Gamepad, Self::Mouse];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalBinding {
    Key(KeyCode),
    Pad(GamepadButton),
    Mouse(MouseButton),
}
impl PhysicalBinding {
    pub fn device(self) -> BindingDevice {
        match self {
            Self::Key(_) => BindingDevice::Keyboard,
            Self::Pad(_) => BindingDevice::Gamepad,
            Self::Mouse(_) => BindingDevice::Mouse,
        }
    }
    pub fn label(self) -> String {
        match self {
            Self::Key(key) => match key {
                KeyCode::ArrowUp => "UP".into(),
                KeyCode::ArrowDown => "DOWN".into(),
                KeyCode::ArrowLeft => "LEFT".into(),
                KeyCode::ArrowRight => "RIGHT".into(),
                KeyCode::ShiftRight => "R SHIFT".into(),
                KeyCode::ShiftLeft => "L SHIFT".into(),
                KeyCode::Enter => "ENTER".into(),
                KeyCode::Space => "SPACE".into(),
                other => format!("{other:?}").trim_start_matches("Key").trim_start_matches("Digit").to_uppercase(),
            },
            Self::Pad(button) => match button {
                GamepadButton::LeftTrigger => "L BUMPER".into(),
                GamepadButton::RightTrigger => "R BUMPER".into(),
                GamepadButton::LeftTrigger2 => "L TRIGGER".into(),
                GamepadButton::RightTrigger2 => "R TRIGGER".into(),
                other => format!("{other:?}").to_uppercase(),
            },
            Self::Mouse(button) => match button {
                MouseButton::Left => "L CLICK".into(),
                MouseButton::Right => "R CLICK".into(),
                MouseButton::Middle => "M CLICK".into(),
                MouseButton::Back => "MOUSE 4".into(),
                MouseButton::Forward => "MOUSE 5".into(),
                MouseButton::Other(n) => format!("MOUSE {n}"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Reject,
    Swap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingConflict {
    pub action: Button,
    pub other: Button,
    pub binding: PhysicalBinding,
}
impl std::fmt::Display for BindingConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is already assigned to {}", self.binding.label(), self.other.label())
    }
}
impl std::error::Error for BindingConflict {}

/// One optional physical binding per logical action and device. Use `assign`
/// instead of mutating slots so a device button never silently triggers two actions.
#[derive(Resource, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerBindings {
    keyboard: [Option<KeyCode>; 14],
    gamepad: [Option<GamepadButton>; 14],
    /// Unbound by default; missing from profiles saved before mouse bindings.
    #[serde(default)]
    mouse: [Option<MouseButton>; 14],
    stick_deadzone: f32,
}
impl Default for ControllerBindings {
    fn default() -> Self {
        Self {
            keyboard: [
                KeyCode::ArrowUp,
                KeyCode::ArrowDown,
                KeyCode::ArrowLeft,
                KeyCode::ArrowRight,
                KeyCode::KeyX,
                KeyCode::KeyC,
                KeyCode::KeyZ,
                KeyCode::KeyS,
                KeyCode::KeyQ,
                KeyCode::KeyE,
                KeyCode::Digit1,
                KeyCode::Digit3,
                KeyCode::Enter,
                KeyCode::ShiftRight,
            ]
            .map(Some),
            gamepad: [
                GamepadButton::DPadUp,
                GamepadButton::DPadDown,
                GamepadButton::DPadLeft,
                GamepadButton::DPadRight,
                GamepadButton::South,
                GamepadButton::East,
                GamepadButton::West,
                GamepadButton::North,
                GamepadButton::LeftTrigger,
                GamepadButton::RightTrigger,
                GamepadButton::LeftTrigger2,
                GamepadButton::RightTrigger2,
                GamepadButton::Start,
                GamepadButton::Select,
            ]
            .map(Some),
            mouse: [None; 14],
            stick_deadzone: 0.2,
        }
    }
}
impl ControllerBindings {
    pub fn key(&self, action: Button) -> Option<KeyCode> {
        self.keyboard[action as usize]
    }
    pub fn pad_button(&self, action: Button) -> Option<GamepadButton> {
        self.gamepad[action as usize]
    }
    pub fn mouse_button(&self, action: Button) -> Option<MouseButton> {
        self.mouse[action as usize]
    }
    pub fn binding(&self, action: Button, device: BindingDevice) -> Option<PhysicalBinding> {
        match device {
            BindingDevice::Keyboard => self.key(action).map(PhysicalBinding::Key),
            BindingDevice::Gamepad => self.pad_button(action).map(PhysicalBinding::Pad),
            BindingDevice::Mouse => self.mouse_button(action).map(PhysicalBinding::Mouse),
        }
    }
    pub fn stick_deadzone(&self) -> f32 {
        self.stick_deadzone
    }
    pub fn set_stick_deadzone(&mut self, value: f32) -> Result<(), String> {
        if !(0.0..1.0).contains(&value) {
            return Err("Stick deadzone must be finite and within 0..1 (exclusive end)".into());
        }
        self.stick_deadzone = value;
        Ok(())
    }
    pub fn conflict(&self, action: Button, binding: PhysicalBinding) -> Option<BindingConflict> {
        Button::ALL
            .into_iter()
            .find(|&other| other != action && self.binding(other, binding.device()) == Some(binding))
            .map(|other| BindingConflict { action, other, binding })
    }
    /// Reject leaves every slot untouched. Swap gives the other action this
    /// action's previous binding (possibly unbound), preserving uniqueness.
    pub fn assign(
        &mut self,
        action: Button,
        binding: PhysicalBinding,
        policy: ConflictPolicy,
    ) -> Result<(), BindingConflict> {
        if let Some(conflict) = self.conflict(action, binding) {
            if policy == ConflictPolicy::Reject {
                return Err(conflict);
            }
            match binding {
                PhysicalBinding::Key(_) => self.keyboard.swap(action as usize, conflict.other as usize),
                PhysicalBinding::Pad(_) => self.gamepad.swap(action as usize, conflict.other as usize),
                PhysicalBinding::Mouse(_) => self.mouse.swap(action as usize, conflict.other as usize),
            }
        } else {
            match binding {
                PhysicalBinding::Key(key) => self.keyboard[action as usize] = Some(key),
                PhysicalBinding::Pad(button) => self.gamepad[action as usize] = Some(button),
                PhysicalBinding::Mouse(button) => self.mouse[action as usize] = Some(button),
            }
        }
        Ok(())
    }
    pub fn clear(&mut self, action: Button, device: BindingDevice) {
        match device {
            BindingDevice::Keyboard => self.keyboard[action as usize] = None,
            BindingDevice::Gamepad => self.gamepad[action as usize] = None,
            BindingDevice::Mouse => self.mouse[action as usize] = None,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..1.0).contains(&self.stick_deadzone) {
            return Err("Invalid stick deadzone".into());
        }
        for device in BindingDevice::ALL {
            for action in Button::ALL {
                if let Some(binding) = self.binding(action, device)
                    && let Some(conflict) = self.conflict(action, binding)
                {
                    return Err(conflict.to_string());
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    bindings: ControllerBindings,
}

/// File operations are explicit. Loading a missing file keeps defaults; a bad
/// file is reported and preserved until the player explicitly chooses Save.
#[derive(Resource, Debug)]
pub struct BindingStore {
    pub path: Option<PathBuf>,
    pub load_error: Option<String>,
}
impl BindingStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, load_error: None }
    }
    /// `<config dir>/<app>/controls.json`; `app` is usually the game's package
    /// name, so each game keeps its own bindings.
    pub fn default_path(app: &str) -> Option<PathBuf> {
        settings::config_path(app, "controls.json")
    }
    pub fn load(&self) -> Result<Option<ControllerBindings>, String> {
        let Some(path) = &self.path else { return Ok(None) };
        let Some(bytes) = settings::read(path)? else { return Ok(None) };
        let document: Document = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if document.version != 1 {
            return Err("Unsupported controls settings version".into());
        }
        document.bindings.validate()?;
        Ok(Some(document.bindings))
    }
    pub fn save(&self, bindings: &ControllerBindings) -> Result<(), String> {
        bindings.validate()?;
        let path = self.path.as_ref().ok_or("No controls settings path configured")?;
        let bytes = serde_json::to_vec_pretty(&Document { version: 1, bindings: bindings.clone() })
            .map_err(|e| e.to_string())?;
        settings::write(path, &bytes)
    }
}

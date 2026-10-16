//! Physical mappings and explicit, validated settings-file persistence.
use super::Button;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Reflect)]
pub enum BindingDevice {
    #[default]
    Keyboard,
    Gamepad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalBinding {
    Key(KeyCode),
    Pad(GamepadButton),
}
impl PhysicalBinding {
    pub fn device(self) -> BindingDevice {
        match self {
            Self::Key(_) => BindingDevice::Keyboard,
            Self::Pad(_) => BindingDevice::Gamepad,
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
    pub fn binding(&self, action: Button, device: BindingDevice) -> Option<PhysicalBinding> {
        match device {
            BindingDevice::Keyboard => self.key(action).map(PhysicalBinding::Key),
            BindingDevice::Gamepad => self.pad_button(action).map(PhysicalBinding::Pad),
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
            }
        } else {
            match binding {
                PhysicalBinding::Key(key) => self.keyboard[action as usize] = Some(key),
                PhysicalBinding::Pad(button) => self.gamepad[action as usize] = Some(button),
            }
        }
        Ok(())
    }
    pub fn clear(&mut self, action: Button, device: BindingDevice) {
        match device {
            BindingDevice::Keyboard => self.keyboard[action as usize] = None,
            BindingDevice::Gamepad => self.gamepad[action as usize] = None,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..1.0).contains(&self.stick_deadzone) {
            return Err("Invalid stick deadzone".into());
        }
        for device in [BindingDevice::Keyboard, BindingDevice::Gamepad] {
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
    pub fn default_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        if let Some(p) = std::env::var_os("APPDATA") {
            return Some(PathBuf::from(p).join(env!("CARGO_PKG_NAME")).join("controls.json"));
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
        Some(base.join(env!("CARGO_PKG_NAME")).join("controls.json"))
    }
    pub fn load(&self) -> Result<Option<ControllerBindings>, String> {
        let Some(path) = &self.path else { return Ok(None) };
        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("Controls settings exceed 64 KiB".into());
        }
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
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp =
            parent.join(format!(".controls-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| e.to_string())?;
        struct Staged(PathBuf);
        impl Drop for Staged {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _staged = Staged(temp.clone());
        let bytes = serde_json::to_vec_pretty(&Document { version: 1, bindings: bindings.clone() })
            .map_err(|e| e.to_string())?;
        let written = file.write_all(&bytes).and_then(|_| file.sync_all());
        drop(file);
        written.map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())
    }
}

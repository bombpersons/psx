//! DualShock-style input. Game logic reads [`ControllerState`]; edit device
//! mappings in [`ControllerBindings`].
use bevy::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "dev-mcp", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "dev-mcp", serde(rename_all = "lowercase"))]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Cross,
    Circle,
    Square,
    Triangle,
    L1,
    R1,
    L2,
    R2,
    Start,
    Select,
}

impl Button {
    pub const ALL: [Self; 14] = [
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::Cross,
        Self::Circle,
        Self::Square,
        Self::Triangle,
        Self::L1,
        Self::R1,
        Self::L2,
        Self::R2,
        Self::Start,
        Self::Select,
    ];
    pub fn mask(self) -> u16 {
        1 << self as u8
    }
}

/// Input sampled once per simulation tick. Edges compare consecutive ticks.
#[derive(Resource, Default, Debug, Clone)]
pub struct ControllerState {
    held: u16,
    previous: u16,
    /// Analog sticks in -1..=1, y up. Zero inside the deadzone.
    pub left_stick: Vec2,
    pub right_stick: Vec2,
}

impl ControllerState {
    pub fn update(&mut self, held: u16, left_stick: Vec2, right_stick: Vec2) {
        self.previous = self.held;
        self.held = held;
        self.left_stick = left_stick;
        self.right_stick = right_stick;
    }
    pub fn held(&self, button: Button) -> bool {
        self.held & button.mask() != 0
    }
    pub fn pressed(&self, button: Button) -> bool {
        (self.held & !self.previous) & button.mask() != 0
    }
    #[allow(dead_code)] // template API; this game only uses held/pressed
    pub fn released(&self, button: Button) -> bool {
        (!self.held & self.previous) & button.mask() != 0
    }
    /// D-pad as a vector (x right, y up), merged with the left stick when larger.
    #[allow(dead_code)] // template API; Croc reads the D-pad buttons directly
    pub fn direction(&self) -> Vec2 {
        let axis = |pos, neg| f32::from(u8::from(self.held(pos))) - f32::from(u8::from(self.held(neg)));
        let pad = Vec2::new(axis(Button::Right, Button::Left), axis(Button::Up, Button::Down));
        if self.left_stick.length_squared() > pad.length_squared() { self.left_stick } else { pad }
    }
    #[cfg(feature = "dev-mcp")]
    pub fn buttons(&self) -> Vec<Button> {
        Button::ALL.into_iter().filter(|b| self.held(*b)).collect()
    }
}

#[derive(Resource)]
pub struct ControllerBindings {
    pub keyboard: [KeyCode; 14],
    pub gamepad: [GamepadButton; 14],
    pub stick_deadzone: f32,
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
            ],
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
            ],
            stick_deadzone: 0.2,
        }
    }
}

fn deadzone(v: Vec2, dz: f32) -> Vec2 {
    if v.length() < dz { Vec2::ZERO } else { v.clamp_length_max(1.0) }
}

/// Physical buttons and sticks from the keyboard and one gamepad.
pub fn sample(keys: &ButtonInput<KeyCode>, pad: Option<&Gamepad>, bindings: &ControllerBindings) -> (u16, Vec2, Vec2) {
    let mut bits = 0;
    for (i, button) in Button::ALL.into_iter().enumerate() {
        if keys.pressed(bindings.keyboard[i]) || pad.is_some_and(|p| p.pressed(bindings.gamepad[i])) {
            bits |= button.mask();
        }
    }
    let (left, right) = pad.map_or((Vec2::ZERO, Vec2::ZERO), |p| {
        (deadzone(p.left_stick(), bindings.stick_deadzone), deadzone(p.right_stick(), bindings.stick_deadzone))
    });
    (bits, left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_simulation_ticks() {
        let mut pad = ControllerState::default();
        pad.update(Button::Cross.mask(), Vec2::ZERO, Vec2::ZERO);
        assert!(pad.pressed(Button::Cross) && pad.held(Button::Cross));
        pad.update(Button::Cross.mask(), Vec2::ZERO, Vec2::ZERO);
        assert!(!pad.pressed(Button::Cross));
        pad.update(0, Vec2::ZERO, Vec2::ZERO);
        assert!(pad.released(Button::Cross) && !pad.held(Button::Cross));
    }

    #[test]
    fn keyboard_defaults() {
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::KeyX);
        keys.press(KeyCode::ArrowRight);
        let (bits, ..) = sample(&keys, None, &ControllerBindings::default());
        assert_eq!(bits, Button::Cross.mask() | Button::Right.mask());
    }

    #[test]
    fn direction_prefers_larger_input() {
        let mut pad = ControllerState::default();
        pad.update(Button::Left.mask(), Vec2::new(0.3, 0.0), Vec2::ZERO);
        assert_eq!(pad.direction(), Vec2::new(-1.0, 0.0));
    }
}

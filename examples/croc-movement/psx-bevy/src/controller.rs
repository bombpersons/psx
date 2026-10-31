//! DualShock-style input. Game logic reads [`ControllerState`]; edit device
//! mappings through [`ControllerBindings`]. [`crate::controls`] supplies an optional menu.
//! The mouse comes alongside, in [`ControllerState::mouse`], for games that
//! add mouse controls; it is not mapped onto the pad.
use bevy::prelude::*;
mod bindings;
pub use bindings::{BindingConflict, BindingDevice, BindingStore, ConflictPolicy, ControllerBindings, PhysicalBinding};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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
    pub fn label(self) -> &'static str {
        [
            "Up", "Down", "Left", "Right", "Cross", "Circle", "Square", "Triangle", "L1", "R1", "L2", "R2", "Start",
            "Select",
        ][self as usize]
    }
    /// The bits of several buttons, as [`ControllerState::update`] takes them.
    pub fn mask_all(buttons: &[Button]) -> u16 {
        buttons.iter().fold(0, |bits, button| bits | button.mask())
    }
}

/// The mouse for one logic tick. Buttons follow the pad's rules (edges
/// compare ticks, a click shorter than a tick is held for one tick); motion
/// and wheel add up everything since the previous tick.
#[derive(Default, Debug, Clone, Copy, PartialEq, Reflect)]
pub struct MouseState {
    held: u8,
    previous: u8,
    /// The cursor on the PS1 screen at the tick, in screen pixels (origin
    /// top-left, y down); `None` outside it. See [`crate::pointer`].
    pub position: Option<Vec2>,
    /// Raw device motion (y down), not limited by the window: mouse look.
    pub motion: Vec2,
    /// Wheel lines, positive away from the player.
    pub wheel: f32,
}

impl MouseState {
    /// `held` is [`crate::pointer::mouse_bits`] of the buttons down this tick.
    pub fn update(&mut self, held: u8, position: Option<Vec2>, motion: Vec2, wheel: f32) {
        *self = Self { held, previous: self.held, position, motion, wheel };
    }
    pub fn held(&self, button: MouseButton) -> bool {
        self.held & crate::pointer::mouse_bit(button) != 0
    }
    pub fn pressed(&self, button: MouseButton) -> bool {
        (self.held & !self.previous) & crate::pointer::mouse_bit(button) != 0
    }
    pub fn released(&self, button: MouseButton) -> bool {
        (!self.held & self.previous) & crate::pointer::mouse_bit(button) != 0
    }
    /// The held buttons, by [`crate::pointer::MOUSE_BUTTONS`] name.
    pub fn button_names(&self) -> Vec<&'static str> {
        crate::pointer::mouse_button_names(self.held)
    }
    /// The cursor in a default `Camera2d`'s world units on a screen of
    /// `size`: origin at the centre, y up, one unit per pixel.
    pub fn world_2d(&self, size: UVec2) -> Option<Vec2> {
        self.position.map(|p| Vec2::new(p.x - size.x as f32 / 2.0, size.y as f32 / 2.0 - p.y))
    }
}

/// Input sampled once per logic tick. Edges compare consecutive ticks, and a
/// tap shorter than a tick still counts as held for one tick.
#[derive(Resource, Default, Debug, Clone, Reflect)]
#[reflect(Resource)]
pub struct ControllerState {
    held: u16,
    previous: u16,
    /// Analog sticks in -1..=1, y up. Zero inside the deadzone.
    pub left_stick: Vec2,
    pub right_stick: Vec2,
    /// The mouse, sampled on the same tick.
    pub mouse: MouseState,
}

impl ControllerState {
    /// Drop device edges when a modal interface takes input focus.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
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
    pub fn released(&self, button: Button) -> bool {
        (!self.held & self.previous) & button.mask() != 0
    }
    /// D-pad as a vector (x right, y up), merged with the left stick when larger.
    pub fn direction(&self) -> Vec2 {
        let axis = |pos, neg| f32::from(u8::from(self.held(pos))) - f32::from(u8::from(self.held(neg)));
        let pad = Vec2::new(axis(Button::Right, Button::Left), axis(Button::Up, Button::Down));
        if self.left_stick.length_squared() > pad.length_squared() { self.left_stick } else { pad }
    }
    /// The held buttons.
    pub fn buttons(&self) -> Vec<Button> {
        Button::ALL.into_iter().filter(|b| self.held(*b)).collect()
    }
}

fn deadzone(v: Vec2, dz: f32) -> Vec2 {
    if v.length() < dz { Vec2::ZERO } else { v.clamp_length_max(1.0) }
}

/// The actions pressed by the held mouse buttons `mouse` through their bindings.
pub fn mouse_actions(mouse: u8, bindings: &ControllerBindings) -> u16 {
    Button::ALL
        .into_iter()
        .filter(|b| bindings.mouse_button(*b).is_some_and(|m| mouse & crate::pointer::mouse_bit(m) != 0))
        .fold(0, |bits, b| bits | b.mask())
}

/// Physical buttons and sticks from the keyboard, one gamepad and the mouse
/// buttons bound to actions (`mouse` is [`crate::pointer::mouse_bits`]).
pub fn sample(
    keys: &ButtonInput<KeyCode>,
    pad: Option<&Gamepad>,
    mouse: u8,
    bindings: &ControllerBindings,
) -> (u16, Vec2, Vec2) {
    let mut bits = mouse_actions(mouse, bindings);
    for button in Button::ALL {
        if bindings.key(button).is_some_and(|key| keys.pressed(key))
            || pad.is_some_and(|p| bindings.pad_button(button).is_some_and(|button| p.pressed(button)))
        {
            bits |= button.mask();
        }
    }
    let (left, right) = pad.map_or((Vec2::ZERO, Vec2::ZERO), |p| {
        (deadzone(p.left_stick(), bindings.stick_deadzone()), deadzone(p.right_stick(), bindings.stick_deadzone()))
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
        let (bits, ..) = sample(&keys, None, 0, &ControllerBindings::default());
        assert_eq!(bits, Button::Cross.mask() | Button::Right.mask());
    }

    #[test]
    fn mouse_edges_are_ticks_and_world_2d_is_centred() {
        let left = crate::pointer::mouse_bit(MouseButton::Left);
        let mut mouse = MouseState::default();
        mouse.update(left, Some(Vec2::new(0.0, 0.0)), Vec2::new(3.0, 0.0), 0.0);
        assert!(mouse.pressed(MouseButton::Left) && mouse.held(MouseButton::Left));
        mouse.update(left, Some(Vec2::new(160.0, 120.0)), Vec2::ZERO, 0.0);
        assert!(!mouse.pressed(MouseButton::Left));
        assert_eq!(mouse.world_2d(UVec2::new(320, 240)), Some(Vec2::ZERO));
        mouse.update(0, Some(Vec2::new(0.0, 0.0)), Vec2::ZERO, 0.0);
        assert!(mouse.released(MouseButton::Left));
        assert_eq!(mouse.world_2d(UVec2::new(320, 240)), Some(Vec2::new(-160.0, 120.0)));
    }

    #[test]
    fn bound_mouse_buttons_press_actions() {
        let mut bindings = ControllerBindings::default();
        let (bits, ..) = sample(&ButtonInput::default(), None, crate::pointer::mouse_bit(MouseButton::Left), &bindings);
        assert_eq!(bits, 0, "the mouse is unbound by default");
        bindings.assign(Button::Cross, PhysicalBinding::Mouse(MouseButton::Left), ConflictPolicy::Reject).unwrap();
        let (bits, ..) = sample(&ButtonInput::default(), None, crate::pointer::mouse_bit(MouseButton::Left), &bindings);
        assert_eq!(bits, Button::Cross.mask());
        let clash = bindings.assign(Button::Circle, PhysicalBinding::Mouse(MouseButton::Left), ConflictPolicy::Reject);
        assert_eq!(clash.unwrap_err().other, Button::Cross, "one mouse button, one action");
    }

    #[test]
    fn direction_prefers_larger_input() {
        let mut pad = ControllerState::default();
        pad.update(Button::Left.mask(), Vec2::new(0.3, 0.0), Vec2::ZERO);
        assert_eq!(pad.direction(), Vec2::new(-1.0, 0.0));
    }
}

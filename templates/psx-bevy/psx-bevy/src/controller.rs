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

/// Both sticks at rest, as [`ControllerState::sticks`] holds them.
pub const CENTRED: [u8; 4] = [0x80; 4];

/// Input sampled once per logic tick. Edges compare consecutive ticks, and a
/// tap shorter than a tick still counts as held for one tick.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource)]
pub struct ControllerState {
    held: u16,
    previous: u16,
    /// The pad in analog mode: a DualShock sending its sticks. On the
    /// PlayStation the game or the player's ANALOG button switches it, and a
    /// game can act on the mode with the sticks at rest; a recording replays
    /// the mode the original read. Live, the pad is always analog.
    pub analog: bool,
    /// The analog sticks as the PlayStation's pad sends them: left x, left
    /// y, right x, right y, each 0 (left or up) to 255 (right or down),
    /// [`CENTRED`] at rest. A port reads these to apply the original's own
    /// deadzone and curve; a gamepad's deadzone applies before (see
    /// [`stick_byte`]).
    pub sticks: [u8; 4],
    /// The sticks as -1..=1, y up: a view of `sticks`.
    pub left_stick: Vec2,
    pub right_stick: Vec2,
    /// The mouse, sampled on the same tick.
    pub mouse: MouseState,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            held: 0,
            previous: 0,
            analog: true,
            sticks: CENTRED,
            left_stick: Vec2::ZERO,
            right_stick: Vec2::ZERO,
            mouse: MouseState::default(),
        }
    }
}

impl ControllerState {
    /// Drop device edges when a modal interface takes input focus.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// `sticks` as [`ControllerState::sticks`] holds them.
    pub fn update(&mut self, held: u16, sticks: [u8; 4]) {
        self.previous = self.held;
        self.held = held;
        self.sticks = sticks;
        let [lx, ly, rx, ry] = sticks.map(stick_axis);
        self.left_stick = Vec2::new(lx, -ly);
        self.right_stick = Vec2::new(rx, -ry);
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

/// A gamepad's stick axis (-1..=1, y up as gamepads report it) as a stick
/// byte: 0 left or up, 255 right or down, 128 inside the deadzone. `invert`
/// for y. `psx scenario play` maps a gamepad the same way.
pub fn stick_byte(v: f32, deadzone: f32, invert: bool) -> u8 {
    let v = if invert { -v } else { v };
    if !v.is_finite() || v.abs() < deadzone {
        return 0x80;
    }
    // Rescale past the deadzone so the full range is still reachable.
    let t = ((v.abs() - deadzone) / (1.0 - deadzone)).min(1.0) * v.signum();
    (127.5 + t * 127.5).round().clamp(0.0, 255.0) as u8
}

/// A stick byte as -1..=1 (0 is -1, 128 is 0, 255 is 1), y down as the byte has it.
pub fn stick_axis(byte: u8) -> f32 {
    let d = f32::from(byte) - 128.0;
    if d < 0.0 { d / 128.0 } else { d / 127.0 }
}

/// The actions pressed by the held mouse buttons `mouse` through their bindings.
pub fn mouse_actions(mouse: u8, bindings: &ControllerBindings) -> u16 {
    Button::ALL
        .into_iter()
        .filter(|b| bindings.mouse_button(*b).is_some_and(|m| mouse & crate::pointer::mouse_bit(m) != 0))
        .fold(0, |bits, b| bits | b.mask())
}

/// Physical buttons and sticks (as [`ControllerState::sticks`]) from the
/// keyboard, one gamepad and the mouse buttons bound to actions (`mouse` is
/// [`crate::pointer::mouse_bits`]).
pub fn sample(
    keys: &ButtonInput<KeyCode>,
    pad: Option<&Gamepad>,
    mouse: u8,
    bindings: &ControllerBindings,
) -> (u16, [u8; 4]) {
    let mut bits = mouse_actions(mouse, bindings);
    for button in Button::ALL {
        if bindings.key(button).is_some_and(|key| keys.pressed(key))
            || pad.is_some_and(|p| bindings.pad_button(button).is_some_and(|button| p.pressed(button)))
        {
            bits |= button.mask();
        }
    }
    let sticks = pad.map_or(CENTRED, |p| {
        let (l, r, dz) = (p.left_stick(), p.right_stick(), bindings.stick_deadzone());
        [stick_byte(l.x, dz, false), stick_byte(l.y, dz, true), stick_byte(r.x, dz, false), stick_byte(r.y, dz, true)]
    });
    (bits, sticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_simulation_ticks() {
        let mut pad = ControllerState::default();
        pad.update(Button::Cross.mask(), CENTRED);
        assert!(pad.pressed(Button::Cross) && pad.held(Button::Cross));
        pad.update(Button::Cross.mask(), CENTRED);
        assert!(!pad.pressed(Button::Cross));
        pad.update(0, CENTRED);
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
        pad.update(Button::Left.mask(), [166, 128, 128, 128]);
        assert_eq!(pad.direction(), Vec2::new(-1.0, 0.0));
    }

    #[test]
    fn sticks_keep_the_raw_bytes_with_floats_as_a_view() {
        let mut pad = ControllerState::default();
        assert_eq!((pad.sticks, pad.left_stick), (CENTRED, Vec2::ZERO));
        // Left stick full left and up; the right one at rest.
        pad.update(0, [0, 0, 128, 128]);
        assert_eq!((pad.left_stick, pad.right_stick), (Vec2::new(-1.0, 1.0), Vec2::ZERO));
        pad.update(0, [255, 255, 128, 128]);
        assert_eq!(pad.left_stick, Vec2::new(1.0, -1.0));
        // A gamepad maps to bytes past its deadzone, gamepad up to byte 0.
        assert_eq!(stick_byte(0.1, 0.2, false), 0x80);
        assert_eq!((stick_byte(1.0, 0.2, false), stick_byte(-1.0, 0.2, false)), (255, 0));
        assert_eq!(stick_byte(1.0, 0.2, true), 0);
    }
}

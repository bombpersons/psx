//! The mouse over the PS1 screen. [`Pointer`] is sampled every rendered frame
//! for menus and other UI; gameplay reads the per-tick copy in
//! [`crate::controller::ControllerState::mouse`]. Positions are screen pixels
//! of [`PsxScreen`], origin top-left and y down (Bevy UI's convention), so
//! they land on the same pixels whatever the window's size or letterbox.
use crate::psx::{PsxScreen, fit_scale};
use bevy::{
    input::mouse::{AccumulatedMouseScroll, MouseScrollUnit},
    prelude::*,
    window::PrimaryWindow,
};

/// One bit per mouse button, as [`Pointer`] and `MouseState` store them.
pub fn mouse_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::Back => 8,
        MouseButton::Forward => 16,
        MouseButton::Other(_) => 0,
    }
}

/// The mouse buttons tools and replays name, with their names.
pub const MOUSE_BUTTONS: [(MouseButton, &str); 5] = [
    (MouseButton::Left, "left"),
    (MouseButton::Right, "right"),
    (MouseButton::Middle, "middle"),
    (MouseButton::Back, "back"),
    (MouseButton::Forward, "forward"),
];

/// A mouse button by its [`MOUSE_BUTTONS`] name, ignoring case.
pub fn mouse_button_named(name: &str) -> Option<MouseButton> {
    MOUSE_BUTTONS.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(b, _)| *b)
}

/// The names of the buttons set in `bits`.
pub fn mouse_button_names(bits: u8) -> Vec<&'static str> {
    MOUSE_BUTTONS.iter().filter(|(b, _)| bits & mouse_bit(*b) != 0).map(|(_, n)| *n).collect()
}

/// The held mouse buttons as bits.
pub fn mouse_bits(buttons: &ButtonInput<MouseButton>) -> u8 {
    buttons.get_pressed().fold(0, |bits, b| bits | mouse_bit(*b))
}

/// Wheel movement in lines, positive away from the player. Pixel-precise
/// devices (touchpads) count 16 pixels as a line.
pub fn wheel_lines(scroll: &AccumulatedMouseScroll) -> f32 {
    match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 16.0,
    }
}

/// A cursor position in the window (logical pixels, top-left origin) as
/// screen pixels, through the same letterbox scale as the window display.
/// `None` over the black bars.
pub fn cursor_to_screen(cursor: Vec2, window: Vec2, screen: Vec2) -> Option<Vec2> {
    let scale = fit_scale(window, screen);
    if scale <= 0.0 {
        return None;
    }
    let at = (cursor - window / 2.0) / scale + screen / 2.0;
    (at.x >= 0.0 && at.y >= 0.0 && at.x < screen.x && at.y < screen.y).then_some(at)
}

/// The mouse this frame.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    /// Screen pixels, origin top-left, y down. `None` outside the screen.
    pub position: Option<Vec2>,
    /// The position changed this frame, so hover can follow the mouse
    /// without fighting keyboard selection while it rests.
    pub moved: bool,
    /// Wheel lines this frame, positive away from the player.
    pub wheel: f32,
    held: u8,
    previous: u8,
}

impl Pointer {
    /// A new frame's sample: `held` is [`mouse_bits`] of the buttons down now.
    pub fn update(&mut self, position: Option<Vec2>, held: u8, wheel: f32) {
        self.moved = position != self.position;
        self.position = position;
        self.previous = self.held;
        self.held = held;
        self.wheel = wheel;
    }
    pub fn held(&self, button: MouseButton) -> bool {
        self.held & mouse_bit(button) != 0
    }
    pub fn just_pressed(&self, button: MouseButton) -> bool {
        (self.held & !self.previous) & mouse_bit(button) != 0
    }
    pub fn just_released(&self, button: MouseButton) -> bool {
        (!self.held & self.previous) & mouse_bit(button) != 0
    }
    /// Any button held.
    pub fn any_held(&self) -> bool {
        self.held != 0
    }
    /// The held buttons as [`mouse_bits`] stores them.
    pub fn held_bits(&self) -> u8 {
        self.held
    }
}

/// Screen rectangles of a menu's clickable items, refreshed by its view from
/// UI layout every frame and read by its input logic, so the logic also runs
/// (and is tested) without the presentation. Later entries are more specific.
#[derive(Resource, Debug, Clone)]
pub struct Hitboxes<T: Send + Sync + 'static>(pub Vec<(Rect, T)>);

impl<T: Send + Sync + 'static> Default for Hitboxes<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: Copy + Send + Sync + 'static> Hitboxes<T> {
    /// The most specific item under `point`.
    pub fn at(&self, point: Vec2) -> Option<T> {
        self.0.iter().rev().find(|(rect, _)| rect.contains(point)).map(|(_, item)| *item)
    }
}

/// The screen rectangle of a laid-out UI node drawn into the PS1 canvas.
pub fn node_rect(node: &ComputedNode, transform: &bevy::ui::UiGlobalTransform) -> Rect {
    Rect::from_center_size(transform.translation, node.size())
}

/// Samples [`Pointer`] from the primary window.
pub(crate) fn track_pointer(
    windows: Query<&Window, With<PrimaryWindow>>,
    screen: Option<Res<PsxScreen>>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    mut pointer: ResMut<Pointer>,
) {
    let position = match (windows.single().ok(), screen) {
        (Some(window), Some(screen)) => {
            window.cursor_position().and_then(|cursor| cursor_to_screen(cursor, window.size(), screen.size().as_vec2()))
        }
        _ => None,
    };
    let held = buttons.map_or(0, |b| mouse_bits(&b));
    pointer.update(position, held, scroll.map_or(0.0, |s| wheel_lines(&s)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_maps_through_the_letterbox() {
        let screen = Vec2::new(320.0, 240.0);
        // 1600x900: scale 3.75, 200 px bars left and right.
        let window = Vec2::new(1600.0, 900.0);
        assert_eq!(cursor_to_screen(Vec2::new(800.0, 450.0), window, screen), Some(Vec2::new(160.0, 120.0)));
        assert_eq!(cursor_to_screen(Vec2::new(200.0, 0.0), window, screen), Some(Vec2::ZERO));
        assert_eq!(cursor_to_screen(Vec2::new(199.0, 450.0), window, screen), None, "left bar");
        assert_eq!(cursor_to_screen(Vec2::new(1400.0, 450.0), window, screen), None, "right edge");
        assert_eq!(cursor_to_screen(Vec2::ZERO, Vec2::ZERO, screen), None, "minimised");
    }

    #[test]
    fn pointer_edges_and_hitboxes() {
        let mut pointer = Pointer::default();
        pointer.update(Some(Vec2::new(5.0, 5.0)), mouse_bit(MouseButton::Left), 0.0);
        assert!(pointer.moved && pointer.just_pressed(MouseButton::Left) && pointer.held(MouseButton::Left));
        pointer.update(Some(Vec2::new(5.0, 5.0)), 0, 1.0);
        assert!(!pointer.moved && pointer.just_released(MouseButton::Left) && !pointer.any_held());
        let boxes = Hitboxes(vec![(Rect::new(0.0, 0.0, 100.0, 10.0), 1), (Rect::new(50.0, 0.0, 100.0, 10.0), 2)]);
        assert_eq!(boxes.at(Vec2::new(10.0, 5.0)), Some(1));
        assert_eq!(boxes.at(Vec2::new(60.0, 5.0)), Some(2), "later entries are more specific");
        assert_eq!(boxes.at(Vec2::new(60.0, 20.0)), None);
    }
}

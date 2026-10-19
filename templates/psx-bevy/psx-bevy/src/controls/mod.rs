//! Reusable example controls menu. Gameplay still consumes logical ControllerState.
//! Add ControlsPlugin beside ShellPlugin/PsxPlugin; call ControlsMenu::open from
//! your game's options screen. Persistence is explicit on Save and close.
#[cfg(test)]
mod tests;
mod view;

use crate::{
    controller::{
        BindingConflict, BindingDevice, BindingStore, Button, ConflictPolicy, ControllerBindings, PhysicalBinding,
    },
    pointer::{Hitboxes, Pointer},
    shell::{InputFocus, PhysicalInputSystems},
};
use bevy::prelude::*;
use std::path::PathBuf;

/// The starter installs this for the demo. Finished games choose where to open
/// it, or use the core binding/store API with their own presentation.
pub struct ControlsPlugin {
    /// Settings file; `None` keeps bindings in memory only. The launcher uses
    /// `BindingStore::default_path(<game package name>)`.
    pub path: Option<PathBuf>,
    pub initially_open: bool,
    /// Disable presentation for rules tests or a custom frontend.
    pub render_ui: bool,
    /// F1 and Start+Select shortcuts. Disable when your options screen owns opening.
    pub global_shortcuts: bool,
}
impl Default for ControlsPlugin {
    fn default() -> Self {
        Self { path: None, initially_open: false, render_ui: true, global_shortcuts: true }
    }
}
impl Plugin for ControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ControllerBindings>()
            .init_resource::<Defaults>()
            .init_resource::<ControlsMenu>()
            .init_resource::<InputFocus>()
            .init_resource::<Pointer>()
            .init_resource::<Hitboxes<Item>>()
            .insert_resource(BindingStore::new(self.path.clone()))
            .insert_resource(Shortcuts(self.global_shortcuts))
            .add_systems(Startup, load)
            .add_systems(
                PreUpdate,
                drive.in_set(PhysicalInputSystems::Capture).run_if(resource_exists::<ButtonInput<KeyCode>>),
            );
        if self.initially_open {
            app.world_mut().resource_mut::<ControlsMenu>().open();
        }
        if self.render_ui {
            view::plugin(app);
        }
    }
}

/// What a click or hover at a screen point means: a row, and for binding
/// rows the device column under it. The view fills [`Hitboxes<Item>`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub row: usize,
    pub device: Option<BindingDevice>,
}

#[derive(Resource)]
struct Defaults(ControllerBindings);
#[derive(Resource)]
struct Shortcuts(bool);
impl FromWorld for Defaults {
    fn from_world(world: &mut World) -> Self {
        Self(world.resource::<ControllerBindings>().clone())
    }
}

#[derive(Debug, Clone, Default)]
enum Mode {
    #[default]
    Browse,
    Listen {
        armed: bool,
        pad: Option<Entity>,
    },
    Conflict(BindingConflict),
}

/// Selection survives closing; edits are a draft until Save succeeds.
#[derive(Resource, Default)]
pub struct ControlsMenu {
    open: bool,
    pub selected: usize,
    pub device: BindingDevice,
    draft: Option<ControllerBindings>,
    mode: Mode,
    status: String,
}
impl ControlsMenu {
    pub fn open(&mut self) {
        self.open = true;
    }
    pub fn cancel(&mut self) {
        self.open = false;
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn draft(&self) -> Option<&ControllerBindings> {
        self.draft.as_ref()
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn select(&mut self, action: Button, device: BindingDevice) {
        self.selected = action as usize;
        self.device = device;
    }
    fn close(&mut self, focus: &mut InputFocus) {
        self.open = false;
        self.draft = None;
        self.mode = Mode::Browse;
        focus.captured = false;
        focus.wait_for_release = true;
    }
    fn begin_capture(&mut self, pad: Option<Entity>) {
        self.mode = Mode::Listen { armed: false, pad };
        self.status = "Release buttons, then press a new binding.".into();
    }
    /// After a binding is set, move to the next action (or Save after the last)
    /// so a whole layout can be entered with Enter and the new keys alone, even
    /// when the arrow keys arrive as unexpected codes (remote desktop sessions).
    fn advance(&mut self) {
        self.selected = if self.selected + 1 < RESTORE { self.selected + 1 } else { SAVE };
        let next = Button::ALL.get(self.selected).map_or("Save", |b| b.label());
        self.status = format!("{} Next: {next}", self.status);
    }
    fn candidate(&mut self, binding: PhysicalBinding) {
        let action = Button::ALL[self.selected];
        match self.draft.as_mut().unwrap().assign(action, binding, ConflictPolicy::Reject) {
            Ok(()) => {
                self.status = format!("{}: {} (unsaved).", action.label(), binding.label());
                self.mode = Mode::Browse;
                self.advance();
            }
            Err(conflict) => {
                self.status = format!("{}: {} already uses it. Swap?", binding.label(), conflict.other.label());
                self.mode = Mode::Conflict(conflict);
            }
        }
    }
    fn confirm_conflict(&mut self) {
        if let Mode::Conflict(conflict) = self.mode {
            self.draft.as_mut().unwrap().assign(conflict.action, conflict.binding, ConflictPolicy::Swap).unwrap();
            self.status = format!("Swapped {} and {} (unsaved).", conflict.action.label(), conflict.other.label());
            self.mode = Mode::Browse;
            self.advance();
        }
    }
}
fn load(mut store: ResMut<BindingStore>, mut bindings: ResMut<ControllerBindings>) {
    match store.load() {
        Ok(Some(loaded)) => *bindings = loaded,
        Ok(None) => {}
        Err(error) => {
            warn!("Controls settings were preserved: {error}");
            store.load_error = Some(error);
        }
    }
}

const RESTORE: usize = 14;
const SAVE: usize = 15;
const DISCARD: usize = 16;
const ROWS: usize = 17;

#[allow(clippy::too_many_arguments)]
fn drive(
    keys: Res<ButtonInput<KeyCode>>,
    pads: Query<(Entity, &Gamepad)>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut menu: ResMut<ControlsMenu>,
    mut focus: ResMut<InputFocus>,
    mut bindings: ResMut<ControllerBindings>,
    defaults: Res<Defaults>,
    mut store: ResMut<BindingStore>,
    shortcuts: Res<Shortcuts>,
    pointer: Res<Pointer>,
    hitboxes: Res<Hitboxes<Item>>,
) {
    let selected_pad = pads.iter().min_by_key(|(e, _)| e.to_bits());
    let pad = selected_pad.map(|(_, p)| p);
    let pad_entity = selected_pad.map(|(e, _)| e);
    let pressed = |b| pad.is_some_and(|p| p.just_pressed(b));
    let chord = pad.is_some_and(|p| {
        (p.just_pressed(GamepadButton::Start) && p.pressed(GamepadButton::Select))
            || (p.just_pressed(GamepadButton::Select) && p.pressed(GamepadButton::Start))
    });
    if shortcuts.0 && (keys.just_pressed(KeyCode::F1) || chord) {
        if menu.open {
            menu.close(&mut focus);
            return;
        } else if !focus.captured {
            // Not on top of another menu that owns input.
            menu.open();
        }
    }
    if !menu.open {
        if menu.draft.is_some() {
            menu.close(&mut focus);
        }
        return;
    }
    focus.captured = true;
    if windows.iter().any(|w| !w.focused) {
        menu.close(&mut focus);
        return;
    }
    if menu.draft.is_none() {
        menu.draft = Some(bindings.clone());
        menu.mode = Mode::Browse;
        menu.status = if store.load_error.is_some() {
            "Saved controls could not be loaded. Defaults kept.".into()
        } else {
            "Changes apply only after Save and close.".into()
        };
        // The opening key/chord must not navigate or become a binding.
        return;
    }
    menu.selected = menu.selected.min(ROWS - 1);
    // The mouse: hover selects, left click acts as Enter on a binding slot or
    // a command row (an action's name only selects it), right click as Esc,
    // the wheel moves the selection.
    let hovered = pointer.position.and_then(|p| hitboxes.at(p));
    let clicked = hovered.filter(|_| pointer.just_pressed(MouseButton::Left));
    let slot = |item: Item| item.device.filter(|_| item.row < RESTORE);
    // Capturing a mouse slot, every mouse button is a candidate: only Esc cancels.
    let mouse_capture = matches!(menu.mode, Mode::Listen { .. }) && menu.device == BindingDevice::Mouse;
    let right_click = pointer.just_pressed(MouseButton::Right) && !mouse_capture;
    if keys.just_pressed(KeyCode::Escape) || right_click {
        if matches!(menu.mode, Mode::Browse) {
            menu.close(&mut focus);
        } else {
            menu.mode = Mode::Browse;
            menu.status = "Binding cancelled.".into();
        }
        return;
    }
    match menu.mode.clone() {
        Mode::Listen { mut armed, pad: previous_pad } => {
            // A click elsewhere moves the capture to another slot, or cancels
            // it, rather than leaving the menu waiting unseen for a key.
            if let Some(item) = clicked.filter(|_| !mouse_capture) {
                match slot(item) {
                    Some(device) if (item.row, device) == (menu.selected, menu.device) => {}
                    Some(device) => {
                        (menu.selected, menu.device) = (item.row, device);
                        menu.begin_capture(pad_entity);
                        return;
                    }
                    None => {
                        menu.mode = Mode::Browse;
                        menu.status = "Binding cancelled.".into();
                        return;
                    }
                }
            }
            if previous_pad != pad_entity {
                armed = false;
            }
            let neutral = keys.get_pressed().next().is_none()
                && !pointer.any_held()
                && pad.is_none_or(|p| p.get_pressed().next().is_none());
            if !armed {
                armed = neutral;
                menu.mode = Mode::Listen { armed, pad: pad_entity };
                menu.status = if menu.device == BindingDevice::Gamepad && pad.is_none() {
                    "Connect a gamepad to assign its buttons.".into()
                } else if armed && menu.device == BindingDevice::Mouse {
                    "Click the mouse button to bind. Esc cancels.".into()
                } else if armed {
                    "Press a new binding. Esc or right click cancels.".into()
                } else {
                    "Release all keys and buttons first.".into()
                };
                return;
            }
            let candidates: Vec<_> = match menu.device {
                BindingDevice::Keyboard => keys.get_just_pressed().copied().map(PhysicalBinding::Key).collect(),
                BindingDevice::Gamepad => {
                    pad.map_or_else(Vec::new, |p| p.get_just_pressed().copied().map(PhysicalBinding::Pad).collect())
                }
                BindingDevice::Mouse => [
                    MouseButton::Left,
                    MouseButton::Right,
                    MouseButton::Middle,
                    MouseButton::Back,
                    MouseButton::Forward,
                ]
                .into_iter()
                .filter(|b| pointer.just_pressed(*b))
                .map(PhysicalBinding::Mouse)
                .collect(),
            };
            if candidates.len() == 1 {
                menu.candidate(candidates[0]);
            } else if candidates.len() > 1 {
                menu.mode = Mode::Listen { armed: false, pad: pad_entity };
                menu.status = "Press one key or button at a time.".into();
            }
        }
        Mode::Conflict(_) => {
            if keys.just_pressed(KeyCode::Enter) || pressed(GamepadButton::South) || clicked.is_some() {
                menu.confirm_conflict();
            } else if pressed(GamepadButton::East) {
                menu.mode = Mode::Browse;
                menu.status = "Kept previous bindings.".into();
            }
        }
        Mode::Browse => {
            if pressed(GamepadButton::East) {
                menu.close(&mut focus);
                return;
            }
            if let Some(item) = hovered.filter(|_| pointer.moved).or(clicked) {
                menu.selected = item.row;
                if let Some(device) = item.device {
                    menu.device = device;
                }
            }
            if pointer.wheel > 0.0 {
                menu.selected = (menu.selected + ROWS - 1) % ROWS;
            } else if pointer.wheel < 0.0 {
                menu.selected = (menu.selected + 1) % ROWS;
            }
            if keys.just_pressed(KeyCode::ArrowUp) || pressed(GamepadButton::DPadUp) {
                menu.selected = (menu.selected + ROWS - 1) % ROWS;
            }
            if keys.just_pressed(KeyCode::ArrowDown) || pressed(GamepadButton::DPadDown) {
                menu.selected = (menu.selected + 1) % ROWS;
            }
            // Left/right move between the device columns.
            let column = BindingDevice::ALL.iter().position(|d| *d == menu.device).unwrap_or(0);
            if keys.just_pressed(KeyCode::ArrowLeft) || pressed(GamepadButton::DPadLeft) {
                menu.device = BindingDevice::ALL[column.saturating_sub(1)];
            }
            if keys.just_pressed(KeyCode::ArrowRight) || pressed(GamepadButton::DPadRight) {
                menu.device = BindingDevice::ALL[(column + 1).min(BindingDevice::ALL.len() - 1)];
            }
            if menu.selected < RESTORE && (keys.just_pressed(KeyCode::Delete) || pressed(GamepadButton::West)) {
                let action = Button::ALL[menu.selected];
                let device = menu.device;
                menu.draft.as_mut().unwrap().clear(action, device);
                menu.status = format!("{} cleared (unsaved)", action.label());
            }
            let click = clicked.is_some_and(|item| slot(item).is_some() || item.row >= RESTORE);
            if keys.just_pressed(KeyCode::Enter) || pressed(GamepadButton::South) || click {
                match menu.selected {
                    RESTORE => {
                        menu.draft = Some(defaults.0.clone());
                        menu.status = "Defaults restored (unsaved).".into();
                    }
                    SAVE => match store.save(menu.draft.as_ref().unwrap()) {
                        Ok(()) => {
                            *bindings = menu.draft.as_ref().unwrap().clone();
                            store.load_error = None;
                            menu.close(&mut focus);
                        }
                        Err(error) => {
                            warn!("Could not save controls: {error}");
                            menu.status = "Could not save. Check the settings location.".into();
                        }
                    },
                    DISCARD => menu.close(&mut focus),
                    _ => menu.begin_capture(pad_entity),
                }
            }
        }
    }
}

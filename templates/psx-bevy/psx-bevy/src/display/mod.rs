//! The display settings menu, built like [`crate::controls`]: screen
//! resolution, [`PsxAspect`], borderless fullscreen and the [`PsxLook`] switches. Edits
//! preview live; Save writes `display.json`, Discard (or Esc) restores what
//! was showing when the menu opened. F2 opens it; F11 toggles fullscreen at
//! any time ([`fullscreen_shortcut`], added by the launcher).
//!
//! This module is the logic and does all input; `view.rs` is a built-in
//! PS1-style view. A game that draws its own (to merge it into its options
//! screen) sets [`DisplayUi`] and draws [`DisplayMenu`], [`Row::ALL`],
//! [`value`] and [`HINT`], filling [`Hitboxes<Target>`] for the mouse.
//!
//! Command-line flags (`--aspect`, `--fullscreen`, `--no-dither`, `--no-quantize`) win
//! over the saved file for that session, through [`DisplayOverrides`].
#[cfg(test)]
mod tests;
mod view;

use crate::{
    pointer::{Hitboxes, Pointer},
    psx::{PsxAspect, PsxLook, PsxScreen, width_for_aspect},
    settings,
    shell::{InputFocus, PhysicalInputSystems},
};
use bevy::{
    prelude::*,
    window::{MonitorSelection, PrimaryWindow, WindowMode},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The starter installs this for the demo beside [`crate::controls::ControlsPlugin`].
pub struct DisplayPlugin {
    /// Settings file; `None` keeps settings in memory only. The launcher uses
    /// `DisplayStore::default_path(<game package name>)`.
    pub path: Option<PathBuf>,
    /// Screen sizes the Resolution row offers, in order. The size the game
    /// starts with is always offered.
    pub screens: Vec<UVec2>,
    pub overrides: DisplayOverrides,
    pub initially_open: bool,
    /// The built-in view; the initial [`DisplayUi::view`].
    pub render_ui: bool,
    /// The F2 shortcut; the initial [`DisplayUi::shortcuts`].
    pub global_shortcuts: bool,
}
impl Default for DisplayPlugin {
    fn default() -> Self {
        Self {
            path: None,
            screens: Vec::new(),
            overrides: default(),
            initially_open: false,
            render_ui: true,
            global_shortcuts: true,
        }
    }
}
impl Plugin for DisplayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DisplayMenu>()
            .init_resource::<InputFocus>()
            .init_resource::<Pointer>()
            .init_resource::<Hitboxes<Target>>()
            .insert_resource(DisplayStore::new(self.path.clone()))
            .insert_resource(Options { screens: self.screens.clone(), overrides: self.overrides, defaults: None })
            .insert_resource(DisplayUi { view: self.render_ui, shortcuts: self.global_shortcuts })
            .add_systems(Startup, load)
            .add_systems(
                PreUpdate,
                drive.in_set(PhysicalInputSystems::Capture).run_if(resource_exists::<ButtonInput<KeyCode>>),
            );
        if self.initially_open {
            app.world_mut().resource_mut::<DisplayMenu>().open();
        }
        view::plugin(app);
    }
}

/// Session values from the command line; `Some` replaces the saved value.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DisplayOverrides {
    pub aspect: Option<PsxAspect>,
    pub fullscreen: Option<bool>,
    pub dither: Option<bool>,
    pub quantize: Option<bool>,
}

/// What `display.json` holds: a snapshot of the live [`PsxScreen`] size,
/// [`PsxAspect`], window mode and [`PsxLook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySettings {
    pub screen: [u32; 2],
    #[serde(default)]
    pub aspect: PsxAspect,
    pub fullscreen: bool,
    pub look: PsxLook,
}

/// The live resources a [`DisplaySettings`] reads and writes.
#[derive(bevy::ecs::system::SystemParam)]
pub struct LiveDisplay<'w, 's> {
    pub screen: ResMut<'w, PsxScreen>,
    pub aspect: ResMut<'w, PsxAspect>,
    pub look: ResMut<'w, PsxLook>,
    pub windows: Query<'w, 's, &'static mut Window, With<PrimaryWindow>>,
}
impl LiveDisplay<'_, '_> {
    pub fn settings(&self) -> DisplaySettings {
        DisplaySettings::current(&self.screen, *self.aspect, &self.look, self.windows.single().ok())
    }
    pub fn windowed(&self) -> bool {
        !self.windows.is_empty()
    }
    /// Writes only what differs, so unchanged resources keep their change ticks.
    pub fn apply(&mut self, settings: &DisplaySettings) {
        if self.screen.size() != UVec2::from(settings.screen) {
            self.screen.set_size(settings.screen.into());
        }
        if *self.aspect != settings.aspect {
            *self.aspect = settings.aspect;
        }
        if *self.look != settings.look {
            *self.look = settings.look;
        }
        if let Ok(mut window) = self.windows.single_mut()
            && is_fullscreen(&window) != settings.fullscreen
        {
            set_fullscreen(&mut window, settings.fullscreen);
        }
    }
}
impl DisplaySettings {
    /// Largest screen side accepted from a file.
    pub const MAX_SIDE: u32 = 4096;

    pub fn current(screen: &PsxScreen, aspect: PsxAspect, look: &PsxLook, window: Option<&Window>) -> Self {
        Self { screen: screen.size().into(), aspect, fullscreen: window.is_some_and(is_fullscreen), look: *look }
    }
    fn overridden(mut self, overrides: DisplayOverrides) -> Self {
        self.aspect = overrides.aspect.unwrap_or(self.aspect);
        self.fullscreen = overrides.fullscreen.unwrap_or(self.fullscreen);
        self.look.dither = overrides.dither.unwrap_or(self.look.dither);
        self.look.quantize = overrides.quantize.unwrap_or(self.look.quantize);
        self
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.screen.iter().any(|side| !(1..=Self::MAX_SIDE).contains(side)) {
            return Err(format!("Screen size must be 1..={} on each side", Self::MAX_SIDE));
        }
        Ok(())
    }
}

pub fn is_fullscreen(window: &Window) -> bool {
    !matches!(window.mode, WindowMode::Windowed)
}

/// Borderless fullscreen on the window's current monitor, or back to a window.
pub fn set_fullscreen(window: &mut Window, on: bool) {
    window.mode = if on { WindowMode::BorderlessFullscreen(MonitorSelection::Current) } else { WindowMode::Windowed };
}

/// F11 toggles borderless fullscreen. The launcher adds it for every window.
pub fn fullscreen_shortcut(keys: Res<ButtonInput<KeyCode>>, mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    if keys.just_pressed(KeyCode::F11) {
        for mut window in &mut windows {
            let on = !is_fullscreen(&window);
            set_fullscreen(&mut window, on);
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    display: DisplaySettings,
}

/// Explicit file operations, like [`crate::controller::BindingStore`]: a
/// missing file keeps the game's defaults; a bad one is reported and left
/// alone until the player saves.
#[derive(Resource, Debug)]
pub struct DisplayStore {
    pub path: Option<PathBuf>,
    pub load_error: Option<String>,
}
impl DisplayStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, load_error: None }
    }
    /// `<config dir>/<app>/display.json`, beside controls.json.
    pub fn default_path(app: &str) -> Option<PathBuf> {
        settings::config_path(app, "display.json")
    }
    pub fn load(&self) -> Result<Option<DisplaySettings>, String> {
        let Some(path) = &self.path else { return Ok(None) };
        let Some(bytes) = settings::read(path)? else { return Ok(None) };
        let document: Document = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if document.version != 1 {
            return Err("Unsupported display settings version".into());
        }
        document.display.validate()?;
        Ok(Some(document.display))
    }
    pub fn save(&self, display: &DisplaySettings) -> Result<(), String> {
        display.validate()?;
        let path = self.path.as_ref().ok_or("No display settings path configured")?;
        let bytes =
            serde_json::to_vec_pretty(&Document { version: 1, display: *display }).map_err(|e| e.to_string())?;
        settings::write(path, &bytes)
    }
}

#[derive(Resource)]
struct Options {
    screens: Vec<UVec2>,
    overrides: DisplayOverrides,
    /// What the game set up before the file was applied: Restore defaults.
    defaults: Option<DisplaySettings>,
}
/// How the display menu is presented. Insert or change it before the app
/// starts, e.g. in the launcher's `add` closure:
/// `app.insert_resource(DisplayUi { view: false, shortcuts: false })`.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayUi {
    /// Draw the built-in PS1-style view. Off when the game draws its own.
    pub view: bool,
    /// F2 and its pad chord open and close the menu. Off when the game's own
    /// options screen opens it with [`DisplayMenu::open`].
    pub shortcuts: bool,
}

impl Default for DisplayUi {
    fn default() -> Self {
        Self { view: true, shortcuts: true }
    }
}

/// The menu's key help, three lines.
pub const HINT: &str = "Up/Down or mouse: select\nLeft/Right or click < >: change\nEsc/East or right click: discard";

/// Common aspect ratios by name; others show only the size.
pub fn aspect(size: [u32; 2]) -> &'static str {
    let ratio = size[0] as f32 / size[1].max(1) as f32;
    [(4.0 / 3.0, "4:3"), (16.0 / 9.0, "16:9"), (16.0 / 10.0, "16:10"), (3.0 / 2.0, "3:2"), (21.0 / 9.0, "21:9")]
        .into_iter()
        .find(|(r, _)| (ratio / r - 1.0).abs() < 0.01)
        .map_or("", |(_, name)| name)
}

/// A setting row's value as the menu shows it (`""` for command rows).
/// `windowed` is false when headless, where window settings don't apply.
pub fn value(row: Row, settings: &DisplaySettings, windowed: bool) -> String {
    let on = |b: bool| if b { "ON" } else { "OFF" }.to_string();
    match row {
        Row::Resolution => format!("{}x{} {}", settings.screen[0], settings.screen[1], aspect(settings.screen)),
        Row::Aspect if !windowed => "N/A".into(),
        Row::Aspect => match settings.aspect {
            PsxAspect::Fixed => "FIXED".into(),
            PsxAspect::Window => "MATCH WINDOW".into(),
        },
        Row::Fullscreen if !windowed => "N/A".into(),
        Row::Fullscreen => on(settings.fullscreen),
        Row::VertexSnap => on(settings.look.vertex_snap),
        Row::Dither => on(settings.look.dither),
        Row::Quantize => on(settings.look.quantize),
        _ => String::new(),
    }
}



/// The rows, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Resolution,
    Aspect,
    Fullscreen,
    VertexSnap,
    Dither,
    Quantize,
    Restore,
    Save,
    Discard,
}
impl Row {
    pub const ALL: [Self; 9] = [
        Self::Resolution,
        Self::Aspect,
        Self::Fullscreen,
        Self::VertexSnap,
        Self::Dither,
        Self::Quantize,
        Self::Restore,
        Self::Save,
        Self::Discard,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Resolution => "Resolution",
            Self::Aspect => "Aspect",
            Self::Fullscreen => "Fullscreen",
            Self::VertexSnap => "Vertex wobble",
            Self::Dither => "Dither",
            Self::Quantize => "15-bit colour",
            Self::Restore => "Restore defaults",
            Self::Save => "Save and close",
            Self::Discard => "Discard and close",
        }
    }
    /// Rows with a value that Left/Right change.
    pub fn is_setting(self) -> bool {
        (self as usize) < Self::Restore as usize
    }
}

/// What the mouse is over: a row, or one of a setting's `<` `>` arrows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Row(Row),
    /// Step the row's value forwards (`>`) or backwards (`<`).
    Step(Row, bool),
}
impl Target {
    pub fn row(self) -> Row {
        match self {
            Self::Row(row) | Self::Step(row, _) => row,
        }
    }
}

/// Selection survives closing. While open, the live resources are the draft.
#[derive(Resource, Default)]
pub struct DisplayMenu {
    open: bool,
    pub selected: usize,
    /// What was showing on opening: Discard puts it back.
    before: Option<DisplaySettings>,
    status: String,
}
impl DisplayMenu {
    pub fn open(&mut self) {
        self.open = true;
    }
    /// Closes as Discard does.
    pub fn cancel(&mut self) {
        self.open = false;
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn status(&self) -> &str {
        &self.status
    }
    pub fn select(&mut self, row: Row) {
        self.selected = row as usize;
    }
    pub fn row(&self) -> Row {
        Row::ALL[self.selected.min(Row::ALL.len() - 1)]
    }
    fn close(&mut self, focus: &mut InputFocus) {
        self.open = false;
        self.before = None;
        focus.captured = false;
        focus.wait_for_release = true;
    }
}

/// `settings` with `row` stepped one choice forwards or backwards. `window`
/// is the window's size, `None` when headless.
fn step(
    settings: DisplaySettings,
    row: Row,
    forward: bool,
    screens: &[UVec2],
    window: Option<Vec2>,
) -> DisplaySettings {
    let mut next = settings;
    let cycle = |at: usize, n: usize| if forward { (at + 1) % n } else { (at + n - 1) % n };
    let matching = |height: u32| screens.iter().copied().find(|s| s.y == height).unwrap_or(screens[0]);
    match (row, window) {
        (Row::Resolution, Some(window)) if settings.aspect == PsxAspect::Window => {
            // Only the height is chosen; the width follows the window.
            let mut heights: Vec<u32> = screens.iter().map(|s| s.y).collect();
            heights.dedup();
            let at = heights.iter().position(|h| *h == settings.screen[1]).unwrap_or(0);
            let height = heights[cycle(at, heights.len())];
            next.screen = [width_for_aspect(height, window), height];
        }
        (Row::Resolution, _) => {
            let at = screens.iter().position(|s| *s == UVec2::from(settings.screen)).unwrap_or(0);
            next.screen = screens[cycle(at, screens.len())].into();
        }
        (Row::Aspect, Some(window)) => match settings.aspect {
            PsxAspect::Fixed => {
                next.aspect = PsxAspect::Window;
                next.screen[0] = width_for_aspect(settings.screen[1], window);
            }
            PsxAspect::Window => {
                next.aspect = PsxAspect::Fixed;
                next.screen = matching(settings.screen[1]).into();
            }
        },
        (Row::Fullscreen, Some(_)) => next.fullscreen = !settings.fullscreen,
        (Row::VertexSnap, _) => next.look.vertex_snap = !settings.look.vertex_snap,
        (Row::Dither, _) => next.look.dither = !settings.look.dither,
        (Row::Quantize, _) => next.look.quantize = !settings.look.quantize,
        _ => {}
    }
    next
}

/// Records the game's defaults, then applies the saved file and the
/// command-line overrides on top.
fn load(mut store: ResMut<DisplayStore>, mut options: ResMut<Options>, mut live: LiveDisplay) {
    let defaults = live.settings();
    options.defaults = Some(defaults);
    if !options.screens.contains(&defaults.screen.into()) {
        options.screens.insert(0, defaults.screen.into());
    }
    let saved = match store.load() {
        Ok(saved) => saved,
        Err(error) => {
            warn!("Display settings were preserved: {error}");
            store.load_error = Some(error);
            None
        }
    };
    let settings = saved.unwrap_or(defaults).overridden(options.overrides);
    live.apply(&settings);
}

fn revert(menu: &DisplayMenu, live: &mut LiveDisplay) {
    if let Some(before) = menu.before {
        live.apply(&before);
    }
}

#[allow(clippy::too_many_arguments)]
fn drive(
    keys: Res<ButtonInput<KeyCode>>,
    pads: Query<(Entity, &Gamepad)>,
    mut menu: ResMut<DisplayMenu>,
    mut focus: ResMut<InputFocus>,
    options: Res<Options>,
    store: Res<DisplayStore>,
    ui: Res<DisplayUi>,
    pointer: Res<Pointer>,
    hitboxes: Res<Hitboxes<Target>>,
    mut live: LiveDisplay,
) {
    let pad = pads.iter().min_by_key(|(e, _)| e.to_bits()).map(|(_, p)| p);
    let pressed = |b| pad.is_some_and(|p| p.just_pressed(b));
    if ui.shortcuts && keys.just_pressed(KeyCode::F2) {
        if menu.open {
            revert(&menu, &mut live);
            menu.close(&mut focus);
            return;
        } else if !focus.captured {
            // Not on top of another menu that owns input.
            menu.open();
        }
    }
    if !menu.open {
        if menu.before.is_some() {
            revert(&menu, &mut live);
            menu.close(&mut focus);
        }
        return;
    }
    focus.captured = true;
    let current = live.settings();
    if menu.before.is_none() {
        menu.before = Some(current);
        menu.status = if store.load_error.is_some() {
            "Saved display settings could not be loaded.".into()
        } else {
            "Changes preview now; Save keeps them.".into()
        };
        // The opening key must not also change a setting.
        return;
    }
    menu.selected = menu.selected.min(Row::ALL.len() - 1);
    if keys.just_pressed(KeyCode::Escape) || pressed(GamepadButton::East) || pointer.just_pressed(MouseButton::Right) {
        revert(&menu, &mut live);
        menu.close(&mut focus);
        return;
    }
    let n = Row::ALL.len();
    if keys.just_pressed(KeyCode::ArrowUp) || pressed(GamepadButton::DPadUp) {
        menu.selected = (menu.selected + n - 1) % n;
    }
    if keys.just_pressed(KeyCode::ArrowDown) || pressed(GamepadButton::DPadDown) {
        menu.selected = (menu.selected + 1) % n;
    }
    // The mouse: hover selects, the wheel moves, left click on `<` or `>`
    // steps that way and on the rest of a row acts as Enter.
    let hovered = pointer.position.and_then(|p| hitboxes.at(p));
    let clicked = hovered.filter(|_| pointer.just_pressed(MouseButton::Left));
    if let Some(target) = hovered.filter(|_| pointer.moved).or(clicked) {
        menu.select(target.row());
    }
    if pointer.wheel > 0.0 {
        menu.selected = (menu.selected + n - 1) % n;
    } else if pointer.wheel < 0.0 {
        menu.selected = (menu.selected + 1) % n;
    }
    let left = keys.just_pressed(KeyCode::ArrowLeft)
        || pressed(GamepadButton::DPadLeft)
        || matches!(clicked, Some(Target::Step(_, false)));
    let right = keys.just_pressed(KeyCode::ArrowRight)
        || pressed(GamepadButton::DPadRight)
        || matches!(clicked, Some(Target::Step(_, true)));
    let confirm =
        keys.just_pressed(KeyCode::Enter) || pressed(GamepadButton::South) || matches!(clicked, Some(Target::Row(_)));
    let row = menu.row();
    let next = if row.is_setting() && (left || right || confirm) {
        if matches!(row, Row::Aspect | Row::Fullscreen) && !live.windowed() {
            menu.status = "Needs a window.".into();
        }
        let window = live.windows.single().ok().map(|w| w.size());
        step(current, row, !left, &options.screens, window)
    } else if confirm {
        match row {
            Row::Restore => {
                menu.status = "Defaults restored (unsaved).".into();
                options.defaults.unwrap_or(current)
            }
            Row::Save => {
                // Without a settings file the session keeps the changes.
                match store.path.as_ref().map_or(Ok(()), |_| store.save(&current)) {
                    Ok(()) => menu.close(&mut focus),
                    Err(error) => {
                        warn!("Could not save display settings: {error}");
                        menu.status = "Could not save. Check the settings location.".into();
                    }
                }
                return;
            }
            _ => {
                revert(&menu, &mut live);
                menu.close(&mut focus);
                return;
            }
        }
    } else {
        current
    };
    if next != current {
        live.apply(&next);
    }
}

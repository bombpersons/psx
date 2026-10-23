//! The editor for the engine's formats: menu bar, outline, view, inspector
//! and status bar, drawn with egui on an ordinary Bevy window. It is a modern
//! desktop tool, not a PS1 game: no psx-bevy look, font or pad menus.
//!
//! The template edits [`LevelData`] only. Grow it with the engine: when a
//! format lands in `engine/`, the editor learns to create, open, show and save
//! it (see `AGENTS.md`).
//!
//! Agents use it through its MCP server ([`mcp`]): reading and viewing tools,
//! editing tools that work like the menus (each call one undo step), and, in
//! developer builds, mouse and keyboard for testing the editor itself.
pub mod document;
mod files;
pub mod mcp;
mod panels;

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::window::WindowCloseRequested;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use document::{Document, History, Selection};
use engine::prelude::*;
use std::path::PathBuf;

/// The whole editor: add it to an app with `DefaultPlugins`.
pub struct EditorPlugin {
    /// A file to open at startup.
    pub open: Option<PathBuf>,
}

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        let mut status = Status::default();
        let document = match &self.open {
            Some(path) => Document::open(path).unwrap_or_else(|e| {
                status.0 = format!("Could not open: {e}");
                Document::new_level()
            }),
            None => Document::new_level(),
        };
        if let Some(path) = &document.path {
            status.0 = format!("Opened {}", path.display());
        }
        app.add_plugins(EguiPlugin::default())
            .insert_resource(document)
            .insert_resource(status)
            .init_resource::<History>()
            .init_resource::<Selection>()
            .init_resource::<Layout>()
            .init_resource::<panels::ViewState>()
            .init_resource::<Dialogs>()
            .init_resource::<Screen>()
            .add_systems(Startup, |mut commands: Commands| {
                commands.spawn(Camera2d);
            })
            .add_systems(Update, (close_requested, window_title))
            .add_systems(EguiPrimaryContextPass, ui);
    }
}

/// The message on the left of the status bar.
#[derive(Resource, Default)]
pub struct Status(pub String);

/// Where the panels were drawn last frame and whether the person is part way
/// through a gesture, for the MCP's answers.
#[derive(Resource, Default)]
struct Screen {
    /// Frames drawn so far.
    frames: u64,
    panels: Vec<(&'static str, egui::Rect)>,
    /// A widget is being dragged.
    dragging: bool,
}

/// Which panels the View menu shows.
#[derive(Resource)]
pub struct Layout {
    pub outline: bool,
    pub inspector: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self { outline: true, inspector: true }
    }
}

/// Open modal dialogs, and what to do once unsaved changes are dealt with.
#[derive(Resource, Default)]
struct Dialogs {
    open: Option<files::OpenDialog>,
    save_as: Option<String>,
    /// Waiting on "discard unsaved changes?".
    confirm: Option<Pending>,
}

/// An action that would lose unsaved changes.
#[derive(Clone, Debug)]
enum Pending {
    New,
    Open(PathBuf),
    Quit,
}

/// Something a menu, shortcut or dialog asked for, applied after the frame's
/// widgets so it never mixes with an in-progress inspector edit.
#[derive(Clone, Debug)]
enum Command {
    New,
    Open,
    OpenPath(PathBuf),
    Save,
    SaveAs,
    SaveTo(PathBuf),
    Quit,
    Undo,
    Redo,
    Delete,
    Duplicate,
    AddObject,
    AddTrigger,
    ResetView,
    /// Runs the pending action without asking again.
    Discard(Pending),
}

mod shortcuts {
    use bevy_egui::egui::{Key, KeyboardShortcut, Modifiers};
    const fn cmd(key: Key) -> KeyboardShortcut {
        KeyboardShortcut::new(Modifiers::COMMAND, key)
    }
    pub const NEW: KeyboardShortcut = cmd(Key::N);
    pub const OPEN: KeyboardShortcut = cmd(Key::O);
    pub const SAVE: KeyboardShortcut = cmd(Key::S);
    pub const SAVE_AS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);
    pub const UNDO: KeyboardShortcut = cmd(Key::Z);
    pub const REDO: KeyboardShortcut = cmd(Key::Y);
    pub const REDO_SHIFT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);
    pub const DUPLICATE: KeyboardShortcut = cmd(Key::D);
    pub const DELETE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Delete);
    pub const QUIT: KeyboardShortcut = cmd(Key::Q);
}

fn shortcut_commands(ctx: &egui::Context) -> Vec<Command> {
    use shortcuts::*;
    // Longer shortcuts first, so Ctrl+Shift+S is not taken as Ctrl+S.
    let mut table = vec![(SAVE_AS, Command::SaveAs), (REDO_SHIFT, Command::Redo), (NEW, Command::New)];
    table.extend([(OPEN, Command::Open), (SAVE, Command::Save), (QUIT, Command::Quit)]);
    // While a text field has focus, its own undo, delete and so on win.
    if !ctx.egui_wants_keyboard_input() {
        table.extend([
            (UNDO, Command::Undo),
            (REDO, Command::Redo),
            (DUPLICATE, Command::Duplicate),
            (DELETE, Command::Delete),
        ]);
    }
    ctx.input_mut(|input| table.into_iter().filter(|(s, _)| input.consume_shortcut(s)).map(|(_, c)| c).collect())
}

#[allow(clippy::too_many_arguments)]
fn ui(
    mut contexts: EguiContexts,
    mut document: ResMut<Document>,
    mut history: ResMut<History>,
    mut selection: ResMut<Selection>,
    mut layout: ResMut<Layout>,
    mut view: ResMut<panels::ViewState>,
    mut dialogs: ResMut<Dialogs>,
    mut status: ResMut<Status>,
    mut screen: ResMut<Screen>,
    mut agents: Option<ResMut<mcp::Agents>>,
    mut exit: MessageWriter<AppExit>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    let mut commands = shortcut_commands(&ctx);
    let before = document.level.clone();
    *selection = selection.valid(&document.level);

    let mut root = egui::Ui::new(
        ctx.clone(),
        "editor".into(),
        egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.viewport_rect()),
    );
    let mut drawn = Vec::new();
    let menu = egui::Panel::top("menu_bar").show(&mut root, |ui| {
        egui::MenuBar::new().ui(ui, |ui| menu_bar(ui, &history, &mut layout, agents.as_deref_mut(), &mut commands));
    });
    drawn.push(("menu bar", menu.response.rect));
    let bar = egui::Panel::bottom("status_bar").show(&mut root, |ui| status_bar(ui, &document, &status));
    drawn.push(("status bar", bar.response.rect));
    if layout.outline {
        let panel = egui::Panel::left("outline")
            .resizable(true)
            .default_size(220.0)
            .show(&mut root, |ui| panels::outline(ui, &document.level, &mut selection));
        drawn.push(("outline", panel.response.rect));
    }
    if layout.inspector {
        let panel = egui::Panel::right("inspector")
            .resizable(true)
            .default_size(280.0)
            .show(&mut root, |ui| panels::inspector(ui, &mut document.level, *selection));
        drawn.push(("inspector", panel.response.rect));
    }
    egui::CentralPanel::default()
        .show(&mut root, |ui| panels::view(ui, &mut document.level, &mut selection, &mut view));
    drawn.push(("view", view.rect));
    *screen = Screen { frames: screen.frames + 1, panels: drawn, dragging: ctx.dragged_id().is_some() };

    if let Some(agents) = agents.as_deref_mut() {
        mcp::agents_window(&ctx, agents);
    }
    let dialogs = &mut *dialogs;
    files::dialogs(&ctx, &mut dialogs.open, &mut dialogs.save_as, &mut commands);
    if let Some(pending) = dialogs.confirm.clone() {
        confirm_discard(&ctx, &document, pending, &mut dialogs.confirm, &mut commands);
    }

    // Direct edits from the inspector and view: one undo step per gesture.
    if document.level != before {
        let ongoing = ctx.dragged_id().is_some() || ctx.memory(|m| m.focused().is_some());
        history.record(before, ongoing);
    } else if !ctx.dragged_id().is_some() {
        history.end_gesture();
    }

    for command in commands {
        apply(command, &mut document, &mut history, &mut selection, &mut view, dialogs, &mut status, &mut exit);
    }
    Ok(())
}

fn menu_bar(
    ui: &mut egui::Ui,
    history: &History,
    layout: &mut Layout,
    agents: Option<&mut mcp::Agents>,
    out: &mut Vec<Command>,
) {
    let item = |ui: &mut egui::Ui, label: &str, shortcut: Option<&egui::KeyboardShortcut>, enabled: bool| {
        let mut button = egui::Button::new(label);
        if let Some(shortcut) = shortcut {
            button = button.shortcut_text(ui.ctx().format_shortcut(shortcut));
        }
        ui.add_enabled(enabled, button).clicked()
    };
    ui.menu_button("File", |ui| {
        let entries = [
            ("New", Some(&shortcuts::NEW), true, Command::New),
            ("Open…", Some(&shortcuts::OPEN), true, Command::Open),
            ("Save", Some(&shortcuts::SAVE), true, Command::Save),
            ("Save As…", Some(&shortcuts::SAVE_AS), true, Command::SaveAs),
        ];
        for (label, shortcut, enabled, command) in entries {
            if item(ui, label, shortcut, enabled) {
                out.push(command);
            }
        }
        ui.separator();
        if item(ui, "Quit", Some(&shortcuts::QUIT), true) {
            out.push(Command::Quit);
        }
    });
    ui.menu_button("Edit", |ui| {
        if item(ui, "Undo", Some(&shortcuts::UNDO), history.can_undo()) {
            out.push(Command::Undo);
        }
        if item(ui, "Redo", Some(&shortcuts::REDO), history.can_redo()) {
            out.push(Command::Redo);
        }
        ui.separator();
        if item(ui, "Add object", None, true) {
            out.push(Command::AddObject);
        }
        if item(ui, "Add trigger", None, true) {
            out.push(Command::AddTrigger);
        }
        if item(ui, "Duplicate", Some(&shortcuts::DUPLICATE), true) {
            out.push(Command::Duplicate);
        }
        if item(ui, "Delete", Some(&shortcuts::DELETE), true) {
            out.push(Command::Delete);
        }
    });
    ui.menu_button("View", |ui| {
        ui.checkbox(&mut layout.outline, "Outline");
        ui.checkbox(&mut layout.inspector, "Inspector");
        ui.separator();
        if item(ui, "Reset view", None, true) {
            out.push(Command::ResetView);
        }
    });
    if let Some(agents) = agents {
        ui.menu_button("Tools", |ui| {
            if ui.button("Agents…").on_hover_text("Let AI agents connect and help build the level").clicked() {
                agents.window = true;
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| mcp::agents_indicator(ui, agents));
    }
}

fn status_bar(ui: &mut egui::Ui, document: &Document, status: &Status) {
    ui.horizontal(|ui| {
        ui.label(&status.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let path = document.path.as_ref().map_or("Untitled".into(), |p| p.display().to_string());
            ui.label(path);
            ui.separator();
            ui.label(format!("{} objects, {} trigger(s)", document.level.objects.len(), document.level.triggers.len()));
            if document.is_dirty() {
                ui.separator();
                ui.label("Unsaved");
            }
        });
    });
}

fn confirm_discard(
    ctx: &egui::Context,
    document: &Document,
    pending: Pending,
    confirm: &mut Option<Pending>,
    out: &mut Vec<Command>,
) {
    let response = egui::Modal::new(egui::Id::new("confirm_discard")).show(ctx, |ui| {
        ui.heading("Unsaved changes");
        ui.label(format!(
            "{} has unsaved changes.",
            document.path.as_ref().map_or("This level".into(), |p| p.display().to_string())
        ));
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                out.push(Command::Save);
                if document.path.is_some() {
                    out.push(Command::Discard(pending.clone()));
                }
                *confirm = None;
            }
            if ui.button("Discard").clicked() {
                out.push(Command::Discard(pending.clone()));
                *confirm = None;
            }
            if ui.button("Cancel").clicked() {
                *confirm = None;
            }
        });
    });
    if response.should_close() {
        *confirm = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn apply(
    command: Command,
    document: &mut Document,
    history: &mut History,
    selection: &mut Selection,
    view: &mut panels::ViewState,
    dialogs: &mut Dialogs,
    status: &mut Status,
    exit: &mut MessageWriter<AppExit>,
) {
    let dirty = document.is_dirty();
    match command {
        Command::New if dirty => dialogs.confirm = Some(Pending::New),
        Command::New => run(Pending::New, document, history, selection, status, exit),
        Command::Open => dialogs.open = Some(files::OpenDialog::scan()),
        Command::OpenPath(path) if dirty => dialogs.confirm = Some(Pending::Open(path)),
        Command::OpenPath(path) => run(Pending::Open(path), document, history, selection, status, exit),
        Command::Quit if dirty => dialogs.confirm = Some(Pending::Quit),
        Command::Quit => run(Pending::Quit, document, history, selection, status, exit),
        Command::Discard(pending) => run(pending, document, history, selection, status, exit),
        Command::Save if document.path.is_none() => dialogs.save_as = Some(files::default_save_path(document)),
        Command::Save => status.0 = save(document, None),
        Command::SaveAs => dialogs.save_as = Some(files::default_save_path(document)),
        Command::SaveTo(path) => status.0 = save(document, Some(path)),
        Command::Undo => {
            let done = history.undo(&mut document.level);
            status.0 = if done { "Undo".into() } else { "Nothing to undo".into() };
        }
        Command::Redo => {
            let done = history.redo(&mut document.level);
            status.0 = if done { "Redo".into() } else { "Nothing to redo".into() };
        }
        Command::Delete => edit(document, history, selection, |level, sel| {
            document::delete(level, sel);
            Selection::None
        }),
        Command::Duplicate => edit(document, history, selection, document::duplicate),
        Command::AddObject => edit(document, history, selection, |level, _| {
            level.objects.push(Placement { type_id: ObjectTypeId(1), position: Vec3::ZERO, yaw: 0.0, params: vec![] });
            Selection::Object(level.objects.len() - 1)
        }),
        Command::AddTrigger => edit(document, history, selection, |level, _| {
            level.triggers.push(document::new_trigger());
            Selection::Trigger(level.triggers.len() - 1)
        }),
        Command::ResetView => *view = default(),
    }
}

/// An edit made by a command: always its own undo step.
fn edit(
    document: &mut Document,
    history: &mut History,
    selection: &mut Selection,
    change: impl FnOnce(&mut LevelData, Selection) -> Selection,
) {
    let before = document.level.clone();
    *selection = change(&mut document.level, *selection);
    if document.level != before {
        history.record(before, false);
    }
}

/// Runs an action that replaces the document, once unsaved changes are dealt with.
fn run(
    pending: Pending,
    document: &mut Document,
    history: &mut History,
    selection: &mut Selection,
    status: &mut Status,
    exit: &mut MessageWriter<AppExit>,
) {
    let path = match pending {
        Pending::New => None,
        Pending::Open(path) => Some(path),
        Pending::Quit => {
            exit.write(AppExit::Success);
            return;
        }
    };
    status.0 = load(path.as_deref(), document, history, selection).unwrap_or_else(|e| format!("Could not open: {e}"));
}

/// Replaces the document with `path`, or a new level, and forgets the old
/// one's history. Returns what the status bar should say.
fn load(
    path: Option<&std::path::Path>,
    document: &mut Document,
    history: &mut History,
    selection: &mut Selection,
) -> Result<String, String> {
    let (opened, said) = match path {
        Some(path) => (Document::open(path)?, format!("Opened {}", path.display())),
        None => (Document::new_level(), "New level".into()),
    };
    *document = opened;
    history.clear();
    *selection = Selection::None;
    Ok(said)
}

fn save(document: &mut Document, path: Option<PathBuf>) -> String {
    match document.save(path.as_deref()) {
        Ok(path) => format!("Saved {}", path.display()),
        Err(e) => format!("Could not save: {e}"),
    }
}

/// The window's close button asks about unsaved changes like File > Quit.
fn close_requested(
    mut requests: MessageReader<WindowCloseRequested>,
    document: Res<Document>,
    mut dialogs: ResMut<Dialogs>,
    mut exit: MessageWriter<AppExit>,
) {
    if requests.read().count() > 0 {
        if document.is_dirty() {
            dialogs.confirm = Some(Pending::Quit);
        } else {
            exit.write(AppExit::Success);
        }
    }
}

fn window_title(document: Res<Document>, mut window: Single<&mut Window>) {
    let title = document.title();
    if window.title != title {
        window.title = title;
    }
}

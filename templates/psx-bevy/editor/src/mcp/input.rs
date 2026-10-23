//! The MCP's testing tools (the `dev-mcp` feature, stdio only): mouse and
//! keyboard played into egui a frame at a time, the widgets egui drew, found by
//! label, and the window's size. While they are served only injected input
//! reaches egui, so someone moving the mouse over the window changes nothing
//! and a test sees the same editor every run.
//!
//! Input goes to egui as events sent straight to its context, with egui's own
//! readers of Bevy's input switched off. When a panel reads Bevy's input itself
//! (a 3D view's camera, say), write Bevy's keyboard and mouse messages in
//! `inject` too, so tests reach it the same way.
use super::{Reply, Response};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::egui::{self, accesskit};
use bevy_egui::input::{EguiInputEvent, FocusedNonWindowEguiContext, ModifierKeysState};
use bevy_egui::{EguiContext, EguiFullOutput, EguiGlobalSettings, EguiInputSet, EguiPostUpdateSet, PrimaryEguiContext};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;

/// What the testing tools ask of the main thread.
pub(super) enum Command {
    Input(Vec<Step>),
    Widgets(WidgetArgs),
    /// Resize the window (logical pixels).
    Window([u32; 2]),
}

#[derive(Resource, Default)]
pub(super) struct Pending(pub Vec<(Command, Reply)>);

pub(super) fn build(app: &mut App) {
    let input = &mut app.world_mut().resource_mut::<EguiGlobalSettings>().input_system_settings;
    input.run_write_modifiers_keys_state_system = false;
    input.run_write_window_pointer_moved_messages = false;
    input.run_write_pointer_button_messages = false;
    input.run_write_mouse_wheel_messages_system = false;
    input.run_write_keyboard_input_messages_system = false;
    input.run_write_ime_messages_system = false;
    input.run_write_window_touch_messages_system = false;
    input.run_write_non_window_touch_messages_system = false;
    input.run_write_non_window_pointer_moved_messages_system = false;
    input.run_write_pinch_gesture_messages_system = false;
    input.run_write_file_dnd_messages_system = false;
    app.init_resource::<Script>()
        .init_resource::<Widgets>()
        .init_resource::<Pending>()
        .add_systems(PreUpdate, (focus, take, inject).chain().after(super::serve).before(EguiInputSet::InitReading))
        .add_systems(
            PostUpdate,
            read_widgets.after(EguiPostUpdateSet::EndPass).before(EguiPostUpdateSet::ProcessOutput),
        );
}

/// egui takes keyboard input as if its window had focus (headless there is no
/// focus), and fills in its accessibility tree for `editor_widgets`.
fn focus(
    mut commands: Commands,
    mut context: Query<(Entity, &mut EguiContext), With<PrimaryEguiContext>>,
    mut enabled: Local<bool>,
) {
    if let Ok((entity, mut ctx)) = context.single_mut() {
        commands.insert_resource(FocusedNonWindowEguiContext(entity));
        if !*enabled {
            ctx.get_mut().enable_accesskit();
            *enabled = true;
        }
    }
}

/// `editor_input`'s arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputArgs {
    steps: Vec<Step>,
}

/// One step of `editor_input`. Each becomes one or more frames of events.
#[derive(Deserialize, Debug)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Step {
    /// Move the pointer, over `frames` frames (1 jumps).
    Move {
        to: [f32; 2],
        #[serde(default = "one")]
        frames: u32,
    },
    Click {
        at: [f32; 2],
        #[serde(default)]
        button: Button,
        #[serde(default)]
        with: Vec<String>,
    },
    DoubleClick {
        at: [f32; 2],
        #[serde(default)]
        button: Button,
    },
    Press {
        #[serde(default)]
        button: Button,
        at: Option<[f32; 2]>,
    },
    Release {
        #[serde(default)]
        button: Button,
        at: Option<[f32; 2]>,
    },
    Drag {
        from: [f32; 2],
        to: [f32; 2],
        #[serde(default)]
        button: Button,
        #[serde(default = "ten")]
        frames: u32,
        #[serde(default)]
        with: Vec<String>,
    },
    /// Lines; positive scrolls up (zooms the view in).
    Wheel {
        lines: f32,
        at: Option<[f32; 2]>,
    },
    /// A key or chord ("Ctrl+Z", "Tab", "Shift+F5"), pressed and released.
    Key {
        key: String,
    },
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
    /// Typed text, for the focused text field.
    Text {
        text: String,
    },
    Wait {
        frames: u32,
    },
}

fn one() -> u32 {
    1
}

fn ten() -> u32 {
    10
}

#[derive(Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

impl Button {
    fn egui(self) -> egui::PointerButton {
        match self {
            Button::Left => egui::PointerButton::Primary,
            Button::Right => egui::PointerButton::Secondary,
            Button::Middle => egui::PointerButton::Middle,
        }
    }
}

/// A key by name: a modifier (Ctrl, Shift, Alt), a letter or digit, or one of
/// egui's key names (Escape, Tab, Enter, Space, Backspace, Delete, ArrowUp,
/// F1...).
#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyName {
    Ctrl,
    Shift,
    Alt,
    Key(egui::Key),
}

fn key_name(name: &str) -> Result<KeyName, String> {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "cmd" => return Ok(KeyName::Ctrl),
        "shift" => return Ok(KeyName::Shift),
        "alt" => return Ok(KeyName::Alt),
        _ => {}
    }
    let key = egui::Key::ALL.iter().find(|k| k.name().eq_ignore_ascii_case(name));
    key.map(|k| KeyName::Key(*k)).ok_or_else(|| format!("unknown key {name}"))
}

/// "Ctrl+Shift+Z": the modifiers, then the key.
fn chord(text: &str) -> Result<Vec<KeyName>, String> {
    let keys = text.split('+').map(str::trim).filter(|k| !k.is_empty()).map(key_name).collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() { Err("empty key".into()) } else { Ok(keys) }
}

/// One event of a frame of injected input.
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    To(egui::Pos2),
    Button(egui::PointerButton, bool),
    Wheel(f32),
    Key(KeyName, bool),
    Text(String),
}

/// The frames of events a step makes, starting from the pointer at `at`.
fn frames(step: &Step, at: &mut egui::Pos2) -> Result<Vec<Vec<Ev>>, String> {
    let p = |v: [f32; 2]| egui::pos2(v[0], v[1]);
    let mods = |with: &[String]| with.iter().map(|m| key_name(m)).collect::<Result<Vec<_>, _>>();
    let down = |keys: &[KeyName]| keys.iter().map(|k| Ev::Key(*k, true)).collect::<Vec<_>>();
    let up = |keys: &[KeyName]| keys.iter().rev().map(|k| Ev::Key(*k, false)).collect::<Vec<_>>();
    let mut out = Vec::new();
    match step {
        Step::Move { to, frames } => {
            let (from, n) = (*at, (*frames).max(1));
            for i in 1..=n {
                out.push(vec![Ev::To(from.lerp(p(*to), i as f32 / n as f32))]);
            }
            *at = p(*to);
        }
        Step::Click { at: to, button, with } => {
            let m = mods(with)?;
            let mut first = down(&m);
            first.push(Ev::To(p(*to)));
            out.push(first);
            out.push(vec![Ev::Button(button.egui(), true)]);
            out.push(vec![Ev::Button(button.egui(), false)]);
            if !m.is_empty() {
                out.push(up(&m));
            }
            *at = p(*to);
        }
        Step::DoubleClick { at: to, button } => {
            // Both clicks in one frame: egui's double-click window (0.3 s) is
            // shorter than a few frames of software rendering.
            out.push(vec![Ev::To(p(*to))]);
            let b = button.egui();
            out.push(vec![Ev::Button(b, true), Ev::Button(b, false), Ev::Button(b, true), Ev::Button(b, false)]);
            *at = p(*to);
        }
        Step::Press { button, at: to } | Step::Release { button, at: to } => {
            let mut frame = Vec::new();
            if let Some(to) = to {
                frame.push(Ev::To(p(*to)));
                *at = p(*to);
            }
            frame.push(Ev::Button(button.egui(), matches!(step, Step::Press { .. })));
            out.push(frame);
        }
        Step::Drag { from, to, button, frames, with } => {
            let m = mods(with)?;
            let mut first = down(&m);
            first.push(Ev::To(p(*from)));
            out.push(first);
            out.push(vec![Ev::Button(button.egui(), true)]);
            let n = (*frames).max(1);
            for i in 1..=n {
                out.push(vec![Ev::To(p(*from).lerp(p(*to), i as f32 / n as f32))]);
            }
            out.push(vec![Ev::Button(button.egui(), false)]);
            if !m.is_empty() {
                out.push(up(&m));
            }
            *at = p(*to);
        }
        Step::Wheel { lines, at: to } => {
            if let Some(to) = to {
                out.push(vec![Ev::To(p(*to))]);
                *at = p(*to);
            }
            out.push(vec![Ev::Wheel(*lines)]);
        }
        Step::Key { key } => {
            let k = chord(key)?;
            out.push(down(&k));
            out.push(up(&k));
        }
        Step::KeyDown { key } => out.push(down(&chord(key)?)),
        Step::KeyUp { key } => out.push(up(&chord(key)?)),
        Step::Text { text } => out.push(vec![Ev::Text(text.clone())]),
        Step::Wait { frames } => out.extend((0..*frames).map(|_| Vec::new())),
    }
    Ok(out)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct WidgetArgs {
    /// Labels, values or hints containing this (any case).
    text: Option<String>,
    /// Roles containing this (any case): button, checkbox, label...
    role: Option<String>,
    /// Only widgets inside this panel.
    panel: Option<String>,
    limit: Option<usize>,
}

/// Input being played into the editor, one frame of events at a time.
#[derive(Resource, Default)]
struct Script {
    /// Each request's frames of events and its reply.
    queue: VecDeque<(VecDeque<Vec<Ev>>, Reply)>,
    pointer: egui::Pos2,
    modifiers: egui::Modifiers,
}

/// The widgets egui drew last frame, from its accessibility tree.
#[derive(Resource, Default)]
struct Widgets(Vec<Widget>);

#[derive(Clone)]
struct Widget {
    role: String,
    label: String,
    value: String,
    /// A text field's hint, shown while it is empty.
    hint: String,
    /// The part on screen.
    rect: egui::Rect,
    /// Partly scrolled or cut out of sight.
    clipped: bool,
    /// In a menu, pop-up or window drawn over the panels.
    floating: bool,
    disabled: bool,
    toggled: Option<bool>,
}

/// Starts what `serve` passed on.
fn take(
    mut pending: ResMut<Pending>,
    mut script: ResMut<Script>,
    widgets: Res<Widgets>,
    screen: Res<crate::Screen>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
) {
    for (command, reply) in std::mem::take(&mut pending.0) {
        let result = match command {
            Command::Input(steps) => {
                let mut at = script.pointer;
                let mut all = VecDeque::new();
                match steps.iter().try_for_each(|s| frames(s, &mut at).map(|f| all.extend(f))) {
                    Ok(()) => {
                        script.queue.push_back((all, reply));
                        continue;
                    }
                    Err(e) => Err(e),
                }
            }
            Command::Widgets(args) => widget_list(&widgets.0, &screen, &args),
            Command::Window([w, h]) => {
                window.resolution.set(w as f32, h as f32);
                // Answer once the editor has drawn at the new size.
                script.queue.push_back((VecDeque::from([Vec::new(), Vec::new()]), reply));
                continue;
            }
        };
        let _ = reply.send(result.map(Response::Json));
    }
}

/// Plays a frame of the script into egui, and answers a request on the frame
/// after its last events, once the editor has seen them.
#[allow(clippy::too_many_arguments)]
fn inject(
    mut script: ResMut<Script>,
    mut modifiers: ResMut<ModifierKeysState>,
    context: Single<Entity, With<PrimaryEguiContext>>,
    document: Res<crate::document::Document>,
    selection: Res<crate::document::Selection>,
    status: Res<crate::Status>,
    screen: Res<crate::Screen>,
    mut events: MessageWriter<EguiInputEvent>,
) {
    let context = *context;
    let Some((frames, _)) = script.queue.front_mut() else { return };
    let Some(frame) = frames.pop_front() else {
        let (_, reply) = script.queue.pop_front().unwrap();
        // What input usually changes; editor_status has the rest.
        let _ = reply.send(Ok(Response::Json(json!({
            "frame": screen.frames,
            "pointer": [script.pointer.x, script.pointer.y],
            "dirty": document.is_dirty(),
            "selection": super::selection_json(*selection),
            "message": status.0,
        }))));
        return;
    };
    for ev in frame {
        let held = script.modifiers;
        let event = match ev {
            Ev::To(p) => {
                script.pointer = p;
                egui::Event::PointerMoved(p)
            }
            Ev::Button(button, pressed) => {
                egui::Event::PointerButton { pos: script.pointer, button, pressed, modifiers: held }
            }
            Ev::Wheel(lines) => egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, lines),
                phase: egui::TouchPhase::Move,
                modifiers: held,
            },
            Ev::Key(KeyName::Key(key), pressed) => {
                egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: held }
            }
            Ev::Key(modifier, pressed) => {
                let m = &mut script.modifiers;
                match modifier {
                    KeyName::Ctrl => (m.ctrl, m.command) = (pressed, pressed),
                    KeyName::Shift => m.shift = pressed,
                    _ => m.alt = pressed,
                }
                (modifiers.ctrl, modifiers.shift, modifiers.alt) = (m.ctrl, m.shift, m.alt);
                continue;
            }
            Ev::Text(text) => egui::Event::Text(text),
        };
        events.write(EguiInputEvent { context, event });
    }
}

/// Copies egui's accessibility tree, built during the pass that just ended.
/// Each widget is cut to the part egui would let you click (scroll areas lay
/// out all their contents), and ones scrolled out of sight are left out.
fn read_widgets(
    mut widgets: ResMut<Widgets>,
    mut outputs: Query<(&EguiFullOutput, &mut EguiContext), With<PrimaryEguiContext>>,
) {
    let Ok((EguiFullOutput(Some(output)), mut ctx)) = outputs.single_mut() else { return };
    let Some(update) = &output.platform_output.accesskit_update else { return };
    let ctx = ctx.get_mut().clone();
    widgets.0 = update
        .nodes
        .iter()
        // Text runs repeat their label's text.
        .filter(|(_, n)| n.role() != accesskit::Role::TextRun)
        .filter_map(|(id, n)| {
            let b = n.bounds()?;
            let full =
                egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32));
            // SAFETY: not a memory-safety matter; the value came from an egui
            // Id, so it has the entropy egui's id maps expect.
            let egui_id = (id.0 != 0).then(|| unsafe { egui::Id::from_high_entropy_bits(id.0) });
            let response = egui_id.and_then(|i| ctx.read_response(i));
            let shown = response.as_ref().map_or(full, |r| full.intersect(r.interact_rect));
            let floating = response.is_some_and(|r| r.layer_id.order != egui::Order::Background);
            if shown.width() < 1.0 || shown.height() < 1.0 {
                return None;
            }
            Some(Widget {
                role: format!("{:?}", n.role()),
                label: n.label().unwrap_or_default().to_string(),
                value: n.value().unwrap_or_default().to_string(),
                hint: n.placeholder().unwrap_or_default().to_string(),
                rect: shown,
                clipped: shown != full,
                floating,
                disabled: n.is_disabled(),
                toggled: n.toggled().map(|t| t != accesskit::Toggled::False),
            })
        })
        .collect();
}

/// What menus, pop-ups and dialogs count as, wherever they are drawn.
const POP_UP: &str = "pop-up";

/// The panel a widget is in: the innermost one under its centre (the view is
/// drawn under the others), or a pop-up.
fn panel_of<'a>(w: &Widget, screen: &'a crate::Screen) -> Option<&'a str> {
    if w.floating {
        return Some(POP_UP);
    }
    screen.panels.iter().rev().find(|(_, r)| r.contains(w.rect.center())).map(|(name, _)| *name)
}

fn widget_list(widgets: &[Widget], screen: &crate::Screen, args: &WidgetArgs) -> Result<Value, String> {
    if let Some(p) = args.panel.as_deref().filter(|p| !p.eq_ignore_ascii_case(POP_UP)) {
        super::panel_rect(screen, p)?;
    }
    let text = args.text.as_deref().map(str::to_lowercase);
    let role = args.role.as_deref().map(str::to_lowercase);
    let mut found: Vec<&Widget> = widgets
        .iter()
        .filter(|w| !w.label.is_empty() || !w.value.is_empty() || !w.hint.is_empty())
        .filter(|w| args.panel.as_ref().is_none_or(|p| panel_of(w, screen).is_some_and(|n| n.eq_ignore_ascii_case(p))))
        .filter(|w| {
            text.as_ref().is_none_or(|t| [&w.label, &w.value, &w.hint].iter().any(|s| s.to_lowercase().contains(t)))
        })
        .filter(|w| role.as_ref().is_none_or(|r| w.role.to_lowercase().contains(r)))
        .collect();
    found.sort_by(|a, b| a.rect.top().total_cmp(&b.rect.top()).then(a.rect.left().total_cmp(&b.rect.left())));
    let total = found.len();
    let list: Vec<Value> = found
        .into_iter()
        .take(args.limit.unwrap_or(60))
        .map(|w| {
            let c = w.rect.center();
            let mut o = json!({"role": w.role, "rect": super::rect_json(w.rect), "centre": [c.x, c.y]});
            for (k, v) in [("label", &w.label), ("value", &w.value), ("hint", &w.hint)] {
                if !v.is_empty() {
                    o[k] = json!(v);
                }
            }
            if let Some(name) = panel_of(w, screen) {
                o["panel"] = json!(name);
            }
            if w.disabled {
                o["disabled"] = json!(true);
            }
            if w.clipped {
                o["clipped"] = json!(true);
            }
            if let Some(t) = w.toggled {
                o["toggled"] = json!(t);
            }
            o
        })
        .collect();
    Ok(json!({"widgets": list, "total": total}))
}

/// The testing tools' names, descriptions and argument schemas.
pub(super) fn tools() -> Vec<(&'static str, &'static str, Value)> {
    let pair = json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2});
    let button = json!({"type": "string", "enum": ["left", "right", "middle"], "default": "left"});
    let mods = json!({"type": "array", "items": {"type": "string", "enum": ["Ctrl", "Shift", "Alt"]}, "description": "Modifiers held throughout"});
    let step = json!({"type": "object", "required": ["do"], "properties": {
        "do": {"type": "string", "enum": ["move", "click", "double_click", "press", "release", "drag", "wheel", "key", "key_down", "key_up", "text", "wait"]},
        "to": pair, "at": pair, "from": pair, "button": button, "with": mods,
        "frames": {"type": "integer", "minimum": 0, "maximum": 3600},
        "lines": {"type": "number", "description": "wheel: positive scrolls up / zooms in"},
        "key": {"type": "string", "description": "A key or chord: \"A\", \"Tab\", \"Escape\", \"Delete\", \"F5\", \"Ctrl+Z\", \"Ctrl+Shift+S\""},
        "text": {"type": "string"}
    }, "description": "move {to, frames=1} | click {at, button, with} | double_click {at, button} | press/release {button, at?} | drag {from, to, button, frames=10, with} | wheel {lines, at?} | key {key} | key_down/key_up {key} | text {text} | wait {frames}"});
    vec![
        (
            "editor_input",
            "Play mouse and keyboard input into the editor, a frame at a time, through egui's input as a person's would arrive. Positions are window pixels, origin top-left. Answers once the editor has seen the last step. While this server runs only injected input reaches the editor.",
            json!({"type": "object", "properties": {"steps": {"type": "array", "items": step, "minItems": 1, "maxItems": 256}}, "required": ["steps"], "additionalProperties": false}),
        ),
        (
            "editor_widgets",
            "Find widgets egui drew last frame (buttons, labels, check boxes, menu items, text fields and their values) by text, role or panel, with their rectangles [x, y, w, h] and centres to click. Widgets scrolled out of sight are left out.",
            json!({"type": "object", "properties": {"text": {"type": "string", "description": "Label, value or hint contains this (any case)"}, "role": {"type": "string", "description": "Role contains this: button, checkbox, label, textinput..."}, "panel": {"type": "string", "description": "menu bar, outline, view, inspector, status bar, or pop-up (menus and dialogs)"}, "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 60}}, "additionalProperties": false}),
        ),
        (
            "editor_window",
            "Resize the window, in pixels. Answers once the editor has drawn at the new size.",
            json!({"type": "object", "properties": {"size": {"type": "array", "items": {"type": "integer", "minimum": 200, "maximum": 4096}, "minItems": 2, "maxItems": 2}}, "required": ["size"], "additionalProperties": false}),
        ),
    ]
}

/// The testing tool `name` asks for, if it is one.
pub(super) fn parse(name: &str, value: Value) -> Option<Result<Command, String>> {
    fn de<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Size {
        size: [u32; 2],
    }
    Some(match name {
        "editor_input" => de::<InputArgs>(value).and_then(|a| match a.steps.len() {
            1..=256 => Ok(Command::Input(a.steps)),
            _ => Err("give 1..256 steps".into()),
        }),
        "editor_widgets" => de(value).map(Command::Widgets),
        "editor_window" => de::<Size>(value).and_then(|s| match s.size {
            [200..=4096, 200..=4096] => Ok(Command::Window(s.size)),
            _ => Err("size must be 200..4096 each way".into()),
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_and_steps_become_frames() {
        assert_eq!(chord("Ctrl+Shift+Z").unwrap(), [KeyName::Ctrl, KeyName::Shift, KeyName::Key(egui::Key::Z)]);
        assert_eq!(chord("delete").unwrap(), [KeyName::Key(egui::Key::Delete)]);
        assert_eq!(chord("a").unwrap(), [KeyName::Key(egui::Key::A)]);
        assert_eq!(chord("F5").unwrap(), [KeyName::Key(egui::Key::F5)]);
        assert!(chord("Hyper").is_err());
        let mut at = egui::Pos2::ZERO;
        let drag = Step::Drag {
            from: [10.0, 10.0],
            to: [30.0, 10.0],
            button: Button::Left,
            frames: 4,
            with: vec!["Ctrl".into()],
        };
        let f = frames(&drag, &mut at).unwrap();
        // Ctrl and move there, press, four moves, release, Ctrl up.
        assert_eq!(f.len(), 8);
        assert_eq!(f[1], [Ev::Button(egui::PointerButton::Primary, true)]);
        assert_eq!(f[6], [Ev::Button(egui::PointerButton::Primary, false)]);
        assert_eq!(f[7], [Ev::Key(KeyName::Ctrl, false)]);
        assert_eq!(at, egui::pos2(30.0, 10.0));
        let step: Step = serde_json::from_value(json!({"do": "click", "at": [5, 6]})).unwrap();
        assert_eq!(frames(&step, &mut at).unwrap().len(), 3);
    }
}

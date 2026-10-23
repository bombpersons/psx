//! The editor's MCP server: tools for agents that build levels on their own or
//! help someone build theirs, and (in developer builds) for testing the editor.
//!
//! Two transports serve the same tools:
//! - `--mcp`: JSON-RPC on stdio, for an agent that starts the editor itself
//!   (stdout then carries nothing else). With `--headless` there is no OS
//!   window: the window is only a size and the editor draws into an image.
//! - HTTP at `/mcp`, so an agent can join an editor someone already has open:
//!   switched on in Tools > Agents (`agents`), or for one run with
//!   `--mcp-listen 127.0.0.1:7878` [`--mcp-token`]. Only loopback addresses are
//!   served, browsers' requests (an `Origin` header) are refused, and every
//!   request needs the bearer token the Agents window shows.
//!
//! The tools come in three sets:
//! - reading and viewing (here): `editor_status`, `editor_level`,
//!   `editor_locate`, `editor_select`, `editor_screenshot`;
//! - editing (`edit`): the menus' commands and the level's contents, each call
//!   one undo step, while the person at the editor keeps their mouse and keyboard;
//! - testing (`input`: the `dev-mcp` feature, stdio only): mouse and keyboard
//!   played in through egui's input, and the widgets egui drew, found by label.
//!
//! Grow it with the editor: a new format or panel gets its reading tools here
//! and its editing tools in `edit` in the same change (see `AGENTS.md`).
//!
//! Screen positions are window pixels (egui points), origin top-left; level
//! positions are metres. The server threads hand requests to the main thread
//! over a channel and wait for replies, as psx-bevy's game MCP does.
mod agents;
mod edit;
#[cfg(feature = "dev-mcp")]
mod input;

pub use agents::{Agents, indicator as agents_indicator, window as agents_window};

use crate::document::{Document, History, Selection};
use crate::panels::ViewState;
use crate::{Dialogs, Screen, Status};
use base64::Engine;
use bevy::camera::RenderTarget;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache, TextureFormat, TextureUsages};
use bevy::render::view::ViewTarget;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::window::PrimaryWindow;
use bevy_egui::egui;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

/// Serves the editor's MCP. Add it after [`crate::EditorPlugin`].
pub struct EditorMcpPlugin {
    /// Serve on stdio; the editor quits when that client goes.
    pub stdio: bool,
    /// Serve over HTTP here (a loopback address) for this run, whatever the
    /// Agents window has kept.
    pub listen: Option<SocketAddr>,
    /// The HTTP bearer token for this run.
    pub token: Option<String>,
    /// No OS window: draw into an image (see `headless_display`).
    pub headless: bool,
}

type Reply = oneshot::Sender<Result<Response, String>>;

enum Response {
    Json(Value),
    Image { png: Vec<u8>, metadata: Value },
}

struct Request {
    command: Command,
    reply: Reply,
    /// From an agent over HTTP (the Agents window shows when one is at work).
    remote: bool,
}

enum Command {
    Status,
    Level(LevelArgs),
    Locate(LocateArgs),
    Select(SelectArgs),
    Screenshot(ShotArgs),
    Edit(edit::Edit),
    #[cfg(feature = "dev-mcp")]
    Testing(input::Command),
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LevelArgs {
    #[serde(default)]
    part: Part,
}

#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Part {
    #[default]
    Summary,
    Objects,
    Triggers,
    Ron,
}

/// One thing in the level, by index (as `editor_level` lists them).
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LocateArgs {
    object: Option<usize>,
    trigger: Option<usize>,
    /// A world point, metres.
    point: Option<[f32; 3]>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SelectArgs {
    object: Option<usize>,
    trigger: Option<usize>,
    /// Centre the view on it (default true).
    centre: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ShotArgs {
    /// Crop to a panel: menu bar, outline, view, inspector or status bar.
    panel: Option<String>,
    /// Crop to [x, y, width, height] window pixels.
    rect: Option<[f32; 4]>,
}

#[derive(Resource)]
struct Bridge {
    receiver: Mutex<mpsc::Receiver<Request>>,
    /// Set when the stdio client goes away.
    closed: Arc<AtomicBool>,
    ready: RenderReady,
    /// Screenshots asked for before the first frames rendered.
    waiting: Vec<(ShotArgs, Reply)>,
}

#[derive(Resource, Clone)]
struct RenderReady(Arc<AtomicBool>);

/// `--headless`: the image the window's camera draws into.
#[derive(Resource, Default)]
struct HeadlessTarget(Option<Handle<Image>>);

/// The editor as the tools see it: what is open, its history, the view and
/// what was drawn.
#[derive(SystemParam)]
struct Editor<'w> {
    document: ResMut<'w, Document>,
    history: ResMut<'w, History>,
    selection: ResMut<'w, Selection>,
    view: ResMut<'w, ViewState>,
    status: ResMut<'w, Status>,
    dialogs: Res<'w, Dialogs>,
    screen: Res<'w, Screen>,
}

impl Editor<'_> {
    /// Why the person's work stops an agent's edit right now, if it does.
    fn busy(&self) -> Option<&'static str> {
        if self.screen.dragging || self.history.in_gesture() {
            Some("the person at the editor is part way through an edit (a drag or a field); try again in a moment")
        } else if self.dialogs.open.is_some() || self.dialogs.save_as.is_some() || self.dialogs.confirm.is_some() {
            Some("a dialog is open in the editor; it needs answering first")
        } else {
            None
        }
    }
}

/// The servers: stdio, if an agent started the editor, and HTTP while the
/// Agents window has it on. They share one queue to the main thread.
#[derive(Resource)]
struct Servers {
    /// Made when first needed: most editors never serve anything.
    runtime: Option<tokio::runtime::Runtime>,
    sender: mpsc::SyncSender<Request>,
    /// One request at a time, from either transport: each waits for the
    /// main thread's answer.
    slots: Arc<Semaphore>,
    stdio: bool,
    http: Option<agents::Http>,
}

impl Servers {
    fn runtime(&mut self) -> &tokio::runtime::Runtime {
        self.runtime.get_or_insert_with(|| {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder.worker_threads(2).thread_name("editor-mcp").enable_all().build().unwrap()
        })
    }
}

impl Plugin for EditorMcpPlugin {
    fn build(&self, app: &mut App) {
        let (sender, receiver) = mpsc::sync_channel(16);
        let closed = Arc::new(AtomicBool::new(false));
        let mut servers =
            Servers { runtime: None, sender, slots: Arc::new(Semaphore::new(1)), stdio: self.stdio, http: None };
        if self.stdio {
            let server = EditorServer {
                sender: servers.sender.clone(),
                slots: servers.slots.clone(),
                testing: cfg!(feature = "dev-mcp"),
                remote: false,
            };
            let closed = closed.clone();
            servers.runtime().spawn(async move {
                match server.serve(rmcp::transport::stdio()).await {
                    Ok(service) => {
                        if let Err(e) = service.waiting().await {
                            eprintln!("editor MCP: {e}");
                        }
                    }
                    Err(e) => eprintln!("editor MCP startup: {e}"),
                }
                closed.store(true, Ordering::Release);
            });
        }
        let ready = RenderReady(Arc::new(AtomicBool::new(false)));
        app.sub_app_mut(RenderApp)
            .insert_resource(ready.clone())
            .add_systems(Render, mark_render_ready.after(RenderSystems::Render).before(RenderSystems::Cleanup));
        app.insert_resource(Bridge { receiver: Mutex::new(receiver), closed, ready, waiting: Vec::new() })
            .insert_resource(servers)
            .insert_resource(Agents::new(self.stdio, self.listen, self.token.clone()))
            .add_systems(PreUpdate, serve)
            .add_systems(Update, agents::switch);
        #[cfg(feature = "dev-mcp")]
        if self.stdio {
            input::build(app);
        }
        if self.headless {
            // There is no OS window to show an input method on.
            app.world_mut().resource_mut::<bevy_egui::EguiGlobalSettings>().enable_ime = false;
            app.init_resource::<HeadlessTarget>().add_systems(Update, headless_display);
        }
    }
}

/// A token nobody can guess: 128 bits of SipHash output, keyed by the
/// standard library from the operating system's random source.
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let part = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(salt);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.finish()
    };
    format!("{:016x}{:016x}", part(1), part(2))
}

fn mark_render_ready(
    ready: Res<RenderReady>,
    cache: Res<PipelineCache>,
    views: Query<&ViewTarget>,
    mut settled: Local<u32>,
) {
    // Wait for the view and every queued pipeline to compile for a few frames
    // running, so the first screenshot isn't of an empty target.
    let compiled =
        cache.pipelines().next().is_some() && cache.pipelines().all(|p| matches!(&p.state, CachedPipelineState::Ok(_)));
    *settled = if !views.is_empty() && compiled { *settled + 1 } else { 0 };
    if *settled >= 3 {
        ready.0.store(true, Ordering::Release);
    }
}

/// Without an OS window the window's camera (the one egui draws on) renders
/// into an image the size of the window.
fn headless_display(
    mut target: ResMut<HeadlessTarget>,
    mut images: ResMut<Assets<Image>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cameras: Query<&mut RenderTarget, With<Camera2d>>,
) {
    let size = UVec2::new(window.physical_width().max(1), window.physical_height().max(1));
    if target.0.as_ref().and_then(|h| images.get(h)).is_none_or(|i| i.size() != size) {
        let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
        image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        target.0 = Some(images.add(image));
    }
    let handle = target.0.clone().unwrap();
    for mut t in &mut cameras {
        if !matches!(&*t, RenderTarget::Image(i) if i.handle == handle) {
            *t = RenderTarget::Image(handle.clone().into());
        }
    }
}

/// Answers requests on the main thread, before the frame's input and UI.
#[allow(clippy::too_many_arguments)]
fn serve(
    mut commands: Commands,
    mut bridge: ResMut<Bridge>,
    mut agents: ResMut<Agents>,
    mut editor: Editor,
    headless: Option<Res<HeadlessTarget>>,
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    #[cfg(feature = "dev-mcp")] mut testing: Option<ResMut<input::Pending>>,
    mut exit: MessageWriter<AppExit>,
) {
    if bridge.closed.load(Ordering::Acquire) {
        exit.write(AppExit::Success);
        return;
    }
    // Answers describe the editor as drawn, so wait for its first frame.
    if editor.screen.frames == 0 {
        return;
    }
    let window = window.as_deref().copied();
    let scale = window.map_or(1.0, Window::scale_factor);
    let ready = bridge.ready.0.load(Ordering::Acquire);
    if ready {
        for (args, reply) in std::mem::take(&mut bridge.waiting) {
            screenshot(&mut commands, &editor.screen, headless.as_deref(), scale, args, reply);
        }
    }
    let requests: Vec<Request> = bridge.receiver.lock().unwrap().try_iter().collect();
    for Request { command, reply, remote } in requests {
        if reply.is_closed() {
            continue;
        }
        if remote {
            agents.last_request = Some(std::time::Instant::now());
        }
        let result = match command {
            Command::Status => Ok(status(&editor, window, headless.is_some())),
            Command::Level(args) => level(&editor.document, args.part),
            Command::Locate(args) => locate(&editor, &args),
            Command::Select(args) => select(&mut editor, &args),
            Command::Edit(e) => {
                let busy = editor.busy();
                let target = edit::Target {
                    document: &mut editor.document,
                    history: &mut editor.history,
                    selection: &mut editor.selection,
                    status: &mut editor.status,
                };
                edit::apply(e, busy, target)
            }
            Command::Screenshot(args) => {
                if ready {
                    screenshot(&mut commands, &editor.screen, headless.as_deref(), scale, args, reply);
                } else {
                    bridge.waiting.push((args, reply));
                }
                continue;
            }
            #[cfg(feature = "dev-mcp")]
            Command::Testing(t) => match testing.as_mut() {
                Some(pending) => {
                    pending.0.push((t, reply));
                    continue;
                }
                None => Err("the testing tools are only served on stdio".into()),
            },
        };
        let _ = reply.send(result.map(Response::Json));
    }
}

fn rect_json(r: egui::Rect) -> Value {
    json!([r.left(), r.top(), r.width(), r.height()])
}

fn panel_rect(screen: &Screen, name: &str) -> Result<egui::Rect, String> {
    screen.panels.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, r)| *r).ok_or_else(|| {
        let names: Vec<_> = screen.panels.iter().map(|(n, _)| *n).collect();
        format!("no panel {name:?} on screen; there are {names:?}")
    })
}

fn selection_json(selection: Selection) -> Value {
    match selection {
        Selection::None => Value::Null,
        Selection::Object(i) => json!({"object": i}),
        Selection::Trigger(i) => json!({"trigger": i}),
    }
}

fn status(editor: &Editor, window: Option<&Window>, headless: bool) -> Value {
    let (document, dialogs) = (&*editor.document, &*editor.dialogs);
    let dialog = [
        (dialogs.open.is_some(), "open"),
        (dialogs.save_as.is_some(), "save as"),
        (dialogs.confirm.is_some(), "unsaved changes"),
    ]
    .into_iter()
    .find_map(|(open, name)| open.then_some(name));
    let panels: serde_json::Map<_, _> =
        editor.screen.panels.iter().map(|(name, r)| (name.to_string(), rect_json(*r))).collect();
    json!({
        "frame": editor.screen.frames,
        "file": document.path,
        "dirty": document.is_dirty(),
        "can_undo": editor.history.can_undo(),
        "can_redo": editor.history.can_redo(),
        "selection": selection_json(*editor.selection),
        "message": editor.status.0,
        "busy": editor.busy(),
        "dialog": dialog,
        "counts": {"objects": document.level.objects.len(), "triggers": document.level.triggers.len()},
        "view": {"centre": [editor.view.centre.x, editor.view.centre.y], "pixels_per_metre": editor.view.scale},
        "panels": panels,
        "window": window.map(|w| [w.width(), w.height()]),
        "headless": headless,
    })
}

fn level(document: &Document, part: Part) -> Result<Value, String> {
    let level = &document.level;
    let objects = || -> Vec<Value> {
        level
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| {
                json!({"index": i, "type_id": o.type_id.0, "position": o.position.to_array(), "yaw": o.yaw, "params": o.params})
            })
            .collect()
    };
    let triggers = || -> Vec<Value> {
        level
            .triggers
            .iter()
            .enumerate()
            .map(|(i, t)| {
                json!({"index": i, "position": t.position.to_array(), "half_extents": t.half_extents.to_array(),
                    "action": t.call.action.0, "args": t.call.args, "once": t.once})
            })
            .collect()
    };
    Ok(match part {
        Part::Summary => {
            let points = level.objects.iter().map(|o| o.position).chain(level.triggers.iter().map(|t| t.position));
            let bounds = points
                .fold(None, |b: Option<(Vec3, Vec3)>, p| Some(b.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p)))));
            json!({
                "file": document.path,
                "objects": level.objects.len(),
                "triggers": level.triggers.len(),
                "bounds": bounds.map(|(lo, hi)| json!({"min": lo.to_array(), "max": hi.to_array()})),
            })
        }
        Part::Objects => json!({"objects": objects()}),
        Part::Triggers => json!({"triggers": triggers()}),
        Part::Ron => json!({"ron": document.to_text()?}),
    })
}

/// Where a level position is drawn, and what is on top of it there.
fn on_screen(editor: &Editor, p: Vec3) -> Value {
    let at = editor.view.to_screen(p);
    let visible = editor.view.rect.contains(at);
    // The view is drawn under the panels; one covering the point takes the click.
    let covered = editor.screen.panels.iter().find(|(name, r)| *name != "view" && r.contains(at)).map(|(n, _)| *n);
    json!({"at": [at.x, at.y], "visible": visible && covered.is_none(), "covered_by": covered})
}

fn locate(editor: &Editor, args: &LocateArgs) -> Result<Value, String> {
    let level = &editor.document.level;
    match (args.object, args.trigger, args.point) {
        (Some(i), None, None) => {
            let o = level.objects.get(i).ok_or(format!("no object {i}"))?;
            Ok(on_screen(editor, o.position))
        }
        (None, Some(i), None) => {
            let t = level.triggers.get(i).ok_or(format!("no trigger {i}"))?;
            let mut found = on_screen(editor, t.position);
            let corners = [t.position - t.half_extents, t.position + t.half_extents].map(|p| editor.view.to_screen(p));
            found["rect"] = rect_json(egui::Rect::from_two_pos(corners[0], corners[1]));
            Ok(found)
        }
        (None, None, Some(p)) => Ok(on_screen(editor, Vec3::from_array(p))),
        _ => Err("give one of object, trigger or point".into()),
    }
}

fn select(editor: &mut Editor, args: &SelectArgs) -> Result<Value, String> {
    let level = &editor.document.level;
    let (selection, at) = match (args.object, args.trigger) {
        (Some(i), None) => (Selection::Object(i), level.objects.get(i).map(|o| o.position)),
        (None, Some(i)) => (Selection::Trigger(i), level.triggers.get(i).map(|t| t.position)),
        (None, None) => (Selection::None, None),
        _ => return Err("give object or trigger, or neither to clear the selection".into()),
    };
    if selection != Selection::None && at.is_none() {
        return Err(format!("nothing to select at {}", selection_json(selection)));
    }
    *editor.selection = selection;
    if let Some(p) = at.filter(|_| args.centre.unwrap_or(true)) {
        editor.view.centre = egui::vec2(p.x, p.z);
    }
    Ok(json!({"selection": selection_json(selection), "view_centre": [editor.view.centre.x, editor.view.centre.y]}))
}

fn screenshot(
    commands: &mut Commands,
    screen: &Screen,
    headless: Option<&HeadlessTarget>,
    scale: f32,
    args: ShotArgs,
    reply: Reply,
) {
    let crop = match (&args.panel, args.rect) {
        (Some(p), _) => match panel_rect(screen, p) {
            Ok(r) => Some(r),
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        },
        (None, Some([x, y, w, h])) => Some(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))),
        (None, None) => None,
    };
    let shot = match headless.and_then(|h| h.0.clone()) {
        Some(image) => Screenshot::image(image),
        None => Screenshot::primary_window(),
    };
    let mut reply = Some(reply);
    commands.spawn(shot).observe(move |capture: On<ScreenshotCaptured>| {
        let Some(reply) = reply.take() else { return };
        let result = (|| {
            let mut image = capture.image.clone().try_into_dynamic().map_err(|e| e.to_string())?;
            if let Some(r) = crop {
                let (x, y) = ((r.left() * scale).max(0.0) as u32, (r.top() * scale).max(0.0) as u32);
                let w = ((r.width() * scale) as u32).min(image.width().saturating_sub(x));
                let h = ((r.height() * scale) as u32).min(image.height().saturating_sub(y));
                image = image.crop_imm(x, y, w.max(1), h.max(1));
            }
            let (width, height) = (image.width(), image.height());
            let mut png = std::io::Cursor::new(Vec::new());
            image.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
            Ok(Response::Image {
                png: png.into_inner(),
                metadata: json!({"width": width, "height": height, "scale": scale}),
            })
        })();
        let _ = reply.send(result);
    });
}

#[derive(Clone)]
struct EditorServer {
    sender: mpsc::SyncSender<Request>,
    slots: Arc<Semaphore>,
    /// Whether this client gets the testing tools (developer builds, stdio).
    #[cfg_attr(not(feature = "dev-mcp"), allow(dead_code))]
    testing: bool,
    /// Over HTTP.
    remote: bool,
}

fn schema(v: Value) -> Arc<serde_json::Map<String, Value>> {
    Arc::new(v.as_object().unwrap().clone())
}

/// The reading and viewing tools: names, descriptions and argument schemas.
fn view_tools() -> Vec<(&'static str, &'static str, Value)> {
    let index = json!({"type": "integer", "minimum": 0});
    let triple = json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3});
    vec![
        (
            "editor_status",
            "The editor's state: file, unsaved changes, undo and redo, selection, status message, whether the person is part way through an edit (busy) or a dialog is open, counts, the view's centre and zoom, where each panel is, and the window size.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        (
            "editor_level",
            "Read the level being edited. part: summary (default: counts and bounds), objects, triggers (each with the index the other tools take), or ron (the file as Save would write it).",
            json!({"type": "object", "properties": {"part": {"type": "string", "enum": ["summary", "objects", "triggers", "ron"], "default": "summary"}}, "additionalProperties": false}),
        ),
        (
            "editor_locate",
            "Where an object, a trigger or a world point (metres) is drawn in the top-down view, in window pixels, and whether a panel covers it.",
            json!({"type": "object", "properties": {"object": index, "trigger": index, "point": triple}, "additionalProperties": false}),
        ),
        (
            "editor_select",
            "Select an object or trigger, as clicking it in the outline does, and centre the view on it (centre: false to leave the view). Neither clears the selection. Moves the view of whoever is using the editor.",
            json!({"type": "object", "properties": {"object": index, "trigger": index, "centre": {"type": "boolean", "default": true}}, "additionalProperties": false}),
        ),
        (
            "editor_screenshot",
            "PNG of the editor as rendered: the whole window, one panel (menu bar, outline, view, inspector or status bar), or a rectangle [x, y, w, h] in window pixels.",
            json!({"type": "object", "properties": {"panel": {"type": "string"}, "rect": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4}}, "additionalProperties": false}),
        ),
    ]
}

impl EditorServer {
    fn tools(&self) -> Vec<Tool> {
        let mut all = view_tools();
        all.extend(edit::tools());
        #[cfg(feature = "dev-mcp")]
        if self.testing {
            all.extend(input::tools());
        }
        all.into_iter().map(|(name, description, s)| Tool::new(name, description, schema(s))).collect()
    }

    fn command(&self, name: &str, value: Value) -> Result<Command, ErrorData> {
        fn de<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ErrorData> {
            serde_json::from_value(v).map_err(|e| ErrorData::invalid_params(e.to_string(), None))
        }
        let bad = |e: String| ErrorData::invalid_params(e, None);
        Ok(match name {
            "editor_status" => {
                if value != json!({}) {
                    return Err(bad("editor_status takes no arguments".into()));
                }
                Command::Status
            }
            "editor_level" => Command::Level(de(value)?),
            "editor_locate" => Command::Locate(de(value)?),
            "editor_select" => Command::Select(de(value)?),
            "editor_screenshot" => Command::Screenshot(de(value)?),
            _ => {
                if let Some(e) = edit::parse(name, value.clone()) {
                    return e.map(Command::Edit).map_err(bad);
                }
                #[cfg(feature = "dev-mcp")]
                if self.testing
                    && let Some(t) = input::parse(name, value)
                {
                    return t.map(Command::Testing).map_err(bad);
                }
                return Err(ErrorData::invalid_params(format!("unknown editor tool {name}"), None));
            }
        })
    }
}

impl ServerHandler for EditorServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Level editor for this game's engine formats. A level places objects (a type id, a position in \
                 metres, a yaw in radians and per-type params) and trigger boxes that call actions. Read with \
                 editor_level and editor_status, look with editor_select and editor_screenshot, and change the \
                 level with the level_* tools: each call is one undo step the person at the editor can undo, and \
                 edits wait while they are part way through one. Open, new and save with editor_open and \
                 editor_save.",
            )
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|t| t.name == name)
    }
    // ListToolsResult is non-exhaustive, so it cannot be built with a literal.
    #[allow(clippy::field_reassign_with_default)]
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut result = ListToolsResult::default();
        result.tools = self.tools();
        Ok(result)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let command = self.command(request.name.as_ref(), Value::Object(request.arguments.unwrap_or_default()))?;
        let _permit = tokio::select! {
            permit = self.slots.acquire() => permit.map_err(|_| ErrorData::internal_error("editor queue closed", None))?,
            _ = context.ct.cancelled() => return Err(ErrorData::internal_error("request cancelled", None)),
        };
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request { command, reply, remote: self.remote })
            .map_err(|e| ErrorData::internal_error(format!("editor queue unavailable: {e}"), None))?;
        let response = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(120), receiver) => result
                .map_err(|_| ErrorData::internal_error("editor response timed out", None))?
                .map_err(|_| ErrorData::internal_error("editor stopped before replying", None))?,
            _ = context.ct.cancelled() => return Err(ErrorData::internal_error("request cancelled", None)),
        };
        // A refused edit is the tool's answer, not a protocol error: the agent
        // reads why and can try again.
        Ok(match response {
            Ok(Response::Json(value)) => CallToolResult::structured(value),
            Ok(Response::Image { png, metadata }) => CallToolResult::success(vec![
                ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
                ContentBlock::text(metadata.to_string()),
            ]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
        .into())
    }
}

//! Developer-only stdio MCP bridge. Bevy owns all game and GPU state.
//! The server thread sends bounded commands to the main thread and awaits replies.
use crate::{
    controller::{Button, ControllerState},
    psx::PsxScreen,
    shell::{GameClock, InjectedInput, InjectedMouse, TickRate},
};
use base64::Engine;
use bevy::render::{
    Render, RenderApp, RenderSystems,
    render_resource::{CachedPipelineState, PipelineCache},
    view::ViewTarget,
};
use bevy::{
    prelude::*,
    reflect::{
        PartialReflect, ReflectFromReflect, ReflectMut, ReflectRef, TypeRegistration, TypeRegistry,
        serde::{TypedReflectDeserializer, TypedReflectSerializer},
    },
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::{RequestContext, RoleServer},
};
use serde::{
    Deserialize,
    de::{DeserializeSeed, IntoDeserializer},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::{Semaphore, oneshot};

type Reply = oneshot::Sender<Result<Response, String>>;
enum Response {
    Json(Value),
    Image { png: Vec<u8>, metadata: Value },
}
struct Request {
    command: Command,
    reply: Reply,
}
enum Command {
    Status,
    Input(Vec<Button>, InjectedMouse),
    Pause(bool),
    Step(StepArgs, InjectedMouse),
    Screenshot(Target),
    Query(QueryArgs),
    SetComponent(SetComponentArgs),
}

/// `game_query`: reflected components of matching entities, and resources.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    /// Entities whose `Name` contains this text.
    #[serde(default)]
    name: Option<String>,
    /// Entities that have all of these components (type names, short or full).
    #[serde(default)]
    with: Vec<String>,
    /// Components to return; all reflected ones when empty.
    #[serde(default)]
    components: Vec<String>,
    /// Resources to return.
    #[serde(default)]
    resources: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Queries wait here for the exclusive system that can read any component.
#[derive(Resource, Default)]
struct PendingQueries(Vec<(QueryArgs, Reply)>);

/// Only named fields of an existing, mutable component can be edited. Entity
/// names must match exactly once; ids include the generation returned by query.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetComponentArgs {
    entity: Option<String>,
    name: Option<String>,
    component: String,
    fields: serde_json::Map<String, Value>,
}

#[derive(Resource, Default)]
struct PendingEdits(Vec<(SetComponentArgs, Reply)>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepArgs {
    frames: u32,
    #[serde(default)]
    buttons: Vec<Button>,
    /// [`InjectedMouse::from_json`]'s form.
    #[serde(default)]
    mouse: Option<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputArgs {
    buttons: Vec<Button>,
    #[serde(default)]
    mouse: Option<Value>,
}
fn mouse_arg(mouse: &Option<Value>) -> Result<InjectedMouse, ErrorData> {
    mouse
        .as_ref()
        .map(InjectedMouse::from_json)
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(|e| ErrorData::invalid_params(e, None))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseArgs {
    paused: bool,
}
#[derive(Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Target {
    #[default]
    Screen,
    Window,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    #[serde(default)]
    target: Target,
}

struct Step {
    remaining: u32,
    total: u32,
    buttons: Vec<Button>,
    mouse: InjectedMouse,
    reply: Reply,
}
/// Pause and stepping state. Injected buttons live in [`InjectedInput`].
#[derive(Resource)]
pub struct Control {
    pub paused: bool,
    step: Option<Step>,
}
impl Default for Control {
    fn default() -> Self {
        Self { paused: true, step: None }
    }
}
impl Control {
    pub(crate) fn interrupt_for_input_focus(&mut self) {
        if let Some(step) = self.step.take() {
            self.paused = true;
            let _ = step.reply.send(Err(
                "Controls menu captured input; close it and release physical buttons before stepping".into(),
            ));
        }
    }
    pub fn has_step(&self) -> bool {
        self.step.is_some()
    }
    /// Drops a step whose client went away. True if its buttons should be released.
    pub fn discard_cancelled_step(&mut self) -> bool {
        if self.step.as_ref().is_some_and(|step| step.reply.is_closed()) {
            self.step = None;
            self.paused = true;
            return true;
        }
        false
    }
    /// Counts a tick against the current step. True when the step finished
    /// and its buttons should be released.
    pub fn finish_tick(&mut self, frame: u64) -> bool {
        let Some(step) = self.step.as_mut() else {
            return false;
        };
        step.remaining -= 1;
        if step.remaining > 0 {
            return false;
        }
        let step = self.step.take().unwrap();
        self.paused = true;
        let _ = step.reply.send(Ok(Response::Json(
            json!({"frame":frame,"advanced_frames":step.total,"buttons":step.buttons,"mouse":step.mouse.to_json(),"paused":true}),
        )));
        true
    }
}

#[derive(Resource)]
struct Bridge {
    receiver: Mutex<mpsc::Receiver<Request>>,
    closed: Arc<AtomicBool>,
    pending: Vec<Request>,
    ready: RenderReady,
}
#[derive(Resource, Clone)]
struct RenderReady(Arc<AtomicBool>);
pub struct GameMcpPlugin;
impl Plugin for GameMcpPlugin {
    fn build(&self, app: &mut App) {
        // Agent commands must be serviced even when a remote-desktop window
        // is unfocused; the renderer's VSync still bounds redraw frequency.
        app.insert_resource(bevy::winit::WinitSettings::continuous());
        let (sender, receiver) = mpsc::sync_channel(16);
        let closed = Arc::new(AtomicBool::new(false));
        let thread_closed = closed.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            runtime.block_on(async move {
                let server = GameServer { sender, slots: Arc::new(Semaphore::new(1)) };
                match server.serve(rmcp::transport::stdio()).await {
                    Ok(service) => {
                        if let Err(error) = service.waiting().await {
                            eprintln!("game MCP: {error}");
                        }
                    }
                    Err(error) => eprintln!("game MCP startup: {error}"),
                }
                thread_closed.store(true, Ordering::Release);
            });
        });
        let ready = RenderReady(Arc::new(AtomicBool::new(false)));
        app.sub_app_mut(RenderApp)
            .insert_resource(ready.clone())
            .add_systems(Render, mark_render_ready.after(RenderSystems::Render).before(RenderSystems::Cleanup));
        app.init_resource::<Control>()
            .init_resource::<PendingQueries>()
            .init_resource::<PendingEdits>()
            .insert_resource(Bridge { receiver: Mutex::new(receiver), closed, pending: Vec::new(), ready })
            .add_systems(PreUpdate, (process_commands, answer_queries, answer_edits).chain());
    }
}

fn mark_render_ready(
    ready: Res<RenderReady>,
    cache: Res<PipelineCache>,
    views: Query<&ViewTarget>,
    mut settled: Local<u32>,
) {
    // A texture may exist before its first camera pass. Wait until the game and
    // screen-pass views exist and every queued pipeline has compiled for a few
    // consecutive frames, so the first screenshot is not an uninitialised target.
    let compiled = cache.pipelines().next().is_some()
        && cache.pipelines().all(|pipeline| matches!(&pipeline.state, CachedPipelineState::Ok(_)));
    *settled = if views.iter().count() >= 2 && compiled { *settled + 1 } else { 0 };
    if *settled >= 3 {
        ready.0.store(true, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn process_commands(
    mut commands: Commands,
    mut bridge: ResMut<Bridge>,
    mut control: ResMut<Control>,
    mut injected: ResMut<InjectedInput>,
    mut queries: ResMut<PendingQueries>,
    mut edits: ResMut<PendingEdits>,
    screen: Res<PsxScreen>,
    clock: Res<GameClock>,
    tick: Res<TickRate>,
    pad: Res<ControllerState>,
    focus: Res<crate::shell::InputFocus>,
    mut exit: MessageWriter<AppExit>,
    adapter: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
    windows: Query<(), With<bevy::window::PrimaryWindow>>,
) {
    if bridge.closed.load(Ordering::Acquire) {
        exit.write(AppExit::Success);
        return;
    }
    let mut requests = std::mem::take(&mut bridge.pending);
    requests.extend(bridge.receiver.lock().unwrap().try_iter());
    for request in requests {
        if request.reply.is_closed() {
            continue;
        }
        if focus.blocked() && matches!(&request.command, Command::Step(..) | Command::Input(..)) {
            let _ = request.reply.send(Err(
                "Controls menu captured input; close it and release physical buttons before stepping".into(),
            ));
            continue;
        }
        if matches!(&request.command, Command::Screenshot(Target::Window)) && windows.is_empty() {
            let _ = request
                .reply
                .send(Err("No primary window in headless mode; use game_screenshot with target screen".into()));
            continue;
        }
        if matches!(&request.command, Command::Screenshot(_)) && !bridge.ready.0.load(Ordering::Acquire) {
            bridge.pending.push(request);
            continue;
        }
        match request.command {
            Command::Status => {
                let renderer=adapter.as_ref().map(|info|json!({"name":info.name,"device_type":format!("{:?}",info.device_type),"backend":format!("{:?}",info.backend)}));
                let _=request.reply.send(Ok(Response::Json(json!({"frame":clock.frame,"paused":control.paused,"headless":windows.is_empty(),"tick_hz":tick.hz,"screen":{"width":screen.width,"height":screen.height,"region":format!("{:?}",screen.region)},"controller_buttons":pad.buttons(),"mouse":{"buttons":pad.mouse.button_names(),"position":pad.mouse.position.map(|p|[p.x,p.y]),"motion":[pad.mouse.motion.x,pad.mouse.motion.y],"wheel":pad.mouse.wheel},"renderer":renderer}))));
            }
            Command::Input(buttons, mouse) => {
                injected.set(&buttons);
                injected.mouse = mouse;
                let _ = request.reply.send(Ok(Response::Json(
                    json!({"frame":clock.frame,"injected_buttons":buttons,"injected_mouse":mouse.to_json()}),
                )));
            }
            Command::Pause(paused) => {
                control.paused = paused;
                let _ = request.reply.send(Ok(Response::Json(json!({"frame":clock.frame,"paused":paused}))));
            }
            Command::Step(args, mouse) => {
                control.paused = true;
                injected.set(&args.buttons);
                injected.mouse = mouse;
                control.step = Some(Step {
                    remaining: args.frames,
                    total: args.frames,
                    buttons: args.buttons,
                    mouse,
                    reply: request.reply,
                });
            }
            Command::Query(args) => queries.0.push((args, request.reply)),
            Command::SetComponent(args) => edits.0.push((args, request.reply)),
            Command::Screenshot(target) => {
                let frame = clock.frame;
                let screenshot = match target {
                    Target::Screen => Screenshot::image(screen.image.clone()),
                    Target::Window => Screenshot::primary_window(),
                };
                let mut reply = Some(request.reply);
                commands.spawn(screenshot).observe(move |capture:On<ScreenshotCaptured>| {
                    let Some(reply)=reply.take() else {return;};
                    let result=(|| {
                        let image=capture.image.clone().try_into_dynamic().map_err(|error|error.to_string())?;
                        let width=image.width();let height=image.height();
                        let mut png=std::io::Cursor::new(Vec::new());
                        image.write_to(&mut png,image::ImageFormat::Png).map_err(|error|error.to_string())?;
                        Ok(Response::Image {png:png.into_inner(),metadata:json!({"requested_at_frame":frame,"width":width,"height":height,"target":match target {Target::Screen=>"screen",Target::Window=>"window"},"mime_type":"image/png"})})
                    })();
                    let _=reply.send(result);
                });
            }
        }
    }
}

/// Answers `game_query` with full world access, so any reflected component
/// or resource can be read.
fn answer_queries(world: &mut World) {
    let pending = std::mem::take(&mut world.resource_mut::<PendingQueries>().0);
    if pending.is_empty() {
        return;
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    for (args, reply) in pending {
        let _ = reply.send(query(world, &registry, &args).map(Response::Json));
    }
}

fn lookup_type<'a>(registry: &'a TypeRegistry, name: &str) -> Result<&'a TypeRegistration, String> {
    registry.get_with_type_path(name).or_else(|| registry.get_with_short_type_path(name)).ok_or_else(|| {
        if registry.is_ambiguous(name) {
            format!("{name} is ambiguous: use its full type path")
        } else {
            format!("unknown type {name}: derive Reflect and add #[reflect(Component)] or #[reflect(Resource)]")
        }
    })
}

fn answer_edits(world: &mut World) {
    let pending = std::mem::take(&mut world.resource_mut::<PendingEdits>().0);
    if pending.is_empty() {
        return;
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    for (args, reply) in pending {
        if !reply.is_closed() {
            let _ = reply.send(set_component(world, &registry, &args).map(Response::Json));
        }
    }
}

impl SetComponentArgs {
    fn validate(&self) -> Result<(), String> {
        if self.entity.is_some() == self.name.is_some() {
            return Err("give exactly one of entity (id from game_query) or name (unique exact Name)".into());
        }
        if self.entity.as_deref().is_some_and(str::is_empty) || self.name.as_deref().is_some_and(str::is_empty) {
            return Err("entity/name must not be empty".into());
        }
        if self.fields.is_empty() || self.fields.len() > 64 {
            return Err("fields must contain 1..64 named component fields".into());
        }
        if serde_json::to_vec(&self.fields).map_err(|e| e.to_string())?.len() > 64 * 1024 {
            return Err("field patch exceeds 64 KiB".into());
        }
        Ok(())
    }
}

fn set_component(world: &mut World, registry: &TypeRegistry, args: &SetComponentArgs) -> Result<Value, String> {
    args.validate()?;
    let control = world.resource::<Control>();
    if !control.paused || control.has_step() {
        return Err("pause the simulation with game_pause before editing components".into());
    }
    let mut matches = world.iter_entities().filter(|e| match (&args.entity, &args.name) {
        (Some(id), _) => e.id().to_string() == *id,
        (_, Some(name)) => e.get::<Name>().is_some_and(|n| n.as_str() == name),
        _ => false,
    });
    let entity = matches.next().ok_or("no entity matches this id/exact name")?.id();
    if matches.next().is_some() {
        return Err("entity name is not unique; use an entity id from game_query".into());
    }
    drop(matches);
    let registration = lookup_type(registry, &args.component)?;
    let access = registration.data::<ReflectComponent>().ok_or("type does not expose ReflectComponent")?;
    let component_id = world.components().get_id(registration.type_id()).ok_or("component is not present")?;
    if !world.components().get_info(component_id).is_some_and(|c| c.mutable()) {
        return Err("component is immutable and cannot be edited".into());
    }
    if world.resource_entities().get(component_id) == Some(entity) {
        return Err("game_set_component edits entity components, not resources".into());
    }
    let value = access.reflect(world.entity(entity)).ok_or("entity does not have this component")?;
    let ReflectRef::Struct(fields) = value.reflect_ref() else {
        return Err("component must be a reflected struct with named fields".into());
    };
    // Build concrete, typed replacements for every selected field before taking
    // mutable world access. Preserve all unselected/ignored fields and never
    // apply a partially valid JSON patch to the live component.
    let mut staged = Vec::new();
    let mut before = serde_json::Map::new();
    let mut after = serde_json::Map::new();
    for (name, json) in &args.fields {
        let field = fields.field(name).ok_or_else(|| format!("unknown or unreflected field {name}"))?;
        let info = field.get_represented_type_info().ok_or_else(|| format!("{name}: field type is unavailable"))?;
        let ty = registry.get(info.type_id()).ok_or_else(|| format!("{name}: field type is not registered"))?;
        let dynamic = TypedReflectDeserializer::new(ty, registry)
            .deserialize(json.clone().into_deserializer())
            .map_err(|e| format!("{name}: invalid {}: {e}", info.type_path()))?;
        let replacement = match dynamic.try_into_reflect() {
            Ok(value) => value,
            Err(dynamic) => ty
                .data::<ReflectFromReflect>()
                .and_then(|f| f.from_reflect(dynamic.as_ref()))
                .ok_or_else(|| format!("{name}: field type does not support concrete reconstruction"))?,
        };
        let concrete = field.try_as_reflect().ok_or_else(|| format!("{name}: field does not support replacement"))?;
        if concrete.as_any().type_id() != replacement.as_any().type_id() {
            return Err(format!("{name}: replacement has the wrong concrete type"));
        }
        before.insert(
            name.clone(),
            serde_json::to_value(TypedReflectSerializer::new(field, registry)).map_err(|e| format!("{name}: {e}"))?,
        );
        after.insert(
            name.clone(),
            serde_json::to_value(TypedReflectSerializer::new(replacement.as_partial_reflect(), registry))
                .map_err(|e| format!("{name}: {e}"))?,
        );
        staged.push((name.clone(), replacement));
    }
    let frame = world.resource::<GameClock>().frame;
    let name = world.entity(entity).get::<Name>().map(|n| n.as_str().to_owned());
    let mut value = access.reflect_mut(world.entity_mut(entity)).ok_or("component disappeared")?;
    let ReflectMut::Struct(fields) = value.reflect_mut() else {
        return Err("component reflection changed".into());
    };
    for (name, replacement) in staged {
        fields
            .field_mut(&name)
            .and_then(PartialReflect::try_as_reflect_mut)
            .ok_or("field reflection changed")?
            .set(replacement)
            .map_err(|_| format!("{name}: component rejected the typed replacement"))?;
    }
    Ok(
        json!({"frame":frame,"entity":entity.to_string(),"name":name,"component":registration.type_info().type_path(),"before":before,"after":after}),
    )
}

fn query(world: &World, registry: &TypeRegistry, args: &QueryArgs) -> Result<Value, String> {
    let lookup = |name: &str| lookup_type(registry, name);
    let serialize = |value: &dyn PartialReflect| serde_json::to_value(TypedReflectSerializer::new(value, registry));
    let mut out = serde_json::Map::new();
    out.insert("frame".into(), json!(world.resource::<GameClock>().frame));
    if args.name.is_some() || !args.with.is_empty() {
        // Component ids of `with`; a type that was never inserted matches nothing.
        let mut with = Vec::new();
        let mut never_inserted = false;
        for name in &args.with {
            match world.components().get_id(lookup(name)?.type_id()) {
                Some(id) => with.push(id),
                None => never_inserted = true,
            }
        }
        let wanted = args.components.iter().map(|name| lookup(name)).collect::<Result<Vec<_>, String>>()?;
        let limit = args.limit.unwrap_or(16).min(256);
        let mut entities = Vec::new();
        for entity in world.iter_entities().filter(|_| !never_inserted) {
            let name = entity.get::<Name>().map(Name::as_str);
            if args.name.as_deref().is_some_and(|n| !name.is_some_and(|name| name.contains(n)))
                || !with.iter().all(|id| entity.contains_id(*id))
            {
                continue;
            }
            if entities.len() == limit {
                break;
            }
            let registrations: Vec<&TypeRegistration> = if wanted.is_empty() {
                entity
                    .archetype()
                    .components()
                    .iter()
                    .filter_map(|id| world.components().get_info(*id)?.type_id())
                    .filter_map(|type_id| registry.get(type_id))
                    .collect()
            } else {
                wanted.clone()
            };
            let mut components = serde_json::Map::new();
            // Reflected but not serialisable (asset handles, type ids): named only.
            let mut opaque = Vec::new();
            for registration in registrations {
                let Some(reflect) = registration.data::<ReflectComponent>() else { continue };
                let Some(value) = reflect.reflect(entity) else { continue };
                let key = registration.type_info().type_path_table().short_path();
                let value = match serialize(value.as_partial_reflect()) {
                    Ok(value) => value,
                    Err(error) if !wanted.is_empty() => json!({"error": error.to_string()}),
                    Err(_) => {
                        opaque.push(key);
                        continue;
                    }
                };
                components.insert(key.into(), value);
            }
            let mut found = json!({"entity": entity.id().to_string(), "name": name, "components": components});
            if !opaque.is_empty() {
                found["not_serializable"] = json!(opaque);
            }
            entities.push(found);
        }
        out.insert("entities".into(), Value::Array(entities));
    }
    if !args.resources.is_empty() {
        let mut resources = serde_json::Map::new();
        for name in &args.resources {
            let registration = lookup(name)?;
            let value = world
                .components()
                .get_id(registration.type_id())
                .and_then(|id| world.resource_entities().get(id))
                .and_then(|entity| registration.data::<ReflectComponent>()?.reflect(world.entity(entity)))
                .map_or(Value::Null, |value| {
                    serialize(value.as_partial_reflect()).unwrap_or_else(|error| json!({"error": error.to_string()}))
                });
            resources.insert(name.clone(), value);
        }
        out.insert("resources".into(), Value::Object(resources));
    }
    Ok(Value::Object(out))
}

#[derive(Clone)]
struct GameServer {
    sender: mpsc::SyncSender<Request>,
    slots: Arc<Semaphore>,
}
fn tools() -> Vec<Tool> {
    let buttons = json!({"type":"array","maxItems":14,"uniqueItems":true,"items":{"type":"string","enum":["up","down","left","right","cross","circle","square","triangle","l1","r1","l2","r2","start","select"]}});
    let pair = json!({"type":"array","items":{"type":"number"},"minItems":2,"maxItems":2});
    let mouse = json!({"type":"object","description":"Injected mouse: buttons held (adding to the real mouse and pressing the pad actions they are bound to), position in screen pixels (origin top-left, y down; replaces the cursor), motion [dx,dy] and wheel lines added every tick","properties":{"buttons":{"type":"array","uniqueItems":true,"items":{"type":"string","enum":["left","right","middle","back","forward"]}},"position":pair,"motion":pair,"wheel":{"type":"number"}},"additionalProperties":false});
    [
        ("game_status","Read simulation frame, pause state, screen dimensions, controller state and the mouse.",json!({"type":"object","properties":{},"additionalProperties":false})),
        ("game_screenshot","Capture actual GPU-rendered PNG pixels. Screen is the native-resolution game; window includes scaled output and letterboxing. Pause for a stable frame.",json!({"type":"object","properties":{"target":{"type":"string","enum":["screen","window"],"default":"screen"}},"additionalProperties":false})),
        ("game_step","Hold the selected buttons (and optional mouse) for exactly N simulation ticks, then release injected input and freeze. Game systems must use GameSystems::Game in FixedUpdate.",json!({"type":"object","properties":{"frames":{"type":"integer","minimum":1,"maximum":3600},"buttons":buttons.clone(),"mouse":mouse.clone()},"required":["frames"],"additionalProperties":false})),
        ("game_set_input","Replace the held injected controller state and mouse. An empty array releases all injected buttons; omitting mouse releases the injected mouse. Physical inputs are merged.",json!({"type":"object","properties":{"buttons":buttons,"mouse":mouse},"required":["buttons"],"additionalProperties":false})),
        ("game_query","Read game state as JSON: reflected components of entities whose Name contains `name` and/or that have every component in `with`, plus the listed `resources`. Components and resources must derive Reflect with #[reflect(Component)] / #[reflect(Resource)]. Type names may be short (Transform) or full paths.",json!({"type":"object","properties":{"name":{"type":"string"},"with":{"type":"array","items":{"type":"string"}},"components":{"type":"array","items":{"type":"string"},"description":"Components to return; all reflected ones when omitted"},"resources":{"type":"array","items":{"type":"string"}},"limit":{"type":"integer","minimum":1,"maximum":256,"default":16}},"additionalProperties":false})),
        ("game_set_component","While paused, edit selected named fields of one existing reflected component. Select entity by its id from game_query, or by a unique exact name. fields maps top-level field names to typed JSON values in game_query's format (e.g. Transform translation is [x,y,z]). Validates all fields before applying; returns before/after values without advancing ticks. Unsupported, missing, immutable and ambiguous targets fail without applying the patch. Available only in dev-mcp builds.",json!({"type":"object","properties":{"entity":{"type":"string"},"name":{"type":"string"},"component":{"type":"string"},"fields":{"type":"object","minProperties":1,"maxProperties":64}},"required":["component","fields"],"oneOf":[{"required":["entity"]},{"required":["name"]}],"additionalProperties":false})),
        ("game_pause","Pause or resume continuous simulation. Developer MCP starts paused; rendering and window events continue.",json!({"type":"object","properties":{"paused":{"type":"boolean"}},"required":["paused"],"additionalProperties":false})),
    ].into_iter().map(|(name,description,schema)|Tool::new(name,description,Arc::new(schema.as_object().unwrap().clone()))).collect()
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ErrorData> {
    serde_json::from_value(value).map_err(|error| ErrorData::invalid_params(error.to_string(), None))
}
impl ServerHandler for GameServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"),env!("CARGO_PKG_VERSION")))
            .with_instructions("Native Bevy PlayStation-style game developer tools. Starts paused. Use game_step then game_screenshot for stable observations; game_pause resumes manual play. Screenshots are real GPU output. stdout is MCP only.")
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|tool| tool.name == name)
    }
    // ListToolsResult is non-exhaustive, so it cannot be built with a literal.
    #[allow(clippy::field_reassign_with_default)]
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut result = ListToolsResult::default();
        result.tools = tools();
        Ok(result)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let value = Value::Object(request.arguments.unwrap_or_default());
        let command = match request.name.as_ref() {
            "game_status" => {
                if value != json!({}) {
                    return Err(ErrorData::invalid_params("status takes no arguments", None));
                }
                Command::Status
            }
            "game_screenshot" => Command::Screenshot(parse::<ScreenshotArgs>(value)?.target),
            "game_set_input" => {
                let args = parse::<InputArgs>(value)?;
                let mouse = mouse_arg(&args.mouse)?;
                Command::Input(args.buttons, mouse)
            }
            "game_pause" => Command::Pause(parse::<PauseArgs>(value)?.paused),
            "game_query" => {
                let args = parse::<QueryArgs>(value)?;
                if args.name.is_none() && args.with.is_empty() && args.resources.is_empty() {
                    return Err(ErrorData::invalid_params("give name, with or resources", None));
                }
                Command::Query(args)
            }
            "game_set_component" => {
                let args = parse::<SetComponentArgs>(value)?;
                args.validate().map_err(|e| ErrorData::invalid_params(e, None))?;
                Command::SetComponent(args)
            }
            "game_step" => {
                let args = parse::<StepArgs>(value)?;
                if !(1..=3600).contains(&args.frames) || args.buttons.len() > 14 {
                    return Err(ErrorData::invalid_params("frames must be 1..3600 and buttons at most 14", None));
                }
                let mouse = mouse_arg(&args.mouse)?;
                Command::Step(args, mouse)
            }
            _ => return Err(ErrorData::invalid_params("unknown game tool", None)),
        };
        let validation_error = matches!(&command, Command::SetComponent(_) | Command::Screenshot(Target::Window));
        let _permit = tokio::select! {
            permit=self.slots.acquire()=>permit.map_err(|_|ErrorData::internal_error("game queue closed",None))?,
            _=context.ct.cancelled()=>return Err(ErrorData::internal_error("request cancelled",None)),
        };
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request { command, reply })
            .map_err(|error| ErrorData::internal_error(format!("game queue unavailable: {error}"), None))?;
        let response = tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(120),receiver)=>result
                .map_err(|_|ErrorData::internal_error("game/render response timed out",None))?
                .map_err(|_|ErrorData::internal_error("game stopped before replying",None))?
                .map_err(|error|if validation_error {ErrorData::invalid_params(error,None)} else {ErrorData::internal_error(error,None)})?,
            _=context.ct.cancelled()=>return Err(ErrorData::internal_error("request cancelled",None)),
        };
        Ok(match response {
            Response::Json(value) => CallToolResult::structured(value),
            Response::Image { png, metadata } => CallToolResult::success(vec![
                ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png"),
                ContentBlock::text(metadata.to_string()),
            ]),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modal_capture_interrupts_a_pending_step_without_advancing_it() {
        let (reply, mut receiver) = oneshot::channel();
        let mut control = Control {
            paused: false,
            step: Some(Step { remaining: 12, total: 12, buttons: vec![Button::Cross], mouse: default(), reply }),
        };
        control.interrupt_for_input_focus();
        assert!(control.paused && !control.has_step());
        assert!(matches!(receiver.try_recv().unwrap(),Err(error) if error.contains("Controls menu")));
    }

    #[derive(Component, Reflect)]
    #[reflect(Component)]
    struct Editable {
        position: Vec3,
        amount: u16,
        phase: Phase,
        #[reflect(ignore)]
        private_data: String,
    }
    #[derive(Reflect, Debug, PartialEq)]
    enum Phase {
        Idle,
        Moving { speed: f32 },
    }
    #[derive(Component, Reflect)]
    #[component(immutable)]
    #[reflect(Component)]
    struct Immutable {
        amount: u16,
    }

    fn app() -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<Control>()
            .init_resource::<GameClock>()
            .register_type::<Editable>()
            .register_type::<Immutable>()
            .register_type::<Transform>();
        let entity = app
            .world_mut()
            .spawn((
                Name::new("Player"),
                Editable { position: Vec3::ZERO, amount: 4, phase: Phase::Idle, private_data: "keep me".into() },
                Immutable { amount: 1 },
            ))
            .id();
        (app, entity)
    }
    fn edit(app: &mut App, value: Value) -> Result<Value, String> {
        let registry = app.world().resource::<AppTypeRegistry>().clone();
        set_component(app.world_mut(), &registry.read(), &serde_json::from_value::<SetComponentArgs>(value).unwrap())
    }
    fn patch(fields: Value) -> Value {
        json!({"name":"Player","component":"Editable","fields":fields})
    }

    #[test]
    fn typed_fields_preserve_unselected_and_ignored_data_without_ticking() {
        let (mut app, entity) = app();
        let reply = edit(&mut app, patch(json!({"position":[2.0,3.0,4.0],"phase":{"Moving":{"speed":2.5}}}))).unwrap();
        let state = app.world().get::<Editable>(entity).unwrap();
        assert_eq!(state.position, Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(state.phase, Phase::Moving { speed: 2.5 });
        assert_eq!(state.amount, 4);
        assert_eq!(state.private_data, "keep me");
        assert_eq!(reply["before"]["position"], json!([0.0, 0.0, 0.0]));
        assert_eq!(reply["after"]["position"], json!([2.0, 3.0, 4.0]));
        assert_eq!(app.world().resource::<GameClock>().frame, 0);
    }
    #[test]
    fn rejects_bad_patches_before_changing_any_fields() {
        let (mut app, entity) = app();
        for fields in [
            json!({"amount":7,"position":[1.0,2.0]}),
            json!({"amount":7,"zzz":1}),
            json!({"amount":-1}),
            json!({"amount":70000}),
            json!({"private_data":"overwrite"}),
        ] {
            assert!(edit(&mut app, patch(fields)).is_err());
            let state = app.world().get::<Editable>(entity).unwrap();
            assert_eq!(state.amount, 4);
            assert_eq!(state.position, Vec3::ZERO);
            assert_eq!(state.private_data, "keep me");
        }
    }
    #[test]
    fn edit_requires_pause_and_exact_unambiguous_target() {
        let (mut app, entity) = app();
        app.world_mut().resource_mut::<Control>().paused = false;
        assert!(edit(&mut app, patch(json!({"amount":7}))).unwrap_err().contains("pause"));
        app.world_mut().resource_mut::<Control>().paused = true;
        let (reply, _receiver) = oneshot::channel();
        app.world_mut().resource_mut::<Control>().step =
            Some(Step { remaining: 1, total: 1, buttons: vec![], mouse: default(), reply });
        assert!(edit(&mut app, patch(json!({"amount":7}))).unwrap_err().contains("pause"));
        app.world_mut().resource_mut::<Control>().step = None;
        app.world_mut().spawn(Name::new("Player"));
        assert!(edit(&mut app, patch(json!({"amount":7}))).unwrap_err().contains("not unique"));
        let reply =
            edit(&mut app, json!({"entity":entity.to_string(),"component":"Editable","fields":{"amount":7}})).unwrap();
        assert_eq!(reply["after"]["amount"], 7);
        assert!(edit(&mut app, json!({"name":"Play","component":"Editable","fields":{"amount":8}})).is_err());
        app.world_mut().despawn(entity);
        assert!(
            edit(&mut app, json!({"entity":entity.to_string(),"component":"Editable","fields":{"amount":7}})).is_err()
        );
    }
    #[test]
    fn immutable_and_missing_components_fail_cleanly() {
        let (mut app, _) = app();
        assert!(
            edit(&mut app, json!({"name":"Player","component":"Immutable","fields":{"amount":7}}))
                .unwrap_err()
                .contains("immutable")
        );
        assert!(
            edit(&mut app, json!({"name":"Player","component":"Transform","fields":{"translation":[0,0,0]}})).is_err()
        );
        assert!(edit(&mut app, json!({"name":"Player","component":"unknown","fields":{"amount":7}})).is_err());
    }
    #[test]
    fn read_only_query_still_reports_edited_state() {
        let (mut app, _) = app();
        edit(&mut app, patch(json!({"amount":12}))).unwrap();
        let registry = app.world().resource::<AppTypeRegistry>().read();
        let output = query(
            app.world(),
            &registry,
            &serde_json::from_value(json!({"name":"Player","components":["Editable"]})).unwrap(),
        )
        .unwrap();
        assert_eq!(output["entities"][0]["components"]["Editable"]["amount"], 12);
    }
}

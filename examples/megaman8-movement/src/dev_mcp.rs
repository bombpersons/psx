//! Developer-only stdio MCP bridge. Bevy owns all game and GPU state.
//! The server thread sends bounded commands to the main thread and awaits replies.
use crate::{
    GameClock,
    controller::{Button, ControllerState},
    psx::PsxScreen,
};
use base64::Engine;
use bevy::render::{
    Render, RenderApp, RenderSystems,
    render_resource::{CachedPipelineState, PipelineCache},
    view::ViewTarget,
};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::{RequestContext, RoleServer},
};
use serde::Deserialize;
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
    Input(Vec<Button>),
    Pause(bool),
    Step(StepArgs),
    Screenshot(Target),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepArgs {
    frames: u32,
    #[serde(default)]
    buttons: Vec<Button>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputArgs {
    buttons: Vec<Button>,
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
    reply: Reply,
}
#[derive(Resource)]
pub struct Control {
    pub paused: bool,
    pub buttons: u16,
    step: Option<Step>,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            paused: true,
            buttons: 0,
            step: None,
        }
    }
}
impl Control {
    pub fn has_step(&self) -> bool {
        self.step.is_some()
    }
    pub fn discard_cancelled_step(&mut self) {
        if self
            .step
            .as_ref()
            .is_some_and(|step| step.reply.is_closed())
        {
            self.step = None;
            self.buttons = 0;
            self.paused = true;
        }
    }
    pub fn finish_tick(&mut self, frame: u64) {
        let Some(step) = self.step.as_mut() else {
            return;
        };
        step.remaining -= 1;
        if step.remaining == 0 {
            let step = self.step.take().unwrap();
            self.buttons = 0;
            self.paused = true;
            let _=step.reply.send(Ok(Response::Json(json!({"frame":frame,"advanced_frames":step.total,"buttons":step.buttons,"paused":true}))));
        }
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
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let server = GameServer {
                    sender,
                    slots: Arc::new(Semaphore::new(1)),
                };
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
            .add_systems(
                Render,
                mark_render_ready
                    .after(RenderSystems::Render)
                    .before(RenderSystems::Cleanup),
            );
        app.init_resource::<Control>()
            .insert_resource(Bridge {
                receiver: Mutex::new(receiver),
                closed,
                pending: Vec::new(),
                ready,
            })
            .add_systems(PreUpdate, process_commands);
    }
}

fn mark_render_ready(
    ready: Res<RenderReady>,
    cache: Res<PipelineCache>,
    views: Query<&ViewTarget>,
    mut settled: Local<u32>,
) {
    // A texture may exist before its first camera pass. Wait until the game and
    // display views exist and every queued pipeline has compiled for a few
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
    screen: Res<PsxScreen>,
    clock: Res<GameClock>,
    pad: Res<ControllerState>,
    mut exit: MessageWriter<AppExit>,
    adapter: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
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
        if matches!(&request.command, Command::Screenshot(_))
            && !bridge.ready.0.load(Ordering::Acquire)
        {
            bridge.pending.push(request);
            continue;
        }
        match request.command {
            Command::Status => {
                let renderer=adapter.as_ref().map(|info|json!({"name":info.name,"device_type":format!("{:?}",info.device_type),"backend":format!("{:?}",info.backend)}));
                let _=request.reply.send(Ok(Response::Json(json!({"frame":clock.frame,"paused":control.paused,"screen":{"width":screen.width,"height":screen.height,"region":format!("{:?}",screen.region)},"controller_buttons":pad.buttons(),"renderer":renderer}))));
            }
            Command::Input(buttons) => {
                control.buttons = mask(&buttons);
                let _ = request.reply.send(Ok(Response::Json(
                    json!({"frame":clock.frame,"injected_buttons":buttons}),
                )));
            }
            Command::Pause(paused) => {
                control.paused = paused;
                let _ = request.reply.send(Ok(Response::Json(
                    json!({"frame":clock.frame,"paused":paused}),
                )));
            }
            Command::Step(args) => {
                control.paused = true;
                control.buttons = mask(&args.buttons);
                control.step = Some(Step {
                    remaining: args.frames,
                    total: args.frames,
                    buttons: args.buttons,
                    reply: request.reply,
                });
            }
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

fn mask(buttons: &[Button]) -> u16 {
    buttons.iter().fold(0, |bits, button| bits | button.mask())
}

#[derive(Clone)]
struct GameServer {
    sender: mpsc::SyncSender<Request>,
    slots: Arc<Semaphore>,
}
fn tools() -> Vec<Tool> {
    let buttons = json!({"type":"array","maxItems":14,"uniqueItems":true,"items":{"type":"string","enum":["up","down","left","right","cross","circle","square","triangle","l1","r1","l2","r2","start","select"]}});
    [
        ("game_status","Read simulation frame, pause state, screen dimensions and controller state.",json!({"type":"object","properties":{},"additionalProperties":false})),
        ("game_screenshot","Capture actual GPU-rendered PNG pixels. Screen is the native-resolution game; window includes scaled output and letterboxing. Pause for a stable frame.",json!({"type":"object","properties":{"target":{"type":"string","enum":["screen","window"],"default":"screen"}},"additionalProperties":false})),
        ("game_step","Hold the selected buttons for exactly N simulation ticks, then release injected input and freeze. Game systems must use GameSystems::Game in FixedUpdate.",json!({"type":"object","properties":{"frames":{"type":"integer","minimum":1,"maximum":3600},"buttons":buttons.clone()},"required":["frames"],"additionalProperties":false})),
        ("game_set_input","Replace the held injected controller state. An empty array releases all injected buttons. Physical inputs are merged.",json!({"type":"object","properties":{"buttons":buttons},"required":["buttons"],"additionalProperties":false})),
        ("game_pause","Pause or resume continuous simulation. Developer MCP starts paused; rendering and window events continue.",json!({"type":"object","properties":{"paused":{"type":"boolean"}},"required":["paused"],"additionalProperties":false})),
    ].into_iter().map(|(name,description,schema)|Tool::new(name,description,Arc::new(schema.as_object().unwrap().clone()))).collect()
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ErrorData> {
    serde_json::from_value(value)
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))
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
            "game_set_input" => Command::Input(parse::<InputArgs>(value)?.buttons),
            "game_pause" => Command::Pause(parse::<PauseArgs>(value)?.paused),
            "game_step" => {
                let args = parse::<StepArgs>(value)?;
                if !(1..=3600).contains(&args.frames) || args.buttons.len() > 14 {
                    return Err(ErrorData::invalid_params(
                        "frames must be 1..3600 and buttons at most 14",
                        None,
                    ));
                }
                Command::Step(args)
            }
            _ => return Err(ErrorData::invalid_params("unknown game tool", None)),
        };
        let _permit = tokio::select! {
            permit=self.slots.acquire()=>permit.map_err(|_|ErrorData::internal_error("game queue closed",None))?,
            _=context.ct.cancelled()=>return Err(ErrorData::internal_error("request cancelled",None)),
        };
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(Request { command, reply })
            .map_err(|error| {
                ErrorData::internal_error(format!("game queue unavailable: {error}"), None)
            })?;
        let response = tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(120),receiver)=>result
                .map_err(|_|ErrorData::internal_error("game/render response timed out",None))?
                .map_err(|_|ErrorData::internal_error("game stopped before replying",None))?
                .map_err(|error|ErrorData::internal_error(error,None))?,
            _=context.ct.cancelled()=>return Err(ErrorData::internal_error("request cancelled",None)),
        };
        Ok(match response {
            Response::Json(value) => CallToolResult::structured(value),
            Response::Image { png, metadata } => CallToolResult::success(vec![
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(png),
                    "image/png",
                ),
                ContentBlock::text(metadata.to_string()),
            ]),
        }
        .into())
    }
}

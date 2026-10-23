//! Letting agents join over HTTP: the server switched on and off from the
//! Agents window (Tools > Agents) or `--mcp-listen`, and the window that shows
//! how to connect. The port, the token and whether the server is on are kept
//! in the person's config folder, so the `claude mcp add` command keeps
//! working from one session to the next.
use super::{EditorServer, Servers};
use bevy::prelude::*;
use bevy_egui::egui;
// psx-bevy's small settings files: atomic writes, a size limit.
use engine::settings as psx_bevy_settings;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// The config folder the settings are kept in. Rename it after the game
/// (`tr2-editor`, say) so each project keeps its own port and token.
const APP: &str = "psx-editor";
const FILE: &str = "agents.ron";
/// The MCP server's name in the `claude mcp add` command.
const SERVER_NAME: &str = "editor";

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    on: bool,
    port: u16,
    token: String,
}

impl Default for Saved {
    fn default() -> Self {
        Self { on: false, port: 7878, token: String::new() }
    }
}

/// What the Agents window shows and sets; the server follows `on`, `addr` and
/// `token`.
#[derive(Resource)]
pub struct Agents {
    pub window: bool,
    pub on: bool,
    pub addr: SocketAddr,
    pub token: String,
    /// Keep changes in the config folder; not for one run's `--mcp-listen`
    /// or an editor an agent started with `--mcp`.
    persist: bool,
    /// Where the server is listening, while it is.
    listening: Option<SocketAddr>,
    /// Why it isn't, when starting it failed.
    error: Option<String>,
    /// When an agent last asked something over HTTP.
    pub(super) last_request: Option<Instant>,
}

impl Agents {
    /// From the saved settings (with a token made and kept the first time),
    /// or for this run only: `listen` and `token` from the command line, or
    /// off when an agent started the editor on stdio.
    pub(super) fn new(stdio: bool, listen: Option<SocketAddr>, token: Option<String>) -> Self {
        let path = psx_bevy_settings::config_path(APP, FILE);
        let mut saved: Saved = path
            .as_deref()
            .and_then(|p| psx_bevy_settings::read(p).ok().flatten())
            .and_then(|bytes| ron::de::from_bytes(&bytes).ok())
            .unwrap_or_default();
        let persist = !stdio && listen.is_none() && path.is_some();
        if saved.token.is_empty() {
            saved.token = super::new_token();
        }
        let mut agents = Self {
            window: false,
            on: saved.on && !stdio,
            addr: (Ipv4Addr::LOCALHOST, saved.port).into(),
            token: token.unwrap_or(saved.token),
            persist,
            listening: None,
            error: None,
            last_request: None,
        };
        if let Some(addr) = listen {
            (agents.on, agents.addr) = (true, addr);
        }
        agents.save();
        agents
    }

    fn save(&self) {
        let Some(path) = psx_bevy_settings::config_path(APP, FILE).filter(|_| self.persist) else { return };
        let saved = Saved { on: self.on, port: self.addr.port(), token: self.token.clone() };
        let text = ron::ser::to_string_pretty(&saved, default()).unwrap_or_default();
        if let Err(e) = psx_bevy_settings::write(&path, text.as_bytes()) {
            eprintln!("editor MCP: can't keep the agent settings in {}: {e}", path.display());
        }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.listening.unwrap_or(self.addr))
    }

    fn claude_command(&self) -> String {
        let (name, url, token) = (SERVER_NAME, self.url(), &self.token);
        format!("claude mcp add --transport http {name} {url} --header \"Authorization: Bearer {token}\"")
    }

    /// The server is on and listening.
    pub fn listening(&self) -> bool {
        self.listening.is_some()
    }

    /// An agent asked something in the last few seconds.
    pub fn active(&self) -> bool {
        self.last_request.is_some_and(|t| t.elapsed().as_secs_f32() < 5.0)
    }
}

pub(super) struct Http {
    /// As asked for (`Agents::listening` has the port actually bound).
    addr: SocketAddr,
    token: String,
    stop: CancellationToken,
}

/// Starts, stops or restarts the HTTP server to match the Agents window.
pub(super) fn switch(
    mut agents: ResMut<Agents>,
    mut servers: ResMut<Servers>,
    winit: Option<ResMut<bevy::winit::WinitSettings>>,
) {
    // Requests are answered between frames, so frames keep coming while an
    // agent could ask, even with the window in the background.
    let continuous = servers.stdio || agents.listening.is_some();
    if let Some(mut winit) = winit.filter(|w| (w.unfocused_mode == bevy::winit::UpdateMode::Continuous) != continuous) {
        *winit = if continuous { bevy::winit::WinitSettings::continuous() } else { default() };
    }
    let a = &mut *agents;
    let want = a.on.then(|| (a.addr, a.token.clone()));
    if want == servers.http.as_ref().map(|h| (h.addr, h.token.clone())) {
        return;
    }
    if let Some(h) = servers.http.take() {
        h.stop.cancel();
        a.listening = None;
        eprintln!("editor MCP: stopped serving http://{}/mcp", h.addr);
    }
    a.error = None;
    let Some((addr, token)) = want else { return };
    match start(&mut servers, addr, token.clone()) {
        Ok((bound, stop)) => {
            a.listening = Some(bound);
            servers.http = Some(Http { addr, token, stop });
            eprintln!("editor MCP: http://{bound}/mcp (header \"Authorization: Bearer {}\")", a.token);
        }
        Err(e) => {
            eprintln!("editor MCP: {e}");
            a.error = Some(e);
            a.on = false;
        }
    }
}

/// Binds here, so a port in use is reported at once, and serves on the runtime.
fn start(servers: &mut Servers, addr: SocketAddr, token: String) -> Result<(SocketAddr, CancellationToken), String> {
    if !addr.ip().is_loopback() {
        return Err(format!("{addr} isn't a loopback address; agents can only connect from this computer"));
    }
    let listener = std::net::TcpListener::bind(addr).map_err(|e| format!("can't listen on {addr}: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let server =
        EditorServer { sender: servers.sender.clone(), slots: servers.slots.clone(), testing: false, remote: true };
    let runtime = servers.runtime();
    let _entered = runtime.enter();
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    let stop = CancellationToken::new();
    runtime.spawn(serve(listener, token, server, stop.clone()));
    Ok((bound, stop))
}

async fn serve(listener: tokio::net::TcpListener, token: String, server: EditorServer, stop: CancellationToken) {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
    // The default config only accepts local Host names (against DNS
    // rebinding); with an empty Origin list, any request from a web page is
    // refused too. Stopping ends the sessions' event streams, so shutting down
    // doesn't wait on them.
    let config =
        StreamableHttpServerConfig::default().enforce_origin_validation().with_cancellation_token(stop.clone());
    let service = StreamableHttpService::new(move || Ok(server.clone()), LocalSessionManager::default().into(), config);
    let wanted = format!("Bearer {token}");
    let router = axum::Router::new().nest_service("/mcp", service).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let given = request.headers().get("authorization").map(|v| v.as_bytes());
            let ok = given.is_some_and(|given| same(given, wanted.as_bytes()));
            async move {
                if ok {
                    next.run(request).await
                } else {
                    let status = axum::http::StatusCode::UNAUTHORIZED;
                    axum::response::IntoResponse::into_response((status, "bearer token needed"))
                }
            }
        },
    ));
    if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(stop.cancelled_owned()).await {
        eprintln!("editor MCP over HTTP: {e}");
    }
}

/// Compares in time that doesn't depend on where the bytes differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// The menu bar's note while the server is on; clicking it opens the window.
pub fn indicator(ui: &mut egui::Ui, agents: &mut Agents) {
    if !agents.listening() {
        return;
    }
    let (text, hint) = if agents.active() {
        (egui::RichText::new("• Agent at work").color(egui::Color32::from_rgb(120, 220, 120)), "An agent is working")
    } else {
        (egui::RichText::new("• Agents on"), "Agents can connect (Tools > Agents)")
    };
    if ui.add(egui::Button::new(text).frame(false)).on_hover_text(hint).clicked() {
        agents.window = true;
    }
}

/// Tools > Agents: switch the server on and off, and how to connect to it.
pub fn window(ctx: &egui::Context, agents: &mut Agents) {
    if !agents.window {
        return;
    }
    let (mut open, mut changed) = (true, false);
    egui::Window::new("Agents").open(&mut open).collapsible(false).resizable(false).default_width(420.0).show(
        ctx,
        |ui| {
            let a = &mut *agents;
            ui.label(
                "Let AI agents (Claude Code, say) read this level and build on it while you work. \
                 Each change they make is one undo step, and they never take over your mouse.",
            );
            ui.add_space(4.0);
            changed |= ui.checkbox(&mut a.on, "Let agents connect").changed();
            let (text, colour) = match (&a.listening, &a.error) {
                (Some(_), _) if a.active() => ("An agent is working".into(), egui::Color32::from_rgb(120, 220, 120)),
                (Some(addr), _) => (format!("Listening on {addr}, for this computer only"), ui.visuals().text_color()),
                (None, Some(e)) => (e.clone(), ui.visuals().error_fg_color),
                (None, None) => ("Off".into(), ui.visuals().weak_text_color()),
            };
            ui.colored_label(colour, text);
            ui.separator();
            let mut port = a.addr.port();
            egui::Grid::new("agents").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Port");
                if ui.add(egui::DragValue::new(&mut port).range(1024..=65535)).changed() {
                    a.addr.set_port(port);
                    changed = true;
                }
                ui.end_row();
                let url = a.url();
                ui.label("Address");
                ui.monospace(&url);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(url);
                }
                ui.end_row();
                ui.label("Token");
                ui.monospace(&a.token);
                ui.horizontal(|ui| {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(a.token.clone());
                    }
                    if ui.small_button("New").on_hover_text("Agents with the old token are disconnected").clicked() {
                        a.token = super::new_token();
                        changed = true;
                    }
                });
                ui.end_row();
            });
            ui.separator();
            ui.label("To add the editor to Claude Code:");
            let command = a.claude_command();
            ui.add(egui::Label::new(egui::RichText::new(&command).monospace().size(10.0)).wrap());
            if ui.button("Copy command").clicked() {
                ui.ctx().copy_text(command);
            }
        },
    );
    agents.window = open;
    if changed {
        agents.save();
    }
}

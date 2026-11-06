//! The executable: the game in a window (`cargo run`), or the port as psx
//! drives it (`--psx`, the port protocol, for `psx port check` and psx web)
//! and a scenario recording drawn here (`--replay REC --frames DIR`), both
//! psx_scenario::replay.
//!
//! At the start it imports the player's disc (disc-import), then runs
//! [`game::Game`] at the original's tick rate with the pad from the keyboard
//! and gamepads (shell), and draws it ([`view`], with renderer). F1 and F2
//! open the controls and display menus (menus).
//!
//! `--disc PATH` imports from that disc; `--skip-import` starts without one
//! (the game needs nothing from it yet); `--software` draws with a software
//! adapter.
mod view;

use disc_import::{DiscImport, State};
use menus::Menus;
use psx_scenario::{Pad, Parameters, Port};
use renderer::{Font, Options, Renderer, Scene};
use shell::{App, Config, Shell};
use std::path::PathBuf;
use std::process::ExitCode;

/// The game's folder name for the player's settings and imported assets.
const NAME: &str = env!("CARGO_BIN_NAME");

/// The original's display size: what the PS1 look draws at, and what
/// `--replay` frames are.
const SCREEN: [u32; 2] = [320, 240];

/// Where the import goes: `game/assets/imported` while developing, the
/// player's data folder in a release build.
fn imported() -> PathBuf {
    if cfg!(debug_assertions) {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/imported"))
    } else {
        disc_import::data_dir(NAME).unwrap_or_else(|| PathBuf::from("imported"))
    }
}

fn software(args: &[String]) -> bool {
    args.iter().any(|a| a == "--software")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = psx_scenario::replay::main(args.clone(), || Shown::new(software(&args))) {
        return code;
    }
    let config = Config { name: NAME.into(), title: game::TITLE.into(), tick_hz: game::tick_hz(), size: [960, 720] };
    match shell::run(config, Play::new(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The game with a window.
struct Play {
    args: Vec<String>,
    menus: Menus,
    renderer: Option<Renderer>,
    font: Option<Font>,
    /// None with `--skip-import`.
    import: Option<DiscImport>,
    /// The game and its view, once the import is ready.
    playing: Option<(game::Game, view::View)>,
}

impl Play {
    fn new(args: Vec<String>) -> Self {
        Self { args, menus: Menus::new(NAME), renderer: None, font: None, import: None, playing: None }
    }
}

impl App for Play {
    fn start(&mut self, shell: &mut Shell) -> Result<(), String> {
        let options = Options { game: SCREEN, display: self.menus.load_display(), software: software(&self.args) };
        let mut renderer = Renderer::new(shell.window().clone(), shell.size(), options)?;
        self.font = Some(Font::builtin(&mut renderer));
        self.renderer = Some(renderer);
        if !self.args.iter().any(|a| a == "--skip-import") {
            self.import = Some(DiscImport::new(disc_import::Options::new(NAME, &self.args, imported())));
        }
        Ok(())
    }

    fn tick(&mut self, shell: &mut Shell) {
        let Some(renderer) = &mut self.renderer else { return };
        if self.menus.tick(shell, renderer) {
            return;
        }
        if let Some(import) = &mut self.import {
            import.update();
            if !import.ready() {
                return;
            }
        }
        if self.playing.is_none() {
            match game::Game::start() {
                Ok(game) => self.playing = Some((game, view::View::new(renderer))),
                Err(e) => {
                    eprintln!("{e}");
                    shell.exit();
                    return;
                }
            }
        }
        if let Some((game, _)) = &mut self.playing {
            game.tick(&shell.pad());
        }
    }

    fn draw(&mut self, shell: &mut Shell, _blend: f32) {
        let (Some(renderer), Some(font)) = (&mut self.renderer, &self.font) else { return };
        let mut scene = match &self.playing {
            Some((game, view)) => view.scene(game, Some(font)),
            None => Scene::default(),
        };
        if let (None, Some(import)) = (&self.playing, &self.import) {
            import.draw(&mut scene, font, renderer.white(), renderer.layout_size());
        }
        self.menus.draw(&mut scene, font, renderer, shell);
        if let Err(e) = renderer.render(&scene) {
            eprintln!("{e}");
        }
    }

    fn resized(&mut self, _shell: &mut Shell, size: [u32; 2]) {
        if let Some(r) = &mut self.renderer {
            r.resize(size[0], size[1]);
        }
    }

    fn dropped(&mut self, _shell: &mut Shell, path: PathBuf) {
        if let Some(import) = &mut self.import
            && !matches!(import.state(), State::Importing { .. })
            && import.offer(&path)
        {
            // Start again once the new import is ready.
            self.playing = None;
        }
    }
}

/// The game as psx drives it (`--psx`, `--replay`): the port's ticks, and
/// each screen at [`SCREEN`] in the PS1 look. The renderer starts at the
/// first screen asked for, so runs that only observe (`psx port check`)
/// draw nothing.
struct Shown {
    game: game::Game,
    software: bool,
    /// None until a screen is asked for; then the renderer and the view, or
    /// None when there is no adapter.
    drawn: Option<Option<(Renderer, view::View)>>,
}

impl Shown {
    fn new(software: bool) -> Self {
        Self { game: game::Game::headless(), software, drawn: None }
    }
}

impl Port for Shown {
    type Observed = <game::Game as Port>::Observed;

    fn kinds(&self) -> Vec<String> {
        self.game.kinds()
    }

    fn launch(&mut self, kind: &str, p: &Parameters) -> Result<(), String> {
        self.game.launch(kind, p)
    }

    fn tick(&mut self, pad: &Pad) {
        self.game.tick(pad);
    }

    fn observe(&mut self) -> Result<Self::Observed, String> {
        self.game.observe()
    }

    fn screen(&mut self) -> Option<psx_scenario::Frame> {
        let software = self.software;
        let (renderer, view) = self
            .drawn
            .get_or_insert_with(|| match Renderer::headless(Options { game: SCREEN, software, ..Default::default() }) {
                Ok(mut r) => {
                    let view = view::View::new(&mut r);
                    Some((r, view))
                }
                Err(e) => {
                    eprintln!("replay: {e}; frames are not drawn");
                    None
                }
            })
            .as_mut()?;
        match renderer.capture(&view.scene(&self.game, None)) {
            Ok(frame) => Some(frame),
            Err(e) => {
                eprintln!("replay: {e}");
                None
            }
        }
    }
}

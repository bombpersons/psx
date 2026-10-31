//! The game's data, imported from the player's own disc by the game itself.
//!
//! A port ships without the original's data. On start, [`GameDataPlugin`]
//! looks for an earlier import (`assets/imported/import.json`, made by the
//! current [`import::VERSION`]); without one it looks for a disc image next to
//! the game, runs [`import::import`] on it with a progress screen, and then
//! starts the game. With no disc it shows a "put your disc next to the game"
//! screen and keeps looking. Players need neither the psx toolkit nor a
//! separate export step.
//!
//! Game code that needs imported files starts on `OnEnter(GameData::Ready)`;
//! [`GameSystems::Game`] only runs once the data is ready. The plugin's
//! default (tests and sandboxes, built from hand-made assets) imports nothing and
//! starts ready; `src/main.rs` turns importing on with [`GameDataPlugin::from_args`].
//!
//! Where the disc is looked for: `--disc PATH` (a file or folder), then the
//! asset base folder (this crate under `cargo run`), its parent and
//! grandparent (the workspace root), and the executable's folder. `.cue`,
//! `.chd`, `.iso`, `.bin` and `.img` are accepted; a `.cue` is preferred over
//! the `.bin` files it lists. `--skip-import` starts without importing.
//!
//! This is a working example of the flow, the game's to change: restyle the
//! screens (plain Bevy UI at window resolution), change when and where it
//! looks, or leave the plugin out while the port uses no imported data.
use bevy::asset::io::file::FileAssetReader;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use import::{DiscReader, ImportProgress};
use psx_bevy::launcher::Args;
use psx_bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Whether the game's data is available.
#[derive(States, Clone, Debug, Default, PartialEq, Eq, Hash, Reflect)]
pub enum GameData {
    /// Looking for an earlier import, then for a disc.
    Checking,
    /// No import and no accepted disc; looking again every second.
    NeedDisc,
    /// The importer is running.
    Importing,
    /// The importer failed; Enter or Start tries again.
    Failed,
    /// Imported data is in place, or this app imports nothing.
    #[default]
    Ready,
}

/// Imports the game's data from the player's disc when needed.
#[derive(Default)]
pub struct GameDataPlugin {
    /// Import from a disc on start. Off by default (tests, sandboxes).
    pub import: bool,
    /// `--disc PATH`: where to look first.
    pub disc: Option<PathBuf>,
    /// Show the disc and progress screens (off when headless).
    pub screens: bool,
    /// Where imported files go; `None` is [`imported_dir`]. Tests use a scratch folder.
    pub out: Option<PathBuf>,
}

impl GameDataPlugin {
    /// For the game's executable: imports unless `--skip-import`, honours
    /// `--disc PATH` and shows the screens unless `--headless`.
    pub fn from_args() -> Self {
        let args = Args::from_env();
        Self {
            import: !args.flag("--skip-import"),
            disc: args.value("--disc").map(PathBuf::from),
            screens: !args.flag("--headless"),
            out: None,
        }
    }
}

impl Plugin for GameDataPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<StatesPlugin>() {
            app.add_plugins(StatesPlugin);
        }
        app.configure_sets(FixedUpdate, GameSystems::Game.run_if(in_state(GameData::Ready)));
        if !self.import {
            app.insert_state(GameData::Ready);
            return;
        }
        app.insert_state(GameData::Checking)
            .insert_resource(Search {
                dirs: search_dirs(self.disc.as_deref()),
                out: self.out.clone().unwrap_or_else(imported_dir),
            })
            .init_resource::<ImportStatus>()
            .insert_resource(Rescan(Timer::new(Duration::from_secs(1), TimerMode::Repeating)))
            .add_systems(OnEnter(GameData::Checking), check)
            .add_systems(Update, rescan.run_if(in_state(GameData::NeedDisc)))
            .add_systems(Update, poll_job.run_if(in_state(GameData::Importing)))
            .add_systems(Update, retry.run_if(in_state(GameData::Failed)));
        if self.screens {
            screens::plugin(app);
        }
    }
}

/// Written last into the import folder: which importer made it, from which disc.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ImportStamp {
    pub version: u32,
    pub serial: String,
    pub disc: String,
}

pub const STAMP_FILE: &str = "import.json";

/// `assets/imported/`: where imported files go. Loads name them
/// `imported/...`, and crates layered on the game's assets find them too.
pub fn imported_dir() -> PathBuf {
    FileAssetReader::get_base_path().join("assets/imported")
}

/// The stamp of a complete import in `dir`, if any.
pub fn read_stamp(dir: &Path) -> Option<ImportStamp> {
    serde_json::from_slice(&std::fs::read(dir.join(STAMP_FILE)).ok()?).ok()
}

/// Folders searched for a disc, most specific first.
pub fn search_dirs(disc_arg: Option<&Path>) -> Vec<PathBuf> {
    let base = FileAssetReader::get_base_path();
    let mut dirs: Vec<PathBuf> = disc_arg.map(Path::to_path_buf).into_iter().collect();
    dirs.extend(base.ancestors().take(3).map(Path::to_path_buf));
    dirs.extend(std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)));
    let mut unique = Vec::new();
    for dir in dirs {
        let dir = dir.canonicalize().unwrap_or(dir);
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// Disc images in `dirs` (a listed file counts as itself): CUE sheets and
/// CHDs first, so a `.bin` with a `.cue` is opened through its sheet.
pub fn candidates(dirs: &[PathBuf]) -> Vec<PathBuf> {
    const ORDER: [&str; 5] = ["cue", "chd", "iso", "bin", "img"];
    let rank = |path: &Path| {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        ORDER.iter().position(|e| *e == ext)
    };
    let mut found = Vec::new();
    for dir in dirs {
        if dir.is_file() {
            found.push(dir.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut here: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && rank(p).is_some()).collect();
        here.sort_by_key(|p| (rank(p), p.clone()));
        found.extend(here);
    }
    found
}

/// Looking for a disc: the first accepted one, or what was found instead.
pub fn find_disc(dirs: &[PathBuf]) -> Result<DiscReader, Vec<(PathBuf, String)>> {
    let mut rejected = Vec::new();
    for path in candidates(dirs) {
        match DiscReader::open(&path) {
            Ok(reader) if reader.accepted() => return Ok(reader),
            Ok(reader) => rejected.push((path, format!("{} is not a supported version", reader.serial))),
            // A .bin that is one track of a .cue does not open alone; only report other failures.
            Err(e) if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("bin")) => rejected.push((path, e)),
            Err(_) => {}
        }
    }
    Err(rejected)
}

/// Runs the importer into a fresh folder beside `out` and swaps it in when
/// it succeeds, so an interrupted import never leaves a stamp over half the data.
pub fn run_import(reader: &DiscReader, out: &Path, progress: &ImportProgress) -> Result<ImportStamp, String> {
    let partial = out.with_extension("partial");
    let io = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    if partial.exists() {
        std::fs::remove_dir_all(&partial).map_err(|e| io(&partial, e))?;
    }
    std::fs::create_dir_all(&partial).map_err(|e| io(&partial, e))?;
    progress.set(0.0, "Starting");
    import::import(reader, &partial, progress)?;
    let stamp = ImportStamp { version: import::VERSION, serial: reader.serial.clone(), disc: file_name(&reader.path) };
    let json = serde_json::to_vec_pretty(&stamp).map_err(|e| e.to_string())?;
    std::fs::write(partial.join(STAMP_FILE), json).map_err(|e| io(&partial, e))?;
    if out.exists() {
        std::fs::remove_dir_all(out).map_err(|e| io(out, e))?;
    }
    std::fs::rename(&partial, out).map_err(|e| io(out, e))?;
    progress.set(1.0, "Done");
    Ok(stamp)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

#[derive(Resource)]
struct Search {
    dirs: Vec<PathBuf>,
    out: PathBuf,
}

/// What the screens show, and the running import.
#[derive(Resource, Default)]
struct ImportStatus {
    rejected: Vec<(PathBuf, String)>,
    disc: String,
    progress: ImportProgress,
    error: String,
    job: Option<std::thread::JoinHandle<Result<ImportStamp, String>>>,
}

#[derive(Resource)]
struct Rescan(Timer);

fn check(search: Res<Search>, mut status: ResMut<ImportStatus>, mut next: ResMut<NextState<GameData>>) {
    if read_stamp(&search.out).is_some_and(|stamp| stamp.version == import::VERSION) {
        next.set(GameData::Ready);
    } else {
        start_or_wait(&search, &mut status, &mut next);
    }
}

fn start_or_wait(search: &Search, status: &mut ImportStatus, next: &mut NextState<GameData>) {
    match find_disc(&search.dirs) {
        Ok(reader) => {
            info!("importing game data from {}", reader.path.display());
            status.disc = file_name(&reader.path);
            status.progress = ImportProgress::default();
            let (out, progress) = (search.out.clone(), status.progress.clone());
            status.job = Some(std::thread::spawn(move || run_import(&reader, &out, &progress)));
            next.set(GameData::Importing);
        }
        Err(rejected) => {
            status.rejected = rejected;
            next.set(GameData::NeedDisc);
        }
    }
}

fn rescan(
    time: Res<Time>,
    mut timer: ResMut<Rescan>,
    search: Res<Search>,
    mut status: ResMut<ImportStatus>,
    mut next: ResMut<NextState<GameData>>,
) {
    if timer.0.tick(time.delta()).just_finished() {
        start_or_wait(&search, &mut status, &mut next);
    }
}

fn poll_job(mut status: ResMut<ImportStatus>, mut next: ResMut<NextState<GameData>>) {
    if !status.job.as_ref().is_some_and(|job| job.is_finished()) {
        return;
    }
    let result = status.job.take().unwrap().join().unwrap_or_else(|_| Err("the importer panicked".into()));
    match result {
        Ok(stamp) => {
            info!("imported game data from {} ({})", stamp.disc, stamp.serial);
            next.set(GameData::Ready);
        }
        Err(e) => {
            error!("import failed: {e}");
            status.error = e;
            next.set(GameData::Failed);
        }
    }
}

fn retry(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    pads: Query<&Gamepad>,
    search: Res<Search>,
    mut status: ResMut<ImportStatus>,
    mut next: ResMut<NextState<GameData>>,
) {
    let key = keys.is_some_and(|keys| keys.just_pressed(KeyCode::Enter));
    if key || pads.iter().any(|pad| pad.just_pressed(GamepadButton::Start)) {
        start_or_wait(&search, &mut status, &mut next);
    }
}

/// The screens shown until the data is ready, over the (still empty) game.
mod screens {
    use super::*;
    use psx_bevy::psx::SCREEN_PASS_ORDER;

    #[derive(Component)]
    struct Root;
    #[derive(Component)]
    struct Heading;
    #[derive(Component)]
    struct Body;
    #[derive(Component)]
    struct Bar;
    #[derive(Component)]
    struct Track;

    const BACKGROUND: Color = Color::srgb(0.03, 0.04, 0.10);
    const TEXT: Color = Color::srgb(0.88, 0.90, 0.96);
    const DIM: Color = Color::srgb(0.60, 0.65, 0.76);
    const ACCENT: Color = Color::srgb(0.30, 0.65, 1.0);

    pub fn plugin(app: &mut App) {
        app.add_systems(Startup, spawn)
            .add_systems(Update, refresh.run_if(not(in_state(GameData::Ready))))
            .add_systems(OnEnter(GameData::Ready), despawn);
    }

    fn spawn(mut commands: Commands) {
        let camera = commands
            .spawn((
                Name::new("Game data camera"),
                Camera2d,
                Camera { order: SCREEN_PASS_ORDER + 10, clear_color: ClearColorConfig::None, ..default() },
                // Same target as the PS1 screen pass, which draws without MSAA.
                Msaa::Off,
                Root,
            ))
            .id();
        commands
            .spawn((
                Name::new("Game data screen"),
                Root,
                UiTargetCamera(camera),
                Node {
                    width: percent(100),
                    height: percent(100),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    padding: UiRect::horizontal(percent(10)),
                    row_gap: px(20),
                    ..default()
                },
                BackgroundColor(BACKGROUND),
            ))
            .with_children(|root| {
                root.spawn((Heading, Text::default(), TextFont::from_font_size(34.0), TextColor(TEXT)));
                root.spawn((Body, Text::default(), TextFont::from_font_size(18.0), TextColor(DIM)));
                root.spawn((
                    Track,
                    Node { width: percent(100), height: px(8), ..default() },
                    BackgroundColor(DIM.with_alpha(0.15)),
                ))
                .with_child((
                    Bar,
                    Node { width: percent(0), height: percent(100), ..default() },
                    BackgroundColor(ACCENT),
                ));
            });
    }

    fn refresh(
        state: Res<State<GameData>>,
        search: Res<Search>,
        status: Res<ImportStatus>,
        mut heading: Single<&mut Text, (With<Heading>, Without<Body>)>,
        mut body: Single<&mut Text, (With<Body>, Without<Heading>)>,
        mut bar: Single<&mut Node, With<Bar>>,
        mut track: Single<&mut Visibility, With<Track>>,
    ) {
        let (head, text, fraction) = match state.get() {
            GameData::Checking => ("Checking game data".to_string(), String::new(), 0.0),
            GameData::NeedDisc => {
                // The workspace root under `cargo run` (where `psx template create`
                // put the disc), else the first folder searched.
                let folders = search.dirs.iter().filter(|d| d.is_dir());
                let folder = folders.clone().find(|d| d.join("Cargo.lock").exists()).or(folders.clone().next());
                let folder = folder.map_or_else(String::new, |d| d.display().to_string());
                let mut text = format!(
                    "This game uses the data from your own copy of {}. Put its disc image \
                     (a .cue with its .bin files, a .chd or an .iso) in this folder:\n\n{folder}\n\n\
                     It will be found automatically; there is no need to restart.",
                    import::GAME
                );
                for (path, why) in &status.rejected {
                    text += &format!("\n\nFound {}, but {why}.", file_name(path));
                }
                ("Put your disc next to the game".to_string(), text, 0.0)
            }
            GameData::Importing => {
                let (fraction, step) = status.progress.get();
                (format!("Importing from {}", status.disc), format!("{step}\n\nThis only happens once."), fraction)
            }
            GameData::Failed => {
                ("Import failed".to_string(), format!("{}\n\nPress Enter or Start to try again.", status.error), 0.0)
            }
            GameData::Ready => return,
        };
        if heading.0 != head {
            heading.0 = head;
        }
        if body.0 != text {
            body.0 = text;
        }
        bar.width = percent(fraction * 100.0);
        **track = if *state.get() == GameData::Importing { Visibility::Inherited } else { Visibility::Hidden };
    }

    fn despawn(mut commands: Commands, roots: Query<Entity, With<Root>>) {
        for entity in &roots {
            commands.entity(entity).despawn();
        }
    }
}

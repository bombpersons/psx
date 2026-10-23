//! The disc import protocol, headless: a tiny generated ISO stands in for the
//! player's disc. Keep these when replacing the demo; adjust the disc to one
//! `import::SERIALS` accepts once the list is filled in.
use engine::testing::headless_app;
use game::disc::{GameData, GameDataPlugin, ImportStamp, STAMP_FILE, read_stamp};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("game-disc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_disc(dir: &Path) -> PathBuf {
    let cnf: &[u8] = b"BOOT = cdrom:\\SLUS_000.01;1\r\n";
    let files: [(&str, &[u8]); 2] = [("SYSTEM.CNF", cnf), ("SLUS_000.01", &[0; 2048])];
    let path = dir.join("game.iso");
    std::fs::write(&path, import::test_disc::iso("TEST_DISC", &files)).unwrap();
    path
}

fn app(disc: Option<&Path>, out: &Path) -> bevy::app::App {
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(GameDataPlugin {
        import: true,
        disc: disc.map(Path::to_path_buf),
        screens: false,
        out: Some(out.into()),
    });
    app
}

fn state(app: &bevy::app::App) -> GameData {
    app.world().resource::<bevy::state::state::State<GameData>>().get().clone()
}

/// Updates until the state settles on one of `until` (the import runs on a thread).
fn run_until(app: &mut bevy::app::App, until: &[GameData]) -> GameData {
    for _ in 0..500 {
        app.update();
        if until.contains(&state(app)) {
            return state(app);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("stuck in {:?}", state(app));
}

#[test]
fn imports_from_a_disc_then_starts_without_it() {
    let dir = scratch("import");
    let disc = write_disc(&dir);
    let out = dir.join("imported");

    let mut first = app(Some(&disc), &out);
    assert_eq!(run_until(&mut first, &[GameData::Ready, GameData::NeedDisc, GameData::Failed]), GameData::Ready);
    let stamp = read_stamp(&out).expect("stamp written");
    assert_eq!(stamp, ImportStamp { version: import::VERSION, serial: "SLUS_000.01".into(), disc: "game.iso".into() });
    assert!(out.join("disc.ron").exists());
    assert!(!out.with_extension("partial").exists());

    // The disc is gone: the earlier import is enough.
    std::fs::remove_file(&disc).unwrap();
    let mut second = app(None, &out);
    assert_eq!(run_until(&mut second, &[GameData::Ready, GameData::NeedDisc]), GameData::Ready);
}

#[test]
fn asks_for_the_disc_when_there_is_none_and_redoes_old_imports() {
    let dir = scratch("need-disc");
    let out = dir.join("imported");
    let empty = dir.join("no-disc-here");
    std::fs::create_dir_all(&empty).unwrap();

    let mut waiting = app(Some(&empty), &out);
    assert_eq!(run_until(&mut waiting, &[GameData::NeedDisc, GameData::Ready]), GameData::NeedDisc);

    // An import from an older importer version is redone once a disc appears.
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("stale.txt"), "old").unwrap();
    let old = ImportStamp { version: import::VERSION.wrapping_sub(1), serial: "X".into(), disc: "x.iso".into() };
    std::fs::write(out.join(STAMP_FILE), serde_json::to_vec(&old).unwrap()).unwrap();
    write_disc(&empty);
    let mut redo = app(Some(&empty), &out);
    assert_eq!(run_until(&mut redo, &[GameData::Ready, GameData::Failed]), GameData::Ready);
    assert_eq!(read_stamp(&out).unwrap().version, import::VERSION);
    assert!(!out.join("stale.txt").exists(), "a new import replaces the old folder");
}

#[test]
fn without_importing_the_game_starts_ready() {
    let mut app = headless_app(game::tick_rate(game::REGION));
    app.add_plugins(GameDataPlugin::default());
    app.update();
    assert_eq!(state(&app), GameData::Ready);
}

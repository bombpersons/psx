use super::*;
use crate::{
    controller::{Button, ControllerState},
    pointer::Hitboxes,
    shell::{ShellPlugin, TickRate},
};
use bevy::time::TimeUpdateStrategy;
use std::path::Path;

fn fixture(path: &Path, overrides: DisplayOverrides) -> App {
    let tick = TickRate::default();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(PsxScreen::default())
        .insert_resource(PsxLook::default())
        .init_resource::<PsxAspect>()
        .add_plugins((
            ShellPlugin { tick },
            DisplayPlugin {
                path: Some(path.into()),
                screens: crate::launcher::screen_choices(UVec2::new(320, 240)),
                overrides,
                render_ui: false,
                ..default()
            },
        ))
        .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period()))
        .init_resource::<ButtonInput<KeyCode>>()
        .init_resource::<ButtonInput<MouseButton>>();
    app.world_mut().spawn((Window::default(), PrimaryWindow));
    frame(&mut app);
    frame(&mut app);
    app
}
fn frame(app: &mut App) {
    app.update();
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
}
fn tap(app: &mut App, code: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(code);
    frame(app);
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(code);
    frame(app);
}
fn directory(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "psx-display-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn live(app: &mut App) -> DisplaySettings {
    let world = app.world_mut();
    let window = world.query_filtered::<&Window, With<PrimaryWindow>>().single(world).ok().cloned();
    let aspect = *world.resource::<PsxAspect>();
    DisplaySettings::current(world.resource::<PsxScreen>(), aspect, world.resource::<PsxLook>(), window.as_ref())
}
fn choose(app: &mut App, row: Row) {
    app.world_mut().resource_mut::<DisplayMenu>().select(row);
    tap(app, KeyCode::Enter);
}

#[test]
fn edits_preview_live_and_discard_restores_them() {
    let dir = directory("discard");
    let path = dir.join("display.json");
    let mut app = fixture(&path, default());
    let before = live(&mut app);
    tap(&mut app, KeyCode::F2);
    assert!(app.world().resource::<DisplayMenu>().is_open());
    tap(&mut app, KeyCode::ArrowRight);
    assert_eq!(live(&mut app).screen, [426, 240], "Resolution previews at once");
    choose(&mut app, Row::Dither);
    choose(&mut app, Row::Fullscreen);
    let edited = live(&mut app);
    assert!(!edited.look.dither && edited.fullscreen);
    tap(&mut app, KeyCode::Escape);
    assert!(!app.world().resource::<DisplayMenu>().is_open());
    assert_eq!(live(&mut app), before);
    assert!(!path.exists(), "Discard writes nothing");
}

#[test]
fn saved_settings_load_on_the_next_start() {
    let dir = directory("save");
    let path = dir.join("display.json");
    let mut app = fixture(&path, default());
    tap(&mut app, KeyCode::F2);
    tap(&mut app, KeyCode::ArrowLeft);
    assert_eq!(live(&mut app).screen, [640, 480], "Left wraps to the last choice");
    choose(&mut app, Row::Quantize);
    choose(&mut app, Row::Save);
    assert!(!app.world().resource::<DisplayMenu>().is_open());
    let saved = live(&mut app);
    assert!(path.exists());
    // Enter also starts the game (Start): closing must not leak it into gameplay.
    frame(&mut app);
    assert!(!app.world().resource::<ControllerState>().held(Button::Start));

    let mut next = fixture(&path, default());
    assert_eq!(live(&mut next), saved);
    tap(&mut next, KeyCode::F2);
    choose(&mut next, Row::Restore);
    assert_eq!(live(&mut next).screen, [320, 240], "Restore defaults returns to the game's own setup");
    assert!(live(&mut next).look.quantize);
}

#[test]
fn command_line_overrides_win_for_the_session() {
    let dir = directory("overrides");
    let path = dir.join("display.json");
    let store = DisplayStore::new(Some(path.clone()));
    store
        .save(&DisplaySettings { screen: [426, 240], aspect: default(), fullscreen: false, look: PsxLook::default() })
        .unwrap();
    let overrides = DisplayOverrides { fullscreen: Some(true), dither: Some(false), ..default() };
    let mut app = fixture(&path, overrides);
    let settings = live(&mut app);
    assert_eq!(settings.screen, [426, 240]);
    assert!(settings.fullscreen && !settings.look.dither && settings.look.quantize);
}

#[test]
fn unreadable_files_are_reported_and_left_alone() {
    let dir = directory("bad");
    let path = dir.join("display.json");
    std::fs::write(&path, b"{\"version\":1,\"display\":{\"screen\":[0,240]}}").unwrap();
    let mut app = fixture(&path, default());
    assert_eq!(live(&mut app).screen, [320, 240]);
    assert!(app.world().resource::<DisplayStore>().load_error.is_some());
    assert!(std::fs::read(&path).unwrap().starts_with(b"{\"version\""));
    assert!(
        DisplaySettings { screen: [8192, 240], aspect: default(), fullscreen: false, look: default() }
            .validate()
            .is_err()
    );
}

#[test]
fn fullscreen_is_borderless() {
    let mut window = Window::default();
    set_fullscreen(&mut window, true);
    assert_eq!(window.mode, WindowMode::BorderlessFullscreen(MonitorSelection::Current));
    assert!(is_fullscreen(&window));
    set_fullscreen(&mut window, false);
    assert!(!is_fullscreen(&window));
}

#[test]
fn match_window_aspect_follows_the_window_and_back() {
    let dir = directory("aspect");
    let path = dir.join("display.json");
    let mut app = fixture(&path, default());
    let window = app.world_mut().query_filtered::<Entity, With<PrimaryWindow>>().single(app.world()).unwrap();
    app.world_mut().get_mut::<Window>(window).unwrap().resolution.set(1000.0, 500.0);
    tap(&mut app, KeyCode::F2);
    choose(&mut app, Row::Aspect);
    assert_eq!(live(&mut app).aspect, PsxAspect::Window);
    assert_eq!(live(&mut app).screen, [480, 240], "2:1 window, height kept");
    // Resolution now chooses only the height.
    choose(&mut app, Row::Resolution);
    assert_eq!(live(&mut app).screen, [960, 480]);
    choose(&mut app, Row::Resolution);
    assert_eq!(live(&mut app).screen, [480, 240], "heights wrap");
    choose(&mut app, Row::Aspect);
    assert_eq!(live(&mut app).screen, [320, 240], "Fixed returns to a listed size of that height");
    // Saved files from before the option exist without the field.
    std::fs::write(&path, br#"{"version":1,"display":{"screen":[320,240],"fullscreen":false,"look":{"vertex_snap":true,"dither":true,"quantize":true}}}"#).unwrap();
    assert_eq!(DisplayStore::new(Some(path)).load().unwrap().unwrap().aspect, PsxAspect::Fixed);
}

/// Rows stacked 10 pixels apart from the top of the screen, standing in for
/// the view's layout.
fn rows_at_top(app: &mut App) {
    let row = |i: usize| Rect::new(0.0, i as f32 * 10.0, 320.0, i as f32 * 10.0 + 10.0);
    let rows = Row::ALL.iter().enumerate().map(|(i, r)| (row(i), Target::Row(*r)));
    // `<` at x 102..114, `>` at 198..210, as the view lays them out.
    let arrows = Row::ALL.iter().enumerate().filter(|(_, r)| r.is_setting()).flat_map(|(i, r)| {
        [
            (Rect::new(102.0, row(i).min.y, 114.0, row(i).max.y), Target::Step(*r, false)),
            (Rect::new(198.0, row(i).min.y, 210.0, row(i).max.y), Target::Step(*r, true)),
        ]
    });
    app.world_mut().resource_mut::<Hitboxes<Target>>().0 = rows.chain(arrows).collect();
}
/// Moves the cursor to screen pixel `at` of the current screen size,
/// through the window's letterbox (the inverse of `cursor_to_screen`).
fn point(app: &mut App, at: Vec2) {
    let screen = app.world().resource::<PsxScreen>().size().as_vec2();
    let world = app.world_mut();
    let mut window = world.query_filtered::<&mut Window, With<PrimaryWindow>>().single_mut(world).unwrap();
    let size = window.size();
    let scale = crate::psx::fit_scale(size, screen);
    window.set_cursor_position(Some((at - screen / 2.0) * scale + size / 2.0));
    frame(app);
}
fn click(app: &mut App, button: MouseButton) {
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(button);
    frame(app);
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().release(button);
    frame(app);
}

#[test]
fn the_mouse_selects_changes_and_discards() {
    let dir = directory("mouse");
    let mut app = fixture(&dir.join("display.json"), default());
    tap(&mut app, KeyCode::F2);
    rows_at_top(&mut app);
    point(&mut app, Vec2::new(40.0, Row::Dither as usize as f32 * 10.0 + 5.0));
    assert_eq!(app.world().resource::<DisplayMenu>().row(), Row::Dither, "hover selects");
    click(&mut app, MouseButton::Left);
    assert!(!live(&mut app).look.dither, "click changes the hovered row");
    click(&mut app, MouseButton::Right);
    assert!(!app.world().resource::<DisplayMenu>().is_open());
    assert!(live(&mut app).look.dither, "right click discards");
}

#[test]
fn clicking_save_does_not_click_in_the_game() {
    let dir = directory("mouse-save");
    let path = dir.join("display.json");
    let mut app = fixture(&path, default());
    tap(&mut app, KeyCode::F2);
    rows_at_top(&mut app);
    point(&mut app, Vec2::new(40.0, Row::Save as usize as f32 * 10.0 + 5.0));
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
    frame(&mut app);
    assert!(path.exists() && !app.world().resource::<DisplayMenu>().is_open());
    for _ in 0..3 {
        frame(&mut app);
    }
    assert!(!app.world().resource::<ControllerState>().mouse.held(MouseButton::Left), "held through the close");
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().release(MouseButton::Left);
    frame(&mut app);
    frame(&mut app);
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
    frame(&mut app);
    assert!(app.world().resource::<ControllerState>().mouse.pressed(MouseButton::Left), "a new click is the game's");
}

#[test]
fn arrows_step_a_setting_both_ways() {
    let dir = directory("arrows");
    let mut app = fixture(&dir.join("display.json"), default());
    tap(&mut app, KeyCode::F2);
    rows_at_top(&mut app);
    let y = Row::Resolution as usize as f32 * 10.0 + 5.0;
    point(&mut app, Vec2::new(204.0, y));
    click(&mut app, MouseButton::Left);
    assert_eq!(live(&mut app).screen, [426, 240], "> steps forwards");
    // A new screen size moves the letterbox under a resting cursor, so aim again.
    for _ in 0..2 {
        point(&mut app, Vec2::new(108.0, y));
        click(&mut app, MouseButton::Left);
    }
    assert_eq!(live(&mut app).screen, [640, 480], "< steps backwards and wraps");
    point(&mut app, Vec2::new(150.0, y));
    click(&mut app, MouseButton::Left);
    assert_eq!(live(&mut app).screen, [320, 240], "the rest of the row steps forwards");
}

/// The launcher's default set-up, then the game's own view: the built-in UI
/// is never spawned and F2 is left to the game, while the menu still works
/// through its public API.
#[test]
fn a_game_can_replace_the_built_in_view_with_its_own() {
    let dir = std::env::temp_dir().join(format!("psx-display-custom-view-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tick = TickRate::default();
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .insert_resource(PsxScreen::default())
        .insert_resource(PsxLook::default())
        .init_resource::<PsxAspect>()
        .add_plugins((
            ShellPlugin { tick },
            DisplayPlugin {
                path: Some(dir.join("display.json")),
                screens: crate::launcher::screen_choices(UVec2::new(320, 240)),
                ..default()
            },
        ))
        .insert_resource(DisplayUi { view: false, shortcuts: false })
        .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period()))
        .init_resource::<ButtonInput<KeyCode>>()
        .init_resource::<ButtonInput<MouseButton>>();
    app.world_mut().spawn((Window::default(), PrimaryWindow));
    frame(&mut app);
    tap(&mut app, KeyCode::F2);
    assert!(!app.world().resource::<DisplayMenu>().is_open());
    assert_eq!(app.world_mut().query::<&Node>().iter(app.world()).count(), 0);

    app.world_mut().resource_mut::<DisplayMenu>().open();
    frame(&mut app);
    tap(&mut app, KeyCode::ArrowDown);
    tap(&mut app, KeyCode::ArrowDown);
    tap(&mut app, KeyCode::ArrowDown);
    assert_eq!(app.world().resource::<DisplayMenu>().row(), Row::VertexSnap);
    tap(&mut app, KeyCode::ArrowRight);
    let look = *app.world().resource::<PsxLook>();
    let settings = DisplaySettings::current(&PsxScreen::default(), PsxAspect::default(), &look, None);
    assert_eq!(value(Row::VertexSnap, &settings, true), "OFF", "edits preview live");
    let _ = std::fs::remove_dir_all(dir);
}

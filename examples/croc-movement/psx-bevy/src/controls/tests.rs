use super::*;
use crate::pointer::Hitboxes;
use crate::{
    controller::ControllerState,
    shell::{GameClock, ShellPlugin, TickRate},
};
use bevy::time::TimeUpdateStrategy;
use std::path::Path;

fn fixture(path: &Path) -> App {
    let tick = TickRate::default();
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        ShellPlugin { tick },
        ControlsPlugin { path: Some(path.into()), render_ui: false, ..default() },
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period()))
    .init_resource::<ButtonInput<KeyCode>>();
    frame(&mut app);
    frame(&mut app);
    app
}
fn frame(app: &mut App) {
    app.update();
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
    let world = app.world_mut();
    let mut query = world.query::<&mut Gamepad>();
    for mut pad in query.iter_mut(world) {
        pad.digital_mut().clear();
    }
}
fn key(app: &mut App, key: KeyCode, pressed: bool) {
    {
        let mut input = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        if pressed {
            input.press(key);
        } else {
            input.release(key);
        }
    }
    frame(app);
}
fn tap(app: &mut App, code: KeyCode) {
    key(app, code, true);
    key(app, code, false);
}
fn pad_button(app: &mut App, pad: Entity, button: GamepadButton, pressed: bool) {
    {
        let mut pad = app.world_mut().get_mut::<Gamepad>(pad).unwrap();
        if pressed {
            pad.digital_mut().press(button);
        } else {
            pad.digital_mut().release(button);
        }
    }
    frame(app);
}
fn pad_tap(app: &mut App, pad: Entity, button: GamepadButton) {
    pad_button(app, pad, button, true);
    pad_button(app, pad, button, false);
}
fn directory(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "psx-controls-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn capture_key(app: &mut App, action: Button, code: KeyCode) {
    app.world_mut().resource_mut::<ControlsMenu>().select(action, BindingDevice::Keyboard);
    tap(app, KeyCode::Enter);
    tap(app, code);
}

#[test]
fn swaps_are_transactional_and_keep_gameplay_actions_unique() {
    let mut bindings = ControllerBindings::default();
    let before = bindings.clone();
    let cross = PhysicalBinding::Key(KeyCode::KeyX);
    assert_eq!(bindings.assign(Button::Up, cross, ConflictPolicy::Reject).unwrap_err().other, Button::Cross);
    assert_eq!(bindings, before);
    bindings.assign(Button::Up, cross, ConflictPolicy::Swap).unwrap();
    assert_eq!(bindings.key(Button::Cross), Some(KeyCode::ArrowUp));
    assert_eq!(bindings.key(Button::Up), Some(KeyCode::KeyX));
    bindings.clear(Button::Up, BindingDevice::Gamepad);
    bindings.assign(Button::Up, PhysicalBinding::Pad(GamepadButton::South), ConflictPolicy::Swap).unwrap();
    assert_eq!(bindings.pad_button(Button::Cross), None);
    assert_eq!(bindings.key(Button::Cross), Some(KeyCode::ArrowUp));
    bindings.validate().unwrap();
}

#[test]
fn keyboard_drafts_persist_and_confirm_keys_never_reach_gameplay() {
    let dir = directory("keyboard");
    let path = dir.join("controls.json");
    let mut app = fixture(&path);
    key(&mut app, KeyCode::KeyX, true);
    assert!(app.world().resource::<ControllerState>().pressed(Button::Cross));
    let clock = app.world().resource::<GameClock>().frame;
    tap(&mut app, KeyCode::F1);
    assert!(app.world().resource::<ControlsMenu>().is_open());
    assert_eq!(app.world().resource::<GameClock>().frame, clock);
    assert!(app.world().resource::<ControllerState>().buttons().is_empty());
    key(&mut app, KeyCode::KeyX, false);
    capture_key(&mut app, Button::Cross, KeyCode::KeyV);
    assert_eq!(app.world().resource::<ControllerBindings>().key(Button::Cross), Some(KeyCode::KeyX));
    assert_eq!(app.world().resource::<ControlsMenu>().draft.as_ref().unwrap().key(Button::Cross), Some(KeyCode::KeyV));
    assert!(!path.exists(), "draft edits must not write a profile");
    app.world_mut().resource_mut::<ControlsMenu>().selected = SAVE;
    key(&mut app, KeyCode::Enter, true);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    assert!(app.world().resource::<InputFocus>().wait_for_release);
    assert_eq!(app.world().resource::<GameClock>().frame, clock);
    assert_eq!(app.world().resource::<ControllerBindings>().key(Button::Cross), Some(KeyCode::KeyV));
    key(&mut app, KeyCode::Enter, false);
    assert!(app.world().resource::<ControllerState>().buttons().is_empty());
    key(&mut app, KeyCode::KeyV, true);
    assert!(app.world().resource::<ControllerState>().pressed(Button::Cross));
    let loaded = fixture(&path);
    assert_eq!(loaded.world().resource::<ControllerBindings>().key(Button::Cross), Some(KeyCode::KeyV));
    assert_eq!(loaded.world().resource::<Defaults>().0.key(Button::Cross), Some(KeyCode::KeyX));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn conflict_cancel_restore_clear_and_discard_leave_live_bindings_alone() {
    let dir = directory("draft");
    let path = dir.join("controls.json");
    let mut app = fixture(&path);
    tap(&mut app, KeyCode::F1);
    capture_key(&mut app, Button::Up, KeyCode::KeyX);
    assert!(matches!(app.world().resource::<ControlsMenu>().mode, Mode::Conflict(_)));
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app.world().resource::<ControlsMenu>().draft.as_ref().unwrap().key(Button::Up), Some(KeyCode::ArrowUp));
    capture_key(&mut app, Button::Up, KeyCode::KeyX);
    tap(&mut app, KeyCode::Enter);
    let menu = app.world().resource::<ControlsMenu>();
    assert_eq!(menu.draft.as_ref().unwrap().key(Button::Cross), Some(KeyCode::ArrowUp));
    assert_eq!(menu.draft.as_ref().unwrap().key(Button::Up), Some(KeyCode::KeyX));
    assert_eq!(menu.selected, Button::Down as usize, "a confirmed swap moves on like any binding");
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Up, BindingDevice::Keyboard);
    tap(&mut app, KeyCode::Delete);
    assert_eq!(app.world().resource::<ControlsMenu>().draft.as_ref().unwrap().key(Button::Up), None);
    app.world_mut().resource_mut::<ControlsMenu>().selected = RESTORE;
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app.world().resource::<ControlsMenu>().draft.as_ref().unwrap(), &ControllerBindings::default());
    capture_key(&mut app, Button::Cross, KeyCode::KeyV);
    app.world_mut().resource_mut::<ControlsMenu>().selected = DISCARD;
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app.world().resource::<ControllerBindings>(), &ControllerBindings::default());
    assert!(!path.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn gamepad_can_reassign_the_cancel_button_and_release_gates_the_saved_mapping() {
    let dir = directory("gamepad");
    let path = dir.join("controls.json");
    let mut app = fixture(&path);
    let pad = app.world_mut().spawn(Gamepad::default()).id();
    pad_button(&mut app, pad, GamepadButton::Start, true);
    pad_button(&mut app, pad, GamepadButton::Select, true);
    assert!(app.world().resource::<ControlsMenu>().is_open());
    pad_button(&mut app, pad, GamepadButton::Start, false);
    pad_button(&mut app, pad, GamepadButton::Select, false);
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Cross, BindingDevice::Gamepad);
    pad_button(&mut app, pad, GamepadButton::South, true);
    frame(&mut app);
    assert!(matches!(app.world().resource::<ControlsMenu>().mode, Mode::Listen { armed: false, .. }));
    pad_button(&mut app, pad, GamepadButton::South, false);
    pad_tap(&mut app, pad, GamepadButton::East);
    assert!(
        matches!(app.world().resource::<ControlsMenu>().mode, Mode::Conflict(_)),
        "East must be assignable during capture"
    );
    pad_tap(&mut app, pad, GamepadButton::South);
    app.world_mut().resource_mut::<ControlsMenu>().selected = SAVE;
    pad_button(&mut app, pad, GamepadButton::South, true);
    assert_eq!(app.world().resource::<ControllerBindings>().pad_button(Button::Cross), Some(GamepadButton::East));
    assert_eq!(app.world().resource::<ControllerBindings>().pad_button(Button::Circle), Some(GamepadButton::South));
    assert!(app.world().resource::<InputFocus>().blocked());
    assert!(app.world().resource::<ControllerState>().buttons().is_empty());
    pad_button(&mut app, pad, GamepadButton::South, false);
    pad_button(&mut app, pad, GamepadButton::East, true);
    assert!(app.world().resource::<ControllerState>().pressed(Button::Cross));
    assert!(!app.world().resource::<ControllerState>().held(Button::Circle));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_profiles_and_failed_saves_preserve_files_live_state_and_draft() {
    let dir = directory("storage");
    let path = dir.join("controls.json");
    let store = BindingStore::new(Some(path.clone()));
    assert_eq!(store.load().unwrap(), None);
    store.save(&ControllerBindings::default()).unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["bindings"]["keyboard"][1] = json["bindings"]["keyboard"][0].clone();
    let bad = serde_json::to_vec(&json).unwrap();
    std::fs::write(&path, &bad).unwrap();
    assert!(store.load().is_err());
    let mut app = fixture(&path);
    assert_eq!(std::fs::read(&path).unwrap(), bad);
    assert_eq!(app.world().resource::<ControllerBindings>(), &ControllerBindings::default());
    let blocked = dir.join("blocked");
    std::fs::write(&blocked, "preserve me").unwrap();
    app.world_mut().resource_mut::<BindingStore>().path = Some(blocked.join("controls.json"));
    tap(&mut app, KeyCode::F1);
    capture_key(&mut app, Button::Cross, KeyCode::KeyV);
    app.world_mut().resource_mut::<ControlsMenu>().selected = SAVE;
    tap(&mut app, KeyCode::Enter);
    assert!(app.world().resource::<ControlsMenu>().is_open());
    assert_eq!(app.world().resource::<ControllerBindings>().key(Button::Cross), Some(KeyCode::KeyX));
    assert_eq!(app.world().resource::<ControlsMenu>().draft.as_ref().unwrap().key(Button::Cross), Some(KeyCode::KeyV));
    assert_eq!(std::fs::read_to_string(blocked).unwrap(), "preserve me");
    assert_eq!(std::fs::read(&path).unwrap(), bad);
    app.world_mut().resource_mut::<BindingStore>().path = Some(path.clone());
    tap(&mut app, KeyCode::Enter);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    assert!(app.world().resource::<BindingStore>().load_error.is_none());
    assert_eq!(store.load().unwrap().unwrap().key(Button::Cross), Some(KeyCode::KeyV));
    tap(&mut app, KeyCode::F1);
    assert!(!app.world().resource::<ControlsMenu>().status().contains("could not be loaded"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn gamepad_navigation_can_edit_save_and_resume_without_a_keyboard() {
    let dir = directory("pad-navigation");
    let path = dir.join("controls.json");
    let mut app = fixture(&path);
    let pad = app.world_mut().spawn(Gamepad::default()).id();
    pad_button(&mut app, pad, GamepadButton::Start, true);
    pad_button(&mut app, pad, GamepadButton::Select, true);
    pad_button(&mut app, pad, GamepadButton::Start, false);
    pad_button(&mut app, pad, GamepadButton::Select, false);
    for _ in 0..4 {
        pad_tap(&mut app, pad, GamepadButton::DPadDown);
    }
    pad_tap(&mut app, pad, GamepadButton::DPadRight);
    pad_tap(&mut app, pad, GamepadButton::South);
    pad_tap(&mut app, pad, GamepadButton::Mode);
    for _ in 5..SAVE {
        pad_tap(&mut app, pad, GamepadButton::DPadDown);
    }
    pad_tap(&mut app, pad, GamepadButton::South);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    assert_eq!(
        BindingStore::new(Some(path)).load().unwrap().unwrap().pad_button(Button::Cross),
        Some(GamepadButton::Mode)
    );
    pad_button(&mut app, pad, GamepadButton::Mode, true);
    assert!(app.world().resource::<ControllerState>().pressed(Button::Cross));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn malformed_or_oversized_profiles_are_rejected_without_rewriting_them() {
    let dir = directory("validation");
    let path = dir.join("controls.json");
    let store = BindingStore::new(Some(path.clone()));
    store.save(&ControllerBindings::default()).unwrap();
    let valid: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (pointer, value) in [
        ("/version", serde_json::json!(2)),
        ("/bindings/keyboard/0", serde_json::json!("InvalidKey")),
        ("/bindings/gamepad", serde_json::json!([])),
        ("/bindings/stick_deadzone", serde_json::json!(1.0)),
        ("/bindings/stick_deadzone", serde_json::json!(-0.1)),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let bytes = serde_json::to_vec(&invalid).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(store.load().is_err(), "accepted invalid {pointer}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    let oversized = vec![b' '; 65537];
    std::fs::write(&path, &oversized).unwrap();
    assert!(store.load().unwrap_err().contains("64 KiB"));
    assert_eq!(std::fs::read(&path).unwrap(), oversized);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ambiguous_presses_and_gamepad_reconnect_require_fresh_neutral_input() {
    let dir = directory("capture");
    let mut app = fixture(&dir.join("controls.json"));
    tap(&mut app, KeyCode::F1);
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Cross, BindingDevice::Keyboard);
    tap(&mut app, KeyCode::Enter);
    {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.press(KeyCode::KeyV);
        keys.press(KeyCode::KeyB);
    }
    frame(&mut app);
    assert!(matches!(app.world().resource::<ControlsMenu>().mode, Mode::Listen { armed: false, .. }));
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release_all();
    frame(&mut app);
    tap(&mut app, KeyCode::KeyV);
    assert_eq!(app.world().resource::<ControlsMenu>().draft.as_ref().unwrap().key(Button::Cross), Some(KeyCode::KeyV));
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Cross, BindingDevice::Gamepad);
    tap(&mut app, KeyCode::Enter);
    let mut pad = Gamepad::default();
    pad.digital_mut().press(GamepadButton::West);
    let entity = app.world_mut().spawn(pad).id();
    frame(&mut app);
    assert!(matches!(app.world().resource::<ControlsMenu>().mode, Mode::Listen { armed: false, .. }));
    pad_button(&mut app, entity, GamepadButton::West, false);
    pad_tap(&mut app, entity, GamepadButton::Mode);
    assert_eq!(
        app.world().resource::<ControlsMenu>().draft.as_ref().unwrap().pad_button(Button::Cross),
        Some(GamepadButton::Mode)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn losing_window_focus_discards_the_draft_and_waits_for_release() {
    let dir = directory("focus");
    let path = dir.join("controls.json");
    let mut app = fixture(&path);
    tap(&mut app, KeyCode::F1);
    capture_key(&mut app, Button::Cross, KeyCode::KeyV);
    app.world_mut().spawn((Window { focused: false, ..default() }, bevy::window::PrimaryWindow));
    frame(&mut app);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    assert_eq!(app.world().resource::<ControllerBindings>(), &ControllerBindings::default());
    assert!(!path.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_options_flow_can_disable_shortcuts_and_restore_its_own_defaults() {
    let dir = directory("custom");
    let path = dir.join("controls.json");
    let mut custom = ControllerBindings::default();
    custom.assign(Button::Cross, PhysicalBinding::Key(KeyCode::Space), ConflictPolicy::Reject).unwrap();
    let mut app = App::new();
    let tick = TickRate::default();
    app.add_plugins((MinimalPlugins, ShellPlugin { tick }))
        .insert_resource(custom.clone())
        .add_plugins(ControlsPlugin { path: Some(path), global_shortcuts: false, render_ui: false, ..default() })
        .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period()))
        .init_resource::<ButtonInput<KeyCode>>();
    frame(&mut app);
    tap(&mut app, KeyCode::F1);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    app.world_mut().resource_mut::<ControlsMenu>().open();
    frame(&mut app);
    capture_key(&mut app, Button::Cross, KeyCode::F1);
    assert!(app.world().resource::<ControlsMenu>().is_open());
    assert_eq!(app.world().resource::<ControlsMenu>().draft().unwrap().key(Button::Cross), Some(KeyCode::F1));
    app.world_mut().resource_mut::<ControlsMenu>().selected = RESTORE;
    tap(&mut app, KeyCode::Enter);
    assert_eq!(app.world().resource::<ControlsMenu>().draft(), Some(&custom));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn binding_moves_to_the_next_action_so_enter_alone_walks_the_layout() {
    let dir = directory("advance");
    let path = dir.join("controls.json");
    let mut app = fixture(&path);
    tap(&mut app, KeyCode::F1);
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Up, BindingDevice::Keyboard);
    // No arrow keys: Enter, then the new key, for every action in order.
    let keys = [
        KeyCode::KeyI,
        KeyCode::KeyK,
        KeyCode::KeyJ,
        KeyCode::KeyL,
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyF,
        KeyCode::KeyG,
        KeyCode::KeyH,
        KeyCode::KeyM,
        KeyCode::KeyN,
        KeyCode::KeyO,
        KeyCode::KeyP,
        KeyCode::KeyR,
    ];
    for (row, &code) in keys.iter().enumerate() {
        assert_eq!(app.world().resource::<ControlsMenu>().selected, row);
        tap(&mut app, KeyCode::Enter);
        tap(&mut app, code);
    }
    let menu = app.world().resource::<ControlsMenu>();
    assert_eq!(menu.selected, SAVE, "after the last action Enter saves");
    assert!(menu.status().ends_with("Next: Save"), "{}", menu.status());
    tap(&mut app, KeyCode::Enter);
    let saved = BindingStore::new(Some(path)).load().unwrap().unwrap();
    for (button, code) in Button::ALL.into_iter().zip(keys) {
        assert_eq!(saved.key(button), Some(code), "{button:?}");
    }
    // Cancelling a capture stays on the row.
    tap(&mut app, KeyCode::F1);
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Cross, BindingDevice::Keyboard);
    tap(&mut app, KeyCode::Enter);
    tap(&mut app, KeyCode::Escape);
    assert_eq!(app.world().resource::<ControlsMenu>().selected, Button::Cross as usize);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Row and slot rectangles laid out like the view's, at 320x240.
fn row_rect(i: usize) -> Rect {
    Rect::new(6.0, 20.0 + i as f32 * 10.0, 314.0, 30.0 + i as f32 * 10.0)
}
fn slot_rect(i: usize, device: BindingDevice) -> Rect {
    let column = BindingDevice::ALL.iter().position(|d| *d == device).unwrap() as f32;
    Rect::new(76.0 + column * 78.0, row_rect(i).min.y, 154.0 + column * 78.0, row_rect(i).max.y)
}
/// The fixture with a window, a screen and the view's hitboxes.
fn mouse_fixture(path: &Path) -> App {
    let mut app = fixture(path);
    app.insert_resource(crate::psx::PsxScreen::default()).init_resource::<ButtonInput<MouseButton>>();
    app.world_mut().spawn((Window::default(), bevy::window::PrimaryWindow));
    let mut boxes: Vec<_> = (0..ROWS).map(|i| (row_rect(i), Item { row: i, device: None })).collect();
    for i in 0..RESTORE {
        for device in BindingDevice::ALL {
            boxes.push((slot_rect(i, device), Item { row: i, device: Some(device) }));
        }
    }
    app.world_mut().resource_mut::<Hitboxes<Item>>().0 = boxes;
    app
}
/// Moves to screen pixel `at` (the default 1280x720 window shows the screen
/// at 3x with 160-pixel bars), then clicks `button` there.
fn click_at(app: &mut App, at: Vec2, button: MouseButton) {
    let world = app.world_mut();
    let mut window =
        world.query_filtered::<&mut Window, With<bevy::window::PrimaryWindow>>().single_mut(world).unwrap();
    window.set_cursor_position(Some(Vec2::new(160.0, 0.0) + at * 3.0));
    frame(app);
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(button);
    frame(app);
    app.world_mut().resource_mut::<ButtonInput<MouseButton>>().release(button);
    frame(app);
}
fn listening(app: &App) -> bool {
    matches!(app.world().resource::<ControlsMenu>().mode, Mode::Listen { .. })
}

/// Without arrow keys (a remote desktop sending other codes), the mouse
/// reaches every slot: click a keyboard cell, press the new key.
#[test]
fn the_mouse_rebinds_without_arrow_keys() {
    let dir = directory("mouse");
    let path = dir.join("controls.json");
    let mut app = mouse_fixture(&path);
    tap(&mut app, KeyCode::F1);
    assert!(app.world().resource::<ControlsMenu>().is_open());
    click_at(&mut app, slot_rect(Button::Up as usize, BindingDevice::Keyboard).center(), MouseButton::Left);
    assert_eq!(app.world().resource::<ControlsMenu>().device, BindingDevice::Keyboard);
    tap(&mut app, KeyCode::KeyW);
    let draft = app.world().resource::<ControlsMenu>().draft().unwrap().clone();
    assert_eq!(draft.key(Button::Up), Some(KeyCode::KeyW));
    click_at(&mut app, row_rect(SAVE).center(), MouseButton::Left);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    assert_eq!(app.world().resource::<ControllerBindings>().key(Button::Up), Some(KeyCode::KeyW));
    assert!(path.exists());
    // Right click discards an open menu.
    tap(&mut app, KeyCode::F1);
    click_at(&mut app, row_rect(RESTORE).center(), MouseButton::Right);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
}

/// Clicking around must never leave the menu waiting unseen for a key: an
/// action name only selects, and a click elsewhere during capture moves the
/// capture (or cancels it) instead of being ignored.
#[test]
fn clicking_around_never_strands_the_menu_in_capture() {
    let dir = directory("mouse-around");
    let mut app = mouse_fixture(&dir.join("controls.json"));
    tap(&mut app, KeyCode::F1);
    click_at(&mut app, row_rect(Button::Up as usize).center() - Vec2::new(120.0, 0.0), MouseButton::Left);
    assert!(!listening(&app), "the action name selects without capturing");
    assert_eq!(app.world().resource::<ControlsMenu>().selected, Button::Up as usize);
    click_at(&mut app, slot_rect(Button::Up as usize, BindingDevice::Gamepad).center(), MouseButton::Left);
    assert!(listening(&app));
    click_at(&mut app, slot_rect(Button::Down as usize, BindingDevice::Keyboard).center(), MouseButton::Left);
    let menu = app.world().resource::<ControlsMenu>();
    assert!(listening(&app), "a click on another slot moves the capture there");
    assert_eq!((menu.selected, menu.device), (Button::Down as usize, BindingDevice::Keyboard));
    click_at(&mut app, row_rect(SAVE).center(), MouseButton::Left);
    assert!(!listening(&app), "a click on a non-binding row cancels the capture");
    assert!(app.world().resource::<ControlsMenu>().is_open(), "and only cancels: Save needs its own click");
    tap(&mut app, KeyCode::KeyW);
    assert_eq!(app.world().resource::<ControlsMenu>().draft().unwrap().key(Button::Down), Some(KeyCode::ArrowDown));
}

/// Another modal (the display menu) owns input focus: the controls shortcut
/// must not open a second menu on top of it.
#[test]
fn shortcut_waits_for_another_menu_to_close() {
    let dir = directory("modal");
    let mut app = fixture(&dir.join("controls.json"));
    app.world_mut().resource_mut::<InputFocus>().captured = true;
    tap(&mut app, KeyCode::F1);
    assert!(!app.world().resource::<ControlsMenu>().is_open());
    app.world_mut().resource_mut::<InputFocus>().captured = false;
    tap(&mut app, KeyCode::F1);
    assert!(app.world().resource::<ControlsMenu>().is_open());
}

/// A mouse slot captures any mouse button, right click included (so only
/// Esc cancels), once the click that opened the capture is released.
#[test]
fn mouse_slots_capture_mouse_buttons() {
    let dir = directory("mouse-slot");
    let path = dir.join("controls.json");
    let mut app = mouse_fixture(&path);
    tap(&mut app, KeyCode::F1);
    click_at(&mut app, slot_rect(Button::Cross as usize, BindingDevice::Mouse).center(), MouseButton::Left);
    assert!(listening(&app));
    assert_eq!(app.world().resource::<ControlsMenu>().device, BindingDevice::Mouse);
    click_at(&mut app, slot_rect(Button::Cross as usize, BindingDevice::Mouse).center(), MouseButton::Right);
    let draft = app.world().resource::<ControlsMenu>().draft().unwrap().clone();
    assert_eq!(draft.mouse_button(Button::Cross), Some(MouseButton::Right));
    assert!(app.world().resource::<ControlsMenu>().is_open(), "right click bound rather than closing");
    // Saved and reloaded with the profile.
    app.world_mut().resource_mut::<ControlsMenu>().select(Button::Up, BindingDevice::Keyboard);
    app.world_mut().resource_mut::<ControlsMenu>().selected = SAVE;
    tap(&mut app, KeyCode::Enter);
    assert_eq!(
        BindingStore::new(Some(path)).load().unwrap().unwrap().mouse_button(Button::Cross),
        Some(MouseButton::Right)
    );
}

/// Profiles saved before mouse bindings existed still load, with the mouse unbound.
#[test]
fn profiles_without_mouse_slots_load() {
    let dir = directory("old-profile");
    let path = dir.join("controls.json");
    let store = BindingStore::new(Some(path.clone()));
    store.save(&ControllerBindings::default()).unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["bindings"].as_object_mut().unwrap().remove("mouse");
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let loaded = store.load().unwrap().unwrap();
    assert!(Button::ALL.iter().all(|b| loaded.mouse_button(*b).is_none()));
    assert_eq!(loaded, ControllerBindings::default());
}

/// What a game's own controls screen sees through the public API.
#[derive(Resource, Default)]
struct CustomView(Vec<String>);

fn custom_view(menu: Res<ControlsMenu>, bindings: Res<ControllerBindings>, mut view: ResMut<CustomView>) {
    view.0.clear();
    if !menu.is_open() {
        return;
    }
    let shown = menu.shown(&bindings);
    for row in 0..ROWS {
        let mark = if row == menu.selected { ">" } else { " " };
        let key = Button::ALL.get(row).and_then(|b| shown.key(*b)).map_or(String::new(), |k| format!("{k:?}"));
        view.0.push(format!("{mark}{} {key}", row_label(row)));
    }
    view.0.push(format!("{}|{}", menu.status(), menu.hint()));
}

/// As the launcher sets it up (built-in view on), then the game's `add`
/// closure swaps in its own view: no built-in UI is spawned (it would need
/// the renderer, which this app lacks), F1 is left to the game, and the
/// public API is enough to draw and drive the menu.
#[test]
fn a_game_can_replace_the_built_in_view_with_its_own() {
    let dir = directory("custom-view");
    let mut app = App::new();
    let tick = TickRate::default();
    app.add_plugins((MinimalPlugins, ShellPlugin { tick }))
        .add_plugins(ControlsPlugin { path: Some(dir.join("controls.json")), ..default() })
        .insert_resource(ControlsUi { view: false, shortcuts: false })
        .init_resource::<CustomView>()
        .add_systems(Update, custom_view)
        .insert_resource(TimeUpdateStrategy::ManualDuration(tick.period()))
        .init_resource::<ButtonInput<KeyCode>>();
    frame(&mut app);
    tap(&mut app, KeyCode::F1);
    assert!(!app.world().resource::<ControlsMenu>().is_open(), "shortcuts are the game's");
    assert_eq!(app.world_mut().query::<&Node>().iter(app.world()).count(), 0, "no built-in view");

    app.world_mut().resource_mut::<ControlsMenu>().open();
    frame(&mut app);
    tap(&mut app, KeyCode::ArrowDown);
    let view = &app.world().resource::<CustomView>().0;
    assert_eq!(view.len(), ROWS + 1);
    assert!(view[1].starts_with(&format!(">{}", Button::ALL[1].label())), "{view:?}");
    assert_eq!(view[SAVE].trim(), "Save and close");
    assert!(view[ROWS].contains("Enter/click: edit"));

    capture_key(&mut app, Button::ALL[1], KeyCode::KeyP);
    assert!(app.world().resource::<CustomView>().0[1].ends_with("KeyP"), "the draft is shown");
    std::fs::remove_dir_all(dir).unwrap();
}

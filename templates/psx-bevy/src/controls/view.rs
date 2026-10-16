//! Ordinary Bevy UI rendered into the PS1 canvas, before its screen pass.
use super::{BindingDevice, Button, ControlsMenu, DISCARD, Mode, RESTORE, SAVE};
use crate::{
    controller::ControllerBindings,
    psx::{PsxView, SCREEN_PASS_ORDER},
};
use bevy::{camera::visibility::RenderLayers, prelude::*, text::FontSmoothing};

#[derive(Component)]
struct ControlsRoot;
#[derive(Component)]
struct ControlsCamera;
#[derive(Component)]
struct Row(usize);
#[derive(Component)]
struct Cell {
    row: usize,
    device: Option<BindingDevice>,
}
#[derive(Component)]
struct Hint;

// These are framebuffer colours: this camera draws in the PSX encoded space.
const WHITE: Color = Color::linear_rgb(0.90, 0.94, 1.0);
const GOLD: Color = Color::linear_rgb(1.0, 0.85, 0.25);
const CYAN: Color = Color::linear_rgb(0.25, 0.85, 1.0);
const NAVY: Color = Color::linear_rgb(0.02, 0.04, 0.22);
const SELECTED: Color = Color::linear_rgb(0.06, 0.18, 0.48);

pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, spawn).add_systems(Update, refresh);
}
fn font(size: f32) -> TextFont {
    TextFont { font_size: size.into(), font_smoothing: FontSmoothing::None, ..default() }
}
fn spawn(mut commands: Commands) {
    let camera = commands
        .spawn((
            Name::new("Controls camera"),
            Camera2d,
            Camera { order: SCREEN_PASS_ORDER - 1, is_active: false, clear_color: ClearColorConfig::None, ..default() },
            RenderLayers::layer(28),
            PsxView,
            ControlsCamera,
        ))
        .id();
    commands
        .spawn((
            Name::new("Controls menu"),
            ControlsRoot,
            UiTargetCamera(camera),
            Node {
                display: Display::None,
                width: percent(100),
                height: percent(100),
                padding: UiRect::axes(px(8), px(5)),
                border: UiRect::all(px(2)),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(NAVY),
            BorderColor::all(CYAN),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("CONTROLLER SETUP"),
                font(13.0),
                TextColor(GOLD),
                Node { height: px(15), min_height: px(15), ..default() },
            ));
            root.spawn((
                Text::new("Choose an action, then assign a key or button."),
                font(8.0),
                TextColor(WHITE),
                Node { height: px(10), min_height: px(10), ..default() },
            ));
            root.spawn(Node { height: px(10), min_height: px(10), flex_direction: FlexDirection::Row, ..default() })
                .with_children(|header| {
                    for (text, width) in [("ACTION", 66.0), ("KEYBOARD", 104.0), ("GAMEPAD", 104.0)] {
                        header.spawn((
                            Text::new(text),
                            font(8.0),
                            TextColor(CYAN),
                            Node { width: px(width), ..default() },
                        ));
                    }
                });
            for (row, button) in Button::ALL.into_iter().enumerate() {
                root.spawn((
                    Row(row),
                    Node { height: px(9), min_height: px(9), flex_direction: FlexDirection::Row, ..default() },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|line| {
                    line.spawn((
                        Cell { row, device: None },
                        Text::new(button.label()),
                        font(8.5),
                        TextColor(WHITE),
                        Node { width: px(66), ..default() },
                    ));
                    for device in [BindingDevice::Keyboard, BindingDevice::Gamepad] {
                        line.spawn((
                            Cell { row, device: Some(device) },
                            Text::new(""),
                            font(8.5),
                            TextColor(WHITE),
                            Node { width: px(104), ..default() },
                        ));
                    }
                });
            }
            for (row, label) in
                [(RESTORE, "Restore defaults"), (SAVE, "Save and close"), (DISCARD, "Discard and close")]
            {
                root.spawn((
                    Row(row),
                    Node { height: px(10), min_height: px(10), ..default() },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|line| {
                    line.spawn((Cell { row, device: None }, Text::new(label), font(8.5), TextColor(WHITE)));
                });
            }
            root.spawn((
                Hint,
                Text::new(""),
                font(7.5),
                TextColor(CYAN),
                Node { margin: UiRect::top(px(2)), flex_shrink: 0.0, ..default() },
            ));
        });
}

fn refresh(
    menu: Res<ControlsMenu>,
    bindings: Res<ControllerBindings>,
    mut roots: Query<&mut Node, With<ControlsRoot>>,
    mut cameras: Query<&mut Camera, With<ControlsCamera>>,
    mut rows: Query<(&Row, &mut BackgroundColor)>,
    mut cells: Query<(&Cell, &mut Text, &mut TextColor)>,
    mut hints: Query<&mut Text, (With<Hint>, Without<Cell>)>,
) {
    if !menu.is_changed() && !bindings.is_changed() {
        return;
    }
    for mut root in &mut roots {
        root.display = if menu.open { Display::Flex } else { Display::None };
    }
    for mut camera in &mut cameras {
        camera.is_active = menu.open;
    }
    if !menu.open {
        return;
    }
    let draft = menu.draft.as_ref().unwrap_or(&bindings);
    for (row, mut color) in &mut rows {
        color.0 = if row.0 == menu.selected { SELECTED } else { Color::NONE };
    }
    for (cell, mut text, mut color) in &mut cells {
        let selected = cell.row == menu.selected;
        color.0 = if selected && cell.device.is_none_or(|d| d == menu.device) { GOLD } else { WHITE };
        if let Some(device) = cell.device {
            let label = draft.binding(Button::ALL[cell.row], device).map_or("---".into(), |b| b.label());
            let label = label.chars().take(18).collect::<String>();
            **text = if selected && device == menu.device { format!("> {label}") } else { label };
        }
    }
    let hint = match menu.mode {
        Mode::Browse => "Arrows/D-pad: select  Enter/South: edit\nDel/West: clear  Esc/East: discard",
        Mode::Listen { .. } => "Esc: cancel binding\nPress one physical key or button.",
        Mode::Conflict(_) => "Enter/South: swap assignments  Esc/East: keep\nNothing changes until Save and close.",
    };
    for mut text in &mut hints {
        **text = format!("{}\n{hint}", menu.status);
    }
}

//! Ordinary Bevy UI rendered into the PS1 canvas, before its screen pass.
//! Labels use the bitmap font in `psx::font`: vector text at this size breaks up.
use super::{BindingDevice, Button, ControlsMenu, ControlsUi, DISCARD, Item, RESTORE, SAVE, row_label};
use crate::{
    controller::ControllerBindings,
    pointer::{Hitboxes, node_rect},
    psx::{
        PsxView, SCREEN_PASS_ORDER,
        font::{GLYPH_W, LINE_H, text_image},
    },
};
use bevy::{camera::visibility::RenderLayers, prelude::*};

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
/// A binding column of one row: clicking it edits that device's slot.
#[derive(Component)]
struct Slot(Item);
#[derive(Component)]
struct Hint;
/// The text an `ImageNode` currently shows, so it is only re-rendered on change.
#[derive(Component)]
struct Label(String);

// These are framebuffer colours: this camera draws in the PSX encoded space.
const WHITE: Color = Color::linear_rgb(0.90, 0.94, 1.0);
const GOLD: Color = Color::linear_rgb(1.0, 0.85, 0.25);
const CYAN: Color = Color::linear_rgb(0.25, 0.85, 1.0);
const NAVY: Color = Color::linear_rgb(0.02, 0.04, 0.22);
const SELECTED: Color = Color::linear_rgb(0.06, 0.18, 0.48);

/// Inner width (320 minus border and padding) and column widths, in pixels.
const INNER: u32 = 304;
const ACTION_W: u32 = 70;
const BINDING_W: u32 = (INNER - ACTION_W) / 3;
/// Characters of a binding that fit its column after the `> ` marker.
const BINDING_CHARS: usize = (BINDING_W / GLYPH_W) as usize - 2;

pub(super) fn plugin(app: &mut App) {
    let shown = |ui: Res<ControlsUi>| ui.view;
    app.add_systems(Startup, spawn.run_if(shown)).add_systems(Update, (refresh, collect_hitboxes).run_if(shown));
}
fn label(text: &str, color: Color, images: &mut Assets<Image>) -> impl Bundle {
    let image = text_image(text);
    (Label(text.into()), size(&image), ImageNode::new(images.add(image)).with_color(color))
}
/// Exactly the image's size: one texel per canvas pixel, never stretched to the row.
fn size(image: &Image) -> Node {
    Node { width: px(image.width()), height: px(image.height()), flex_shrink: 0.0, ..default() }
}
/// A fixed-width column holding one label at its natural size.
fn column(width: u32) -> Node {
    Node { width: px(width), height: px(LINE_H), ..default() }
}
fn spawn(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
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
                padding: UiRect::axes(px(6), px(3)),
                border: UiRect::all(px(2)),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(NAVY),
            BorderColor::all(CYAN),
        ))
        .with_children(|root| {
            root.spawn(Node { height: px(LINE_H + 3), flex_shrink: 0.0, ..default() }).with_child(label(
                "CONTROLLER SETUP",
                GOLD,
                &mut images,
            ));
            root.spawn(Node { height: px(LINE_H), flex_shrink: 0.0, ..default() }).with_children(|header| {
                for (text, width) in
                    [("ACTION", ACTION_W), ("KEYBOARD", BINDING_W), ("GAMEPAD", BINDING_W), ("MOUSE", BINDING_W)]
                {
                    header.spawn(column(width)).with_child(label(text, CYAN, &mut images));
                }
            });
            for (row, button) in Button::ALL.into_iter().enumerate() {
                root.spawn((
                    Row(row),
                    Node { height: px(LINE_H), flex_shrink: 0.0, ..default() },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|line| {
                    line.spawn(column(ACTION_W))
                        .with_child((Cell { row, device: None }, label(button.label(), WHITE, &mut images)));
                    for device in BindingDevice::ALL {
                        line.spawn((column(BINDING_W), Slot(Item { row, device: Some(device) })))
                            .with_child((Cell { row, device: Some(device) }, label("", WHITE, &mut images)));
                    }
                });
            }
            for (row, text) in [RESTORE, SAVE, DISCARD].map(|row| (row, row_label(row))) {
                root.spawn((
                    Row(row),
                    Node { height: px(LINE_H), flex_shrink: 0.0, ..default() },
                    BackgroundColor(Color::NONE),
                ))
                .with_child((Cell { row, device: None }, label(text, WHITE, &mut images)));
            }
            // Statuses quote binding names of any length: clip rather than spill past the border.
            root.spawn(Node { margin: UiRect::top(px(2)), flex_shrink: 0.0, overflow: Overflow::clip(), ..default() })
                .with_child((Hint, label("", CYAN, &mut images)));
        });
}


/// Rows, then the binding columns inside them, as screen rectangles.
fn collect_hitboxes(
    menu: Res<ControlsMenu>,
    rows: Query<(&Row, &ComputedNode, &UiGlobalTransform)>,
    slots: Query<(&Slot, &ComputedNode, &UiGlobalTransform)>,
    mut hitboxes: ResMut<Hitboxes<Item>>,
) {
    hitboxes.0.clear();
    if !menu.is_open() {
        return;
    }
    let rows = rows.iter().map(|(row, node, at)| (node_rect(node, at), Item { row: row.0, device: None }));
    let slots = slots.iter().map(|(slot, node, at)| (node_rect(node, at), slot.0));
    hitboxes.0.extend(rows.chain(slots));
}

#[allow(clippy::too_many_arguments)]
fn refresh(
    menu: Res<ControlsMenu>,
    bindings: Res<ControllerBindings>,
    mut images: ResMut<Assets<Image>>,
    mut roots: Query<&mut Node, (With<ControlsRoot>, Without<Label>)>,
    mut cameras: Query<&mut Camera, With<ControlsCamera>>,
    mut rows: Query<(&Row, &mut BackgroundColor)>,
    mut cells: Query<(&Cell, &mut Label, &mut ImageNode, &mut Node), Without<Hint>>,
    mut hints: Query<(&mut Label, &mut ImageNode, &mut Node), With<Hint>>,
) {
    if !menu.is_changed() && !bindings.is_changed() {
        return;
    }
    for mut root in &mut roots {
        root.display = if menu.is_open() { Display::Flex } else { Display::None };
    }
    for mut camera in &mut cameras {
        camera.is_active = menu.is_open();
    }
    if !menu.is_open() {
        return;
    }
    let mut set = |label: &mut Label, node: &mut ImageNode, layout: &mut Node, text: String| {
        if label.0 != text {
            let image = text_image(&text);
            *layout = size(&image);
            node.image = images.add(image);
            label.0 = text;
        }
    };
    let draft = menu.shown(&bindings);
    for (row, mut color) in &mut rows {
        color.0 = if row.0 == menu.selected { SELECTED } else { Color::NONE };
    }
    for (cell, mut label, mut node, mut layout) in &mut cells {
        let selected = cell.row == menu.selected;
        node.color = if selected && cell.device.is_none_or(|d| d == menu.device) { GOLD } else { WHITE };
        if let Some(device) = cell.device {
            let text = draft.binding(Button::ALL[cell.row], device).map_or("---".into(), |b| b.label());
            let text = text.chars().take(BINDING_CHARS).collect::<String>();
            let text = if selected && device == menu.device { format!("> {text}") } else { text };
            set(&mut label, &mut node, &mut layout, text);
        }
    }
    for (mut label, mut node, mut layout) in &mut hints {
        set(&mut label, &mut node, &mut layout, format!("{}\n{}", menu.status(), menu.hint()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controls::Mode;
    use crate::psx::font::text_width;

    #[test]
    fn layout_and_fixed_text_fit_the_canvas() {
        assert!(Button::ALL.iter().all(|b| text_width(b.label()) <= ACTION_W));
        for mode in [
            Mode::Browse,
            Mode::Listen { armed: false, pad: None },
            Mode::Conflict(crate::controller::BindingConflict {
                action: Button::Up,
                other: Button::Down,
                binding: crate::controller::PhysicalBinding::Key(KeyCode::KeyX),
            }),
        ] {
            for device in BindingDevice::ALL {
                let menu = ControlsMenu { mode: mode.clone(), device, ..default() };
                assert!(text_width(menu.hint()) <= INNER, "{}", menu.hint());
            }
        }
    }
}

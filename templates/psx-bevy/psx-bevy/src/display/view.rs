//! Ordinary Bevy UI rendered into the PS1 canvas, like the controls menu.
//! The menu follows the screen size, so resolution changes preview with it.
use super::{DisplayMenu, DisplaySettings, DisplayUi, HINT, Row, Target};
use crate::{
    pointer::{Hitboxes, node_rect},
    psx::{
        PsxAspect, PsxLook, PsxScreen, PsxView, SCREEN_PASS_ORDER,
        font::{GLYPH_W, LINE_H, text_image},
    },
};
use bevy::{camera::visibility::RenderLayers, prelude::*, window::PrimaryWindow};

#[derive(Component)]
struct DisplayRoot;
#[derive(Component)]
struct DisplayCamera;
#[derive(Component)]
struct Line(usize);
#[derive(Component)]
struct Value(usize);
/// A setting's `<` (backwards) or `>` (forwards), clickable.
#[derive(Component)]
struct Arrow(usize, bool);
/// The glyph inside an [`Arrow`], tinted with its row's selection.
#[derive(Component)]
struct ArrowGlyph(usize);
type ArrowGlyphs<'w, 's> =
    Query<'w, 's, (&'static ArrowGlyph, &'static mut ImageNode), (Without<Value>, Without<Hint>)>;
#[derive(Component)]
struct Hint;
/// The text an `ImageNode` currently shows, so it is only re-rendered on change.
#[derive(Component)]
struct Label(String);

// Framebuffer colours, as in the controls menu.
const WHITE: Color = Color::linear_rgb(0.90, 0.94, 1.0);
const GOLD: Color = Color::linear_rgb(1.0, 0.85, 0.25);
const CYAN: Color = Color::linear_rgb(0.25, 0.85, 1.0);
const NAVY: Color = Color::linear_rgb(0.02, 0.04, 0.22);
const SELECTED: Color = Color::linear_rgb(0.06, 0.18, 0.48);

/// Fixed columns, so the arrows stay put as values change.
const LABEL_W: u32 = 102;
const ARROW_W: u32 = 12;
const VALUE_W: u32 = 14 * GLYPH_W;

pub(super) fn plugin(app: &mut App) {
    let shown = |ui: Res<DisplayUi>| ui.view;
    app.add_systems(Startup, spawn.run_if(shown)).add_systems(Update, (refresh, collect_hitboxes).run_if(shown));
}
fn label(text: &str, color: Color, images: &mut Assets<Image>) -> impl Bundle {
    let image = text_image(text);
    (Label(text.into()), size(&image), ImageNode::new(images.add(image)).with_color(color))
}
fn size(image: &Image) -> Node {
    Node { width: px(image.width()), height: px(image.height()), flex_shrink: 0.0, ..default() }
}
fn line() -> Node {
    Node { height: px(LINE_H), flex_shrink: 0.0, ..default() }
}
fn spawn(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let camera = commands
        .spawn((
            Name::new("Display camera"),
            Camera2d,
            Camera { order: SCREEN_PASS_ORDER - 2, is_active: false, clear_color: ClearColorConfig::None, ..default() },
            RenderLayers::layer(27),
            PsxView,
            DisplayCamera,
        ))
        .id();
    commands
        .spawn((
            Name::new("Display menu"),
            DisplayRoot,
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
                "DISPLAY SETTINGS",
                GOLD,
                &mut images,
            ));
            for (i, row) in Row::ALL.into_iter().enumerate() {
                if row == Row::Restore {
                    root.spawn(Node { height: px(LINE_H / 2), flex_shrink: 0.0, ..default() });
                }
                root.spawn((Line(i), line(), BackgroundColor(Color::NONE))).with_children(|line| {
                    line.spawn(Node { width: px(LABEL_W), height: px(LINE_H), flex_shrink: 0.0, ..default() })
                        .with_child(label(row.label(), WHITE, &mut images));
                    if row.is_setting() {
                        let cell = |width| Node {
                            width: px(width),
                            height: px(LINE_H),
                            flex_shrink: 0.0,
                            justify_content: JustifyContent::Center,
                            ..default()
                        };
                        line.spawn((cell(ARROW_W), Arrow(i, false)))
                            .with_child((ArrowGlyph(i), label("<", CYAN, &mut images)));
                        line.spawn(Node { justify_content: JustifyContent::Start, ..cell(VALUE_W) })
                            .with_child((Value(i), label("", WHITE, &mut images)));
                        line.spawn((cell(ARROW_W), Arrow(i, true)))
                            .with_child((ArrowGlyph(i), label(">", CYAN, &mut images)));
                    }
                });
            }
            root.spawn(Node { margin: UiRect::top(px(4)), flex_shrink: 0.0, overflow: Overflow::clip(), ..default() })
                .with_child((Hint, label("", CYAN, &mut images)));
        });
}

/// Rows, then the arrows inside them.
fn collect_hitboxes(
    menu: Res<DisplayMenu>,
    lines: Query<(&Line, &ComputedNode, &UiGlobalTransform)>,
    arrows: Query<(&Arrow, &ComputedNode, &UiGlobalTransform)>,
    mut hitboxes: ResMut<Hitboxes<Target>>,
) {
    hitboxes.0.clear();
    if menu.is_open() {
        let lines = lines.iter().map(|(line, node, at)| (node_rect(node, at), Target::Row(Row::ALL[line.0])));
        let arrows =
            arrows.iter().map(|(arrow, node, at)| (node_rect(node, at), Target::Step(Row::ALL[arrow.0], arrow.1)));
        hitboxes.0.extend(lines.chain(arrows));
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh(
    menu: Res<DisplayMenu>,
    screen: Res<PsxScreen>,
    psx_aspect: Res<PsxAspect>,
    look: Res<PsxLook>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut roots: Query<&mut Node, (With<DisplayRoot>, Without<Label>)>,
    mut cameras: Query<&mut Camera, With<DisplayCamera>>,
    mut lines: Query<(&Line, &mut BackgroundColor)>,
    mut values: Query<(&Value, &mut Label, &mut ImageNode, &mut Node), Without<Hint>>,
    mut arrows: ArrowGlyphs,
    mut hints: Query<(&mut Label, &mut ImageNode, &mut Node), With<Hint>>,
) {
    if !menu.is_changed() && !menu.is_open() {
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
    let settings = DisplaySettings::current(&screen, *psx_aspect, &look, windows.single().ok());
    for (line, mut color) in &mut lines {
        color.0 = if line.0 == menu.selected { SELECTED } else { Color::NONE };
    }
    for (value, mut label, mut node, mut layout) in &mut values {
        let selected = value.0 == menu.selected;
        node.color = if selected { GOLD } else { WHITE };
        let text = super::value(Row::ALL[value.0], &settings, !windows.is_empty());
        set(&mut label, &mut node, &mut layout, text);
    }
    for (glyph, mut node) in &mut arrows {
        node.color = if glyph.0 == menu.selected { GOLD } else { CYAN };
    }
    for (mut label, mut node, mut layout) in &mut hints {
        set(&mut label, &mut node, &mut layout, format!("{}\n{HINT}", menu.status()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{aspect, value};
    use crate::psx::font::text_width;

    #[test]
    fn rows_and_hint_fit_the_smallest_common_screen() {
        // 256 wide minus border and padding.
        let inner = 256 - 2 * 2 - 2 * 6;
        assert!(text_width(HINT) <= inner, "{HINT}");
        assert!(Row::ALL.iter().all(|r| text_width(r.label()) <= LABEL_W));
        assert!(LABEL_W + 2 * ARROW_W + VALUE_W <= inner);
        let window =
            DisplaySettings { screen: [4096, 4096], aspect: PsxAspect::Window, fullscreen: false, look: default() };
        for settings in [window, DisplaySettings { screen: [1280, 800], ..window }] {
            for row in Row::ALL.into_iter().filter(|r| r.is_setting()) {
                let text = value(row, &settings, true);
                assert!(text_width(&text) <= VALUE_W, "{text}");
            }
        }
    }

    #[test]
    fn names_common_aspects() {
        assert_eq!(aspect([320, 240]), "4:3");
        assert_eq!(aspect([426, 240]), "16:9");
        assert_eq!(aspect([368, 240]), "");
    }
}

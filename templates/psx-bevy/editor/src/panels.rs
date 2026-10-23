//! The outline (what the level holds), the view (a top-down map to select
//! and move things) and the inspector (every field of the selection).
use crate::document::Selection;
use bevy::prelude::*;
use bevy_egui::egui;
use engine::prelude::*;

pub fn outline(ui: &mut egui::Ui, level: &LevelData, selection: &mut Selection) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::CollapsingHeader::new(format!("Objects ({})", level.objects.len())).default_open(true).show(ui, |ui| {
            for (i, object) in level.objects.iter().enumerate() {
                let label =
                    format!("{i}: type {}  ({:.1}, {:.1})", object.type_id.0, object.position.x, object.position.z);
                if ui.selectable_label(*selection == Selection::Object(i), label).clicked() {
                    *selection = Selection::Object(i);
                }
            }
        });
        egui::CollapsingHeader::new(format!("Triggers ({})", level.triggers.len())).default_open(true).show(ui, |ui| {
            for (i, trigger) in level.triggers.iter().enumerate() {
                let label = format!(
                    "{i}: action {}  ({:.1}, {:.1})",
                    trigger.call.action.0, trigger.position.x, trigger.position.z
                );
                if ui.selectable_label(*selection == Selection::Trigger(i), label).clicked() {
                    *selection = Selection::Trigger(i);
                }
            }
        });
    });
}

pub fn inspector(ui: &mut egui::Ui, level: &mut LevelData, selection: Selection) {
    egui::ScrollArea::vertical().show(ui, |ui| match selection {
        Selection::Object(i) => {
            let object = &mut level.objects[i];
            ui.heading(format!("Object {i}"));
            egui::Grid::new("object").num_columns(2).striped(true).show(ui, |ui| {
                ui.label("Type");
                ui.add(egui::DragValue::new(&mut object.type_id.0));
                ui.end_row();
                ui.label("Position (m)");
                vec3(ui, &mut object.position, 0.05);
                ui.end_row();
                ui.label("Yaw");
                ui.drag_angle(&mut object.yaw);
                ui.end_row();
            });
            ints(ui, "Params", &mut object.params);
        }
        Selection::Trigger(i) => {
            let trigger = &mut level.triggers[i];
            ui.heading(format!("Trigger {i}"));
            egui::Grid::new("trigger").num_columns(2).striped(true).show(ui, |ui| {
                ui.label("Position (m)");
                vec3(ui, &mut trigger.position, 0.05);
                ui.end_row();
                ui.label("Half extents (m)");
                vec3(ui, &mut trigger.half_extents, 0.05);
                trigger.half_extents = trigger.half_extents.max(Vec3::ZERO);
                ui.end_row();
                ui.label("Action");
                ui.add(egui::DragValue::new(&mut trigger.call.action.0));
                ui.end_row();
                ui.label("Once");
                ui.checkbox(&mut trigger.once, "");
                ui.end_row();
            });
            ints(ui, "Args", &mut trigger.call.args);
        }
        Selection::None => {
            ui.weak("Select an object or trigger in the outline or the view.");
        }
    });
}

fn vec3(ui: &mut egui::Ui, value: &mut Vec3, speed: f64) {
    ui.horizontal(|ui| {
        for (axis, v) in ["x ", "y ", "z "].into_iter().zip([&mut value.x, &mut value.y, &mut value.z]) {
            ui.add(egui::DragValue::new(v).speed(speed).prefix(axis).max_decimals(3));
        }
    });
}

/// A list of integers whose meaning belongs to the type or action.
fn ints(ui: &mut egui::Ui, label: &str, values: &mut Vec<i32>) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.strong(label);
        if ui.small_button("+").clicked() {
            values.push(0);
        }
    });
    let mut remove = None;
    for (i, value) in values.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("[{i}]"));
            ui.add(egui::DragValue::new(value));
            if ui.small_button("−").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        values.remove(i);
    }
}

/// Pan and zoom of the top-down view.
#[derive(Resource)]
pub struct ViewState {
    /// World XZ at the centre of the view.
    pub centre: egui::Vec2,
    /// Pixels per metre.
    pub scale: f32,
    /// Where the view was drawn last frame.
    pub rect: egui::Rect,
}

impl Default for ViewState {
    fn default() -> Self {
        Self { centre: egui::Vec2::ZERO, scale: 40.0, rect: egui::Rect::NOTHING }
    }
}

impl ViewState {
    /// Where world point `p` is drawn (X right, Z down).
    pub fn to_screen(&self, p: Vec3) -> egui::Pos2 {
        self.rect.center() + (egui::vec2(p.x, p.z) - self.centre) * self.scale
    }
}

/// Top-down (X right, Z down) map of the level. Click selects, dragging the
/// selection moves it, right or middle drag pans, the wheel zooms.
pub fn view(ui: &mut egui::Ui, level: &mut LevelData, selection: &mut Selection, state: &mut ViewState) {
    let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
    let rect = response.rect;
    state.rect = rect;
    let to_screen = |p: Vec3| state.to_screen(p);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);

    // Grid: 1 m lines, brighter axes.
    let grid = visuals.widgets.noninteractive.bg_stroke;
    let half = rect.size() / state.scale / 2.0;
    let (min, max) = (state.centre - half, state.centre + half);
    if state.scale > 8.0 {
        for x in min.x.floor() as i32..=max.x.ceil() as i32 {
            let sx = to_screen(Vec3::new(x as f32, 0.0, 0.0)).x;
            let stroke = if x == 0 { egui::Stroke::new(1.5, visuals.weak_text_color()) } else { grid };
            painter.vline(sx, rect.y_range(), stroke);
        }
        for z in min.y.floor() as i32..=max.y.ceil() as i32 {
            let sy = to_screen(Vec3::new(0.0, 0.0, z as f32)).y;
            let stroke = if z == 0 { egui::Stroke::new(1.5, visuals.weak_text_color()) } else { grid };
            painter.hline(rect.x_range(), sy, stroke);
        }
    }

    let accent = visuals.selection.bg_fill;
    let text = visuals.text_color();
    let font = egui::FontId::proportional(12.0);
    for (i, trigger) in level.triggers.iter().enumerate() {
        let a = to_screen(trigger.position - trigger.half_extents);
        let b = to_screen(trigger.position + trigger.half_extents);
        let selected = *selection == Selection::Trigger(i);
        let colour = if selected { accent } else { egui::Color32::from_rgb(200, 140, 60) };
        let r = egui::Rect::from_two_pos(a, b);
        painter.rect_filled(r, 2.0, colour.gamma_multiply(0.25));
        painter.rect_stroke(
            r,
            2.0,
            egui::Stroke::new(if selected { 2.0 } else { 1.0 }, colour),
            egui::StrokeKind::Inside,
        );
        painter.text(
            r.left_top() + egui::vec2(3.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!("action {}", trigger.call.action.0),
            font.clone(),
            text,
        );
    }
    for (i, object) in level.objects.iter().enumerate() {
        let p = to_screen(object.position);
        let selected = *selection == Selection::Object(i);
        let colour = if selected { accent } else { egui::Color32::from_rgb(90, 160, 220) };
        painter.circle(p, 7.0, colour, egui::Stroke::new(1.0, text));
        let facing = egui::vec2(object.yaw.sin(), object.yaw.cos()) * 12.0;
        painter.line_segment([p, p + facing], egui::Stroke::new(2.0, text));
        painter.text(
            p + egui::vec2(10.0, -10.0),
            egui::Align2::LEFT_BOTTOM,
            object.type_id.0.to_string(),
            font.clone(),
            text,
        );
    }

    // Selection: nearest object within 10 px, else the smallest trigger under the pointer.
    let pick = |pos: egui::Pos2| -> Selection {
        let object = level
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (i, to_screen(o.position).distance(pos)))
            .filter(|(_, d)| *d <= 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = object {
            return Selection::Object(i);
        }
        level
            .triggers
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                egui::Rect::from_two_pos(to_screen(t.position - t.half_extents), to_screen(t.position + t.half_extents))
                    .contains(pos)
            })
            .min_by(|a, b| a.1.half_extents.length_squared().total_cmp(&b.1.half_extents.length_squared()))
            .map_or(Selection::None, |(i, _)| Selection::Trigger(i))
    };
    if let Some(pos) = response.interact_pointer_pos()
        && (response.clicked() || response.drag_started_by(egui::PointerButton::Primary))
    {
        *selection = pick(pos);
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        let delta = response.drag_delta() / state.scale;
        let moved = Vec3::new(delta.x, 0.0, delta.y);
        match *selection {
            Selection::Object(i) => level.objects[i].position += moved,
            Selection::Trigger(i) => level.triggers[i].position += moved,
            Selection::None => {}
        }
    }
    if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
        state.centre -= response.drag_delta() / state.scale;
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            state.scale = (state.scale * (scroll * 0.002).exp()).clamp(4.0, 400.0);
        }
    }
}

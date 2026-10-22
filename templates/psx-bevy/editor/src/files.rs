//! Open and Save As: in-app dialogs over the project's files, so the editor
//! needs no native dialog library and works the same on every desktop.
use crate::Command;
use crate::document::Document;
use bevy_egui::egui;
use std::path::{Path, PathBuf};

/// File > Open: every `.ron` file under the working directory, filtered as you type.
pub struct OpenDialog {
    filter: String,
    files: Vec<PathBuf>,
}

impl OpenDialog {
    pub fn scan() -> Self {
        let mut files = Vec::new();
        find_ron(Path::new("."), &mut files);
        files.sort();
        Self { filter: String::new(), files }
    }
}

/// Directories that never hold source formats: build output and tool caches.
fn skipped(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "target" | "ghidra-projects")
}

fn find_ron(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !skipped(&name) {
                find_ron(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "ron") {
            out.push(path.strip_prefix(".").map(Path::to_path_buf).unwrap_or(path));
        }
    }
}

/// The path Save As offers first.
pub fn default_save_path(document: &Document) -> String {
    document.path.as_ref().map_or("level.ron".into(), |p| p.display().to_string())
}

pub(crate) fn dialogs(
    ctx: &egui::Context,
    open: &mut Option<OpenDialog>,
    save_as: &mut Option<String>,
    out: &mut Vec<Command>,
) {
    if let Some(dialog) = open {
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("open_dialog")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Open");
            let filter =
                ui.add(egui::TextEdit::singleline(&mut dialog.filter).hint_text("Filter").desired_width(f32::INFINITY));
            filter.request_focus();
            let needle = dialog.filter.to_lowercase();
            let matches: Vec<&PathBuf> =
                dialog.files.iter().filter(|p| p.to_string_lossy().to_lowercase().contains(&needle)).collect();
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for path in &matches {
                    if ui.selectable_label(false, path.display().to_string()).clicked() {
                        out.push(Command::OpenPath((*path).clone()));
                        close = true;
                    }
                }
                if matches.is_empty() {
                    ui.weak("No .ron files match.");
                }
            });
            if filter.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && matches.len() == 1 {
                out.push(Command::OpenPath(matches[0].clone()));
                close = true;
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
        if close || response.should_close() {
            *open = None;
        }
    }

    if let Some(path) = save_as {
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("save_as_dialog")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading("Save As");
            let field = ui.add(egui::TextEdit::singleline(path).desired_width(f32::INFINITY));
            field.request_focus();
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if (ui.button("Save").clicked() || enter) && !path.trim().is_empty() {
                    out.push(Command::SaveTo(PathBuf::from(path.trim())));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close || response.should_close() {
            *save_as = None;
        }
    }
}

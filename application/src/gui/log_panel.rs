use std::sync::{Arc, Mutex};

use eframe::egui;

use super::state::{AppState, Lang, LogEntry};

/// Show the log panel using an AppState reference (convenience wrapper).
pub fn show(ui: &mut egui::Ui, state: &AppState) {
    show_with(ui, state.lang, &state.log_lines);
}

/// Show the log panel with explicit parameters (used by standalone binaries).
pub fn show_with(ui: &mut egui::Ui, l: Lang, log_lines: &Arc<Mutex<Vec<LogEntry>>>) {
    ui.horizontal(|ui| {
        ui.strong(l.t("日志", "Log"));
        let count = log_lines.lock().unwrap().len();
        if count > 0 {
            ui.colored_label(
                egui::Color32::from_rgb(120, 120, 120),
                format!("({})", count),
            );
            if ui.small_button(l.t("清除", "Clear")).clicked() {
                log_lines.lock().unwrap().clear();
            }
        }
    });

    ui.separator();

    let lines = log_lines.lock().unwrap();

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            // Build all log text into one string for a single selectable TextEdit.
            // This allows copy/paste and selection survives repaints.
            let mut full_text = String::new();
            for entry in lines.iter() {
                if !full_text.is_empty() { full_text.push('\n'); }
                full_text.push_str(&format!("{} {}", entry.timestamp, entry.message));
            }
            ui.add(
                egui::TextEdit::multiline(&mut full_text.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(1),
            );
        });
}

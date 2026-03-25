use eframe::egui;

use super::state::AppState;

pub fn show(ui: &mut egui::Ui, state: &AppState) {
    ui.horizontal(|ui| {
        ui.strong("日志 / Log");
        if ui.small_button("清除 / Clear").clicked() {
            state.log_lines.lock().unwrap().clear();
        }
    });

    ui.separator();

    let lines = state.log_lines.lock().unwrap().clone();

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for entry in &lines {
                let color = match entry.level {
                    log::Level::Error => egui::Color32::from_rgb(255, 100, 100),
                    log::Level::Warn => egui::Color32::from_rgb(255, 200, 50),
                    log::Level::Info => egui::Color32::from_rgb(200, 200, 200),
                    log::Level::Debug => egui::Color32::from_rgb(150, 150, 150),
                    log::Level::Trace => egui::Color32::from_rgb(100, 100, 100),
                };
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(120, 120, 120), &entry.timestamp);
                    ui.colored_label(color, &entry.message);
                });
            }
        });
}

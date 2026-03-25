use std::sync::atomic::Ordering;

use eframe::egui;

use super::state::{AppState, TaskStatus};
use super::worker::{self, TaskHandle};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    server_handle: &mut Option<TaskHandle>,
    manage_handle: &mut Option<TaskHandle>,
) {
    let is_server_running = server_handle.as_ref().map_or(false, |h| !h.is_finished());
    let is_managing = manage_handle.as_ref().map_or(false, |h| !h.is_finished());

    egui::ScrollArea::vertical().show(ui, |ui| {
        // === HTTP Server Section ===
        ui.heading("HTTP 服务器 / HTTP Server");
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("端口 / Port:");
            ui.add_enabled(
                !is_server_running,
                egui::DragValue::new(&mut state.server_port).range(1024..=65535u16),
            );
        });

        ui.add_space(4.0);

        if is_server_running {
            // Server is running — show status and enabled toggle
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "服务器运行中 / Server running on port {}",
                    state.server_port
                ));
            });

            ui.add_space(4.0);

            // Enabled/paused toggle
            let mut enabled = state.server_enabled.load(Ordering::Relaxed);
            if ui
                .checkbox(&mut enabled, "接受管理请求 / Accept manage requests")
                .changed()
            {
                state.server_enabled.store(enabled, Ordering::Relaxed);
                if enabled {
                    log::info!(
                        "管理器已启用 / Manager enabled — requests on port {} will be executed",
                        state.server_port
                    );
                } else {
                    log::info!(
                        "管理器已暂停 / Manager paused — requests on port {} will be rejected (503)",
                        state.server_port
                    );
                }
            }
            if !enabled {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 50),
                    "已暂停：POST /manage 将返回 503 / Paused: POST /manage returns 503",
                );
            }
        } else {
            if ui.button("▶ 启动服务器 / Start Server").clicked() {
                let _ = yas_genshin::cli::save_config(&state.user_config);
                // Ensure enabled is true when starting fresh
                state
                    .server_enabled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                *server_handle = Some(worker::spawn_server(state));
            }

            // Show result of previous server run
            let status = state.server_status.lock().unwrap().clone();
            match status {
                TaskStatus::Completed(ref msg) => {
                    ui.colored_label(egui::Color32::from_rgb(100, 200, 100), msg);
                }
                TaskStatus::Failed(ref msg) => {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), msg);
                }
                _ => {}
            }
        }

        ui.add_space(12.0);
        ui.separator();

        // === Execute JSON Section ===
        ui.heading("执行JSON / Execute JSON");
        ui.add_space(4.0);
        ui.label("从文件加载管理指令并执行 / Load manage instructions from JSON file");
        ui.add_space(4.0);

        ui.add_enabled_ui(!is_managing && !is_server_running, |ui| {
            if ui
                .button("📁 选择文件并执行 / Choose File & Execute...")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    match std::fs::read_to_string(&path) {
                        Ok(json_str) => {
                            log::info!("加载文件 / Loaded file: {}", path.display());
                            let _ = yas_genshin::cli::save_config(&state.user_config);
                            *manage_handle = Some(worker::spawn_manage_json(
                                state.user_config.clone(),
                                json_str,
                                state.manage_status.clone(),
                            ));
                        }
                        Err(e) => {
                            log::error!("读取文件失败 / Failed to read file: {}", e);
                        }
                    }
                }
            }
        });

        // Show manage result
        if is_managing {
            ui.horizontal(|ui| {
                ui.spinner();
                let status = state.manage_status.lock().unwrap().clone();
                if let TaskStatus::Running(msg) = status {
                    ui.label(msg);
                }
            });
        } else {
            let status = state.manage_status.lock().unwrap().clone();
            match status {
                TaskStatus::Completed(ref msg) => {
                    ui.colored_label(egui::Color32::from_rgb(100, 200, 100), msg);
                }
                TaskStatus::Failed(ref msg) => {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), msg);
                }
                _ => {}
            }
        }
    });
}

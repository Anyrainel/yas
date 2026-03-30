use std::sync::atomic::Ordering;

use eframe::egui;

use super::state::{AppState, TaskStatus};
use super::worker::{self, TaskHandle};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    server_handle: &mut Option<TaskHandle>,
    manage_handle: &mut Option<TaskHandle>,
    scan_running: bool,
) {
    let is_server_running = server_handle.as_ref().map_or(false, |h| !h.is_finished());
    let is_managing = manage_handle.as_ref().map_or(false, |h| !h.is_finished());
    let l = state.lang;

    ui.add_space(4.0);
    ui.label(l.t(
        "接收来自网页前端的圣遗物管理指令（装备/锁定/解锁）",
        "Accept artifact manage instructions (equip/lock/unlock) from a web frontend.",
    ));
    if scan_running {
        ui.add_space(4.0);
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 50),
            l.t(
                "扫描正在进行，请等待完成",
                "Scan is running. Please wait for it to finish.",
            ),
        );
    }
    ui.add_space(8.0);

    // === HTTP Server Section ===
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(l.t("HTTP 服务器", "HTTP Server"));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label(l.t("端口:", "Port:"));
            let mut port_buf = state.server_port.to_string();
            let port_edit = egui::TextEdit::singleline(&mut port_buf)
                .desired_width(50.0)
                .horizontal_align(egui::Align::RIGHT);
            if ui.add_enabled(!is_server_running, port_edit).changed() {
                if let Ok(v) = port_buf.parse::<u16>() {
                    if v >= 1024 {
                        state.server_port = v;
                    }
                }
            }

            ui.add_space(12.0);

            if scan_running && !is_server_running {
                ui.add_enabled(false, egui::Button::new(l.t("▶ 启动", "▶ Start")));
            } else if is_server_running {
                if ui.button(l.t("■ 停止", "■ Stop")).clicked() {
                    if let Some(ref h) = server_handle {
                        h.stop();
                    }
                }
                ui.colored_label(
                    egui::Color32::from_rgb(100, 200, 100),
                    format!(
                        "● {} {}",
                        l.t("运行中", "Running on port"),
                        state.server_port
                    ),
                );
            } else {
                if ui.button(l.t("▶ 启动", "▶ Start")).clicked() {
                    state.server_enabled.store(true, Ordering::Relaxed);
                    *server_handle = Some(worker::spawn_server(state));
                }
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(
                &mut state.stop_on_all_matched,
                l.t(
                    "匹配完成后停止扫描（更快，但无法提供完整圣遗物快照）",
                    "Stop after all matched (faster, but no full inventory snapshot)",
                ),
            );
        });

        // Error from previous run
        if !is_server_running {
            let status = state.server_status.lock().unwrap().clone();
            match status {
                TaskStatus::Failed(ref msg) => {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), msg);
                }
                TaskStatus::Completed(ref msg) => {
                    ui.colored_label(egui::Color32::from_rgb(150, 150, 150), msg);
                }
                _ => {}
            }
        }
    });

    ui.add_space(12.0);

    // === Execute JSON Section ===
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(l.t("离线执行", "Offline Execute"));
        ui.add_space(2.0);
        ui.label(l.t(
            "从JSON文件加载管理指令并执行（无需启动服务器）",
            "Load instructions from a JSON file and execute directly.",
        ));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            let can_execute = !is_managing && !is_server_running && !scan_running;
            if ui
                .add_enabled(
                    can_execute,
                    egui::Button::new(l.t("📁 选择文件...", "📁 Choose File...")),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
                {
                    match std::fs::read_to_string(&path) {
                        Ok(json_str) => {
                            log::info!("{}: {}", l.t("加载文件", "Loaded"), path.display());
                            *manage_handle = Some(worker::spawn_manage_json(
                                state.user_config.clone(),
                                json_str,
                                state.manage_status.clone(),
                                l,
                            ));
                        }
                        Err(e) => {
                            log::error!("{}: {}", l.t("读取文件失败", "Failed to read file"), e);
                        }
                    }
                }
            }

            if is_managing {
                ui.spinner();
                let status = state.manage_status.lock().unwrap().clone();
                if let TaskStatus::Running(msg) = status {
                    ui.label(msg);
                }
            }
        });

        if !is_managing {
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

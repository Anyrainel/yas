use std::sync::atomic::Ordering;

use eframe::egui;

use super::state::{AppState, TaskStatus};
use super::widgets;
use super::worker::{self, TaskHandle};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    server_handle: &mut Option<TaskHandle>,
    scan_running: bool,
) {
    let is_server_running = server_handle.as_ref().map_or(false, |h| !h.is_finished());
    let l = state.lang;

    // === Action bar (always visible at top) ===
    ui.add_space(4.0);
    action_bar(ui, state, server_handle, is_server_running, scan_running);
    if !is_server_running {
        ui.colored_label(
            egui::Color32::from_rgb(120, 120, 120),
            l.t(
                "接收来自网页前端的圣遗物管理指令（装备/锁定/解锁）。",
                "Accept artifact manage instructions (equip/lock/unlock) from a web frontend.",
            ),
        );
    }
    ui.add_space(4.0);
    ui.separator();

    // === Scrollable config area ===
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);

            // === Character Names (always visible, shared with scanner) ===
            widgets::character_names_section(ui, state, !is_server_running);

            ui.add_space(8.0);

            // === Server Options ===
            egui::CollapsingHeader::new(l.t("服务器选项", "Server Options"))
                .default_open(true)
                .show(ui, |ui| {
                    ui.add_enabled_ui(!is_server_running, |ui| {
                        ui.checkbox(
                            &mut state.update_inventory,
                            l.t(
                                "扫描后更新圣遗物列表",
                                "Update inventory after scan",
                            ),
                        );
                        ui.checkbox(
                            &mut state.manager_dump_images,
                            l.t("保存OCR截图", "Dump OCR images"),
                        );
                    });
                });

            // === Timing Delays ===
            egui::CollapsingHeader::new(l.t("延迟设置", "Timing Delays"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.add_enabled_ui(!is_server_running, |ui| {
                        ui.columns(2, |cols| {
                            // Shared inventory delays (same fields as scanner tab)
                            widgets::inventory_delays(&mut cols[0], state, l);

                            // Manager-specific delays
                            widgets::delay_group(&mut cols[1], "mgr_delays", l.t("管理器", "Manager"), &mut [
                                (l.t("画面切换", "Screen transition"), &mut state.user_config.mgr_transition_delay),
                                (l.t("操作按钮", "Action button"), &mut state.user_config.mgr_action_delay),
                                (l.t("格子点击", "Grid cell click"), &mut state.user_config.mgr_cell_delay),
                                (l.t("滚动等待", "Scroll settle"), &mut state.user_config.mgr_scroll_delay),
                            ]);
                        });
                    });
                });
        });
}

/// Top action bar with port, start/stop button, and status.
fn action_bar(
    ui: &mut egui::Ui,
    state: &mut AppState,
    server_handle: &mut Option<TaskHandle>,
    is_server_running: bool,
    scan_running: bool,
) {
    let l = state.lang;

    if scan_running && !is_server_running {
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 50),
            l.t(
                "扫描正在进行，请等待完成",
                "Scan is running. Please wait for it to finish.",
            ),
        );
    }

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
            let status = state.server_status.lock().unwrap().clone();
            if let TaskStatus::Running(ref phase) = status {
                ui.spinner();
                ui.label(phase);
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(100, 200, 100),
                    format!(
                        "● {} {}",
                        l.t("运行中", "Running on port"),
                        state.server_port
                    ),
                );
            }
        } else {
            if ui.button(l.t("▶ 启动", "▶ Start")).clicked() {
                state.server_enabled.store(true, Ordering::Relaxed);
                // Force immediate save before starting server
                let _ = yas_genshin::cli::save_config(&state.user_config);
                state.config_dirty_since = None;
                *server_handle = Some(worker::spawn_server(state));
            }
        }
    });

    // Status from previous run
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
}

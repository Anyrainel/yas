use std::sync::atomic::Ordering;

use eframe::egui;

use super::state::{AppState, RefreshState, TaskStatus, UiText};
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
                        ui.checkbox(&mut state.hdr_mode, l.t("我的原神在使用HDR", "HDR mode"));
                    });
                });

            // === Timing Delays ===
            //
            // Scan API runs the same character/weapon/artifact scanners as the
            // scanner tab, so all their delays apply when the server executes a
            // scan job. Layout: character + inventory + manager in one row.
            egui::CollapsingHeader::new(l.t("延迟设置", "Timing Delays"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.add_enabled_ui(!is_server_running, |ui| {
                        let defaults = genshin_scanner::cli::GoodUserConfig::default();
                        ui.columns(3, |cols| {
                            widgets::delay_group(&mut cols[0], "char_delays", l.t("角色", "Character"), l, &mut [
                                (l.t("打开界面", "Open screen"), &mut state.user_config.char_open_delay, defaults.char_open_delay,
                                    l.t("打开角色界面后等待完全加载的时间", "Wait time for character screen to fully load after opening")),
                                (l.t("关闭界面", "Close screen"), &mut state.user_config.char_close_delay, defaults.char_close_delay,
                                    l.t("关闭角色界面后等待返回主界面的时间", "Wait time after closing character screen to return to main view")),
                                (l.t("面板切换", "Panel switch"), &mut state.user_config.char_tab_delay, defaults.char_tab_delay,
                                    l.t("切换角色详情标签页（天赋/命座等）后的等待", "Wait after switching character detail tabs (talents/constellations etc.)")),
                                (l.t("切换角色", "Next character"), &mut state.user_config.char_next_delay, defaults.char_next_delay,
                                    l.t("切换到下一个角色后等待面板更新的时间", "Wait after switching to next character for panel to update")),
                            ]);
                            widgets::inventory_delays(&mut cols[1], state, l);
                            widgets::delay_group(&mut cols[2], "mgr_delays", l.t("管理器", "Manager"), l, &mut [
                                (l.t("画面切换", "Screen transition"), &mut state.user_config.mgr_transition_delay, defaults.mgr_transition_delay,
                                    l.t("打开/关闭角色面板等大画面切换后的等待", "Wait after major screen transitions like opening/closing character panel")),
                                (l.t("操作按钮", "Action button"), &mut state.user_config.mgr_action_delay, defaults.mgr_action_delay,
                                    l.t("点击锁定/装备等操作按钮后的等待", "Wait after clicking action buttons like lock/equip")),
                                (l.t("格子点击", "Grid cell click"), &mut state.user_config.mgr_cell_delay, defaults.mgr_cell_delay,
                                    l.t("锁定切换后点击下一个格子前的等待", "Wait before clicking the next grid cell after a lock toggle")),
                                (l.t("滚动等待", "Scroll settle"), &mut state.user_config.mgr_scroll_delay, defaults.mgr_scroll_delay,
                                    l.t("翻页后等待物品列表稳定的时间", "Wait after scrolling for item list to stabilize")),
                            ]);
                        });
                    });
                });

            // === Advanced Options (shared with scanner tab) ===
            egui::CollapsingHeader::new(l.t("高级选项", "Advanced Options"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.add_enabled_ui(!is_server_running, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.checkbox(&mut state.verbose, l.t("详细信息", "Verbose"));
                            ui.checkbox(&mut state.dump_images, l.t("保存OCR截图 → debug_images/", "Dump OCR images → debug_images/"));
                            ui.checkbox(&mut state.dump_job_data, l.t("保存请求数据", "Dump request data"));
                        });

                        ui.add_space(4.0);
                        state.mappings_refresh.poll();
                        ui.horizontal(|ui| {
                            let busy = state.mappings_refresh.is_running();
                            if ui.add_enabled(!busy, egui::Button::new(
                                l.t("刷新游戏数据", "Refresh game data"),
                            )).clicked() {
                                state.mappings_refresh = RefreshState::Running(
                                    std::thread::spawn(|| {
                                        genshin_scanner::scanner::common::mappings::force_refresh()
                                            .map_err(|e| UiText::from_bilingual(format!("{}", e)))
                                    }),
                                );
                            }
                            match &state.mappings_refresh {
                                RefreshState::Ok => {
                                    ui.colored_label(egui::Color32::GREEN, "OK");
                                }
                                RefreshState::Failed(msg) => {
                                    ui.colored_label(egui::Color32::RED, msg.text(l));
                                }
                                RefreshState::Running(_) => {
                                    ui.spinner();
                                }
                                RefreshState::Idle => {}
                            }
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
        ui.add_enabled(
            !is_server_running,
            egui::DragValue::new(&mut state.server_port)
                .range(1024..=65535)
                .speed(0.0),
        );

        ui.add_space(12.0);

        if scan_running && !is_server_running {
            ui.add_enabled(
                false,
                egui::Button::new(l.t("▶ 启动HTTP服务器", "▶ Start HTTP Server")),
            );
        } else if is_server_running {
            if ui.button(l.t("■ 停止服务器", "■ Stop Server")).clicked() {
                if let Some(ref h) = server_handle {
                    h.stop();
                }
            }
            let status = state.server_status.lock().unwrap().clone();
            if let TaskStatus::Running(ref phase) = status {
                ui.spinner();
                ui.label(phase.text(l));
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
            if ui
                .button(l.t("▶ 启动HTTP服务器", "▶ Start HTTP Server"))
                .clicked()
            {
                if let Err(e) = super::privilege::ensure_admin_for_action() {
                    *state.server_status.lock().unwrap() =
                        TaskStatus::Failed(UiText::from_bilingual(format!("{}", e)));
                } else {
                    state.server_enabled.store(true, Ordering::Relaxed);
                    // Force immediate save before starting server
                    if let Err(e) = genshin_scanner::cli::save_config(&state.user_config) {
                        yas::log_warn!("配置保存失败: {}", "Config save failed: {}", e);
                    }
                    state.config_dirty_since = None;
                    *server_handle = Some(worker::spawn_server(state));
                }
            }
        }
    });

    // Status from previous run
    if !is_server_running {
        let status = state.server_status.lock().unwrap().clone();
        match status {
            TaskStatus::Failed(ref msg) => {
                ui.colored_label(egui::Color32::from_rgb(255, 100, 100), msg.text(l));
            },
            TaskStatus::Completed(ref msg) => {
                ui.colored_label(egui::Color32::from_rgb(150, 150, 150), msg.text(l));
            },
            _ => {},
        }
    }
}

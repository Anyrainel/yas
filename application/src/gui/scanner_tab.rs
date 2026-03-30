use eframe::egui;

use super::state::{AppState, TaskStatus};
use super::worker::{self, TaskHandle};

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    scan_handle: &mut Option<TaskHandle>,
    game_busy: bool,
) {
    let is_scanning = scan_handle.as_ref().map_or(false, |h| !h.is_finished());
    let l = state.lang;

    // === Action bar (always visible at top) ===
    ui.add_space(4.0);
    action_bar(ui, state, scan_handle, is_scanning, game_busy);
    if !is_scanning {
        ui.colored_label(
            egui::Color32::from_rgb(120, 120, 120),
            l.t(
                "请确认游戏已运行，扫描过程中可按鼠标右键终止。",
                "Make sure the game is running. Right-click to abort during scanning.",
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

        // === Character Names (always visible) ===
        let names_header = if state.names_need_attention {
            egui::RichText::new(l.t("⚠ 角色名称", "⚠ Character Names"))
                .color(egui::Color32::from_rgb(255, 200, 50))
                .strong()
        } else {
            egui::RichText::new(l.t("角色名称", "Character Names")).strong()
        };
        ui.label(names_header);
        ui.add_space(2.0);
        ui.add_enabled_ui(!is_scanning, |ui| {
            if state.names_need_attention {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 50),
                    l.t(
                        "请填写必填角色名称（旅行者、奇偶·男、奇偶·女），然后再次点击开始扫描。",
                        "Fill in the required names (Traveler, Manekin, Manekina), then click Start Scan again.",
                    ),
                );
                ui.add_space(4.0);
            } else {
                ui.label(l.t(
                    "这些角色可在游戏内改名，请填写您实际使用的名字（* 为必填）",
                    "These characters can be renamed in-game. Enter the names you actually use (* = required).",
                ));
            }
            ui.add_space(2.0);

            let required_color = if state.names_need_attention {
                egui::Color32::from_rgb(255, 200, 50)
            } else {
                ui.visuals().text_color()
            };

            // Two name fields per row using horizontal layouts
            let total_w = ui.available_width();
            // Reserve ~80px per label + 24px spacing between pairs
            let field_w = ((total_w - 80.0 * 2.0 - 24.0) / 2.0).max(80.0);
            ui.horizontal(|ui| {
                let traveler_empty = state.names_need_attention && state.user_config.traveler_name.trim().is_empty();
                let label_color = if traveler_empty { egui::Color32::from_rgb(255, 100, 100) } else { required_color };
                ui.colored_label(label_color, l.t("旅行者*", "Traveler*"));
                ui.add(egui::TextEdit::singleline(&mut state.user_config.traveler_name).desired_width(field_w));
                ui.add_space(16.0);
                ui.label(l.t("流浪者", "Wanderer"));
                ui.add(egui::TextEdit::singleline(&mut state.user_config.wanderer_name).desired_width(field_w));
            });
            ui.horizontal(|ui| {
                let manekin_empty = state.names_need_attention && state.user_config.manekin_name.trim().is_empty();
                let label_color = if manekin_empty { egui::Color32::from_rgb(255, 100, 100) } else { required_color };
                ui.colored_label(label_color, l.t("奇偶·男*", "Manekin*"));
                ui.add(egui::TextEdit::singleline(&mut state.user_config.manekin_name).desired_width(field_w));
                ui.add_space(16.0);
                let manekina_empty = state.names_need_attention && state.user_config.manekina_name.trim().is_empty();
                let label_color = if manekina_empty { egui::Color32::from_rgb(255, 100, 100) } else { required_color };
                ui.colored_label(label_color, l.t("奇偶·女*", "Manekina*"));
                ui.add(egui::TextEdit::singleline(&mut state.user_config.manekina_name).desired_width(field_w));
            });

            if state.names_need_attention {
                let required_filled = !state.user_config.traveler_name.trim().is_empty()
                    && !state.user_config.manekin_name.trim().is_empty()
                    && !state.user_config.manekina_name.trim().is_empty();
                if required_filled {
                    state.names_need_attention = false;
                }
            }
        });

        ui.add_space(8.0);

        // === Scan Targets (collapsible, horizontal) ===
        egui::CollapsingHeader::new(l.t("扫描目标", "Scan Targets"))
            .default_open(true)
            .show(ui, |ui| {
                ui.add_enabled_ui(!is_scanning, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut state.scan_characters, l.t("角色", "Characters"));
                        ui.add_space(12.0);
                        ui.checkbox(&mut state.scan_weapons, l.t("武器", "Weapons"));
                        ui.add_space(12.0);
                        ui.checkbox(&mut state.scan_artifacts, l.t("圣遗物", "Artifacts"));
                    });
                });
            });

        // === Timing Delays ===
        egui::CollapsingHeader::new(l.t("延迟设置", "Timing Delays"))
            .default_open(false)
            .show(ui, |ui| {
                ui.add_enabled_ui(!is_scanning, |ui| {
                    // Three delay groups side by side using columns
                    ui.columns(3, |cols| {
                        delay_group(&mut cols[0], "char_delays", l.t("角色", "Character"), &mut [
                            (l.t("面板切换", "Panel switch"), &mut state.user_config.char_tab_delay),
                            (l.t("切换角色", "Next character"), &mut state.user_config.char_next_delay),
                            (l.t("打开界面", "Open screen"), &mut state.user_config.char_open_delay),
                            (l.t("关闭界面", "Close screen"), &mut state.user_config.char_close_delay),
                        ]);
                        delay_group(&mut cols[1], "weapon_delays", l.t("武器", "Weapon"), &mut [
                            (l.t("切换物品", "Switch item"), &mut state.user_config.weapon_grid_delay),
                            (l.t("翻页", "Page scroll"), &mut state.user_config.weapon_scroll_delay),
                            (l.t("面板切换", "Panel switch"), &mut state.user_config.weapon_tab_delay),
                            (l.t("打开背包", "Open backpack"), &mut state.user_config.weapon_open_delay),
                        ]);
                        delay_group(&mut cols[2], "artifact_delays", l.t("圣遗物", "Artifact"), &mut [
                            (l.t("切换物品", "Switch item"), &mut state.user_config.artifact_grid_delay),
                            (l.t("翻页", "Page scroll"), &mut state.user_config.artifact_scroll_delay),
                            (l.t("面板切换", "Panel switch"), &mut state.user_config.artifact_tab_delay),
                            (l.t("打开背包", "Open backpack"), &mut state.user_config.artifact_open_delay),
                        ]);
                    });
                });
            });

        // === Advanced Options ===
        egui::CollapsingHeader::new(l.t("高级选项", "Advanced Options"))
            .default_open(false)
            .show(ui, |ui| {
                ui.add_enabled_ui(!is_scanning, |ui| {
                    // Checkboxes in a flowing horizontal layout
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut state.verbose, l.t("详细信息", "Verbose"));
                        ui.checkbox(&mut state.continue_on_failure, l.t("失败继续", "Continue on failure"));
                        ui.checkbox(&mut state.dump_images, l.t("保存OCR截图", "Dump OCR images"));
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(
                            &mut state.weapon_skip_delay,
                            l.t("跳过武器面板等待", "Skip weapon panel delay"),
                        );
                        ui.checkbox(
                            &mut state.artifact_skip_delay,
                            l.t("跳过圣遗物面板等待", "Skip artifact panel delay"),
                        );
                    });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(l.t("最大扫描数 (0=全部):", "Max count (0=all):"));
                        ui.add_space(8.0);
                        ui.label(l.t("角色:", "Char:"));
                        max_count_field(ui, &mut state.char_max_count);
                        ui.add_space(8.0);
                        ui.label(l.t("武器:", "Wpn:"));
                        max_count_field(ui, &mut state.weapon_max_count);
                        ui.add_space(8.0);
                        ui.label(l.t("圣遗物:", "Art:"));
                        max_count_field(ui, &mut state.artifact_max_count);
                    });
                });
            });
    });
}

/// Top action bar
fn action_bar(
    ui: &mut egui::Ui,
    state: &mut AppState,
    scan_handle: &mut Option<TaskHandle>,
    is_scanning: bool,
    game_busy: bool,
) {
    let l = state.lang;

    if game_busy && !is_scanning {
        ui.colored_label(
            egui::Color32::from_rgb(255, 200, 50),
            l.t(
                "管理器正在运行，请先停止后再扫描",
                "Manager is running. Stop it before scanning.",
            ),
        );
    }

    ui.horizontal(|ui| {
        if is_scanning {
            if ui.button(l.t("⏹ 停止扫描", "⏹ Stop Scan")).clicked() {
                yas::utils::set_abort();
            }
            let status = state.scan_status.lock().unwrap().clone();
            if let TaskStatus::Running(phase) = status {
                ui.spinner();
                ui.label(phase);
            }
        } else {
            let any_selected =
                state.scan_characters || state.scan_weapons || state.scan_artifacts;
            let can_scan = any_selected && !game_busy;
            if ui
                .add_enabled(can_scan, egui::Button::new(l.t("▶ 开始扫描", "▶ Start Scan")))
                .clicked()
            {
                let required_missing = state.user_config.traveler_name.trim().is_empty()
                    || state.user_config.manekin_name.trim().is_empty()
                    || state.user_config.manekina_name.trim().is_empty();

                if required_missing {
                    state.names_need_attention = true;
                    log::warn!("{}", l.t(
                        "旅行者、奇偶·男、奇偶·女为必填项",
                        "Traveler, Manekin, and Manekina names are required",
                    ));
                } else {
                    state.names_need_attention = false;
                    // Force immediate save before scanning (don't wait for debounce)
                    let _ = yas_genshin::cli::save_config(&state.user_config);
                    state.config_dirty_since = None;
                    *scan_handle = Some(worker::spawn_scan(state));
                }
            }
        }
    });

    let status = state.scan_status.lock().unwrap().clone();
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

fn delay_group(ui: &mut egui::Ui, id: &str, category: &str, fields: &mut [(&str, &mut u64)]) {
    ui.strong(category);
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            for (label, value) in fields.iter_mut() {
                ui.label(format!("  {} (ms):", label));
                num_input_u64(ui, value, 60.0);
                ui.end_row();
            }
        });
}

fn max_count_field(ui: &mut egui::Ui, value: &mut usize) {
    let mut buf = value.to_string();
    let response = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .desired_width(40.0)
            .horizontal_align(egui::Align::RIGHT),
    );
    if response.changed() {
        if let Ok(v) = buf.parse::<usize>() {
            *value = v.min(2000);
        } else if buf.is_empty() {
            *value = 0;
        }
    }
}

/// Numeric text input for u64 values (no drag behavior).
fn num_input_u64(ui: &mut egui::Ui, value: &mut u64, width: f32) {
    let mut buf = value.to_string();
    let response = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .desired_width(width)
            .horizontal_align(egui::Align::RIGHT),
    );
    if response.changed() {
        if let Ok(v) = buf.parse::<u64>() {
            *value = v.min(5000);
        } else if buf.is_empty() {
            *value = 0;
        }
    }
}

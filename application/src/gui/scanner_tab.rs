use eframe::egui;

use super::state::{AppState, TaskStatus};
use super::worker::{self, TaskHandle};

pub fn show(ui: &mut egui::Ui, state: &mut AppState, scan_handle: &mut Option<TaskHandle>) {
    let is_scanning = scan_handle.as_ref().map_or(false, |h| !h.is_finished());

    egui::ScrollArea::vertical().show(ui, |ui| {
        // === Character Names ===
        ui.heading("角色名称 / Character Names");
        ui.add_enabled_ui(!is_scanning, |ui| {
            egui::Grid::new("names_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("旅行者 / Traveler:");
                    ui.add(egui::TextEdit::singleline(&mut state.user_config.traveler_name).desired_width(200.0));
                    ui.end_row();

                    ui.label("流浪者 / Wanderer:");
                    ui.add(egui::TextEdit::singleline(&mut state.user_config.wanderer_name).desired_width(200.0));
                    ui.end_row();

                    ui.label("奇偶·男 / Manekin:");
                    ui.add(egui::TextEdit::singleline(&mut state.user_config.manekin_name).desired_width(200.0));
                    ui.end_row();

                    ui.label("奇偶·女 / Manekina:");
                    ui.add(egui::TextEdit::singleline(&mut state.user_config.manekina_name).desired_width(200.0));
                    ui.end_row();
                });
        });

        ui.add_space(8.0);
        ui.separator();

        // === Scan Targets ===
        ui.heading("扫描目标 / Scan Targets");
        ui.add_enabled_ui(!is_scanning, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut state.scan_characters, "角色 / Characters");
                ui.checkbox(&mut state.scan_weapons, "武器 / Weapons");
                ui.checkbox(&mut state.scan_artifacts, "圣遗物 / Artifacts");
            });
        });

        ui.add_space(8.0);
        ui.separator();

        // === Rarity Filters ===
        ui.heading("稀有度过滤 / Rarity Filters");
        ui.add_enabled_ui(!is_scanning, |ui| {
            egui::Grid::new("rarity_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("武器最低稀有度 / Min weapon rarity:");
                    ui.add(egui::Slider::new(&mut state.weapon_min_rarity, 1..=5));
                    ui.end_row();

                    ui.label("圣遗物最低稀有度 / Min artifact rarity:");
                    ui.add(egui::Slider::new(&mut state.artifact_min_rarity, 1..=5));
                    ui.end_row();
                });
        });

        ui.add_space(8.0);
        ui.separator();

        // === Timing Delays (collapsible) ===
        egui::CollapsingHeader::new("延迟设置 / Timing Delays")
            .default_open(false)
            .show(ui, |ui| {
                ui.add_enabled_ui(!is_scanning, |ui| {
                    egui::Grid::new("delay_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.strong("角色 / Character");
                            ui.end_row();
                            delay_field(ui, "Tab切换 / Tab switch (ms):", &mut state.user_config.char_tab_delay);
                            delay_field(ui, "打开延迟 / Open delay (ms):", &mut state.user_config.char_open_delay);

                            ui.end_row();
                            ui.strong("武器 / Weapon");
                            ui.end_row();
                            delay_field(ui, "格子延迟 / Grid delay (ms):", &mut state.user_config.weapon_grid_delay);
                            delay_field(ui, "滚动延迟 / Scroll delay (ms):", &mut state.user_config.weapon_scroll_delay);
                            delay_field(ui, "Tab切换 / Tab switch (ms):", &mut state.user_config.weapon_tab_delay);
                            delay_field(ui, "打开延迟 / Open delay (ms):", &mut state.user_config.weapon_open_delay);

                            ui.end_row();
                            ui.strong("圣遗物 / Artifact");
                            ui.end_row();
                            delay_field(ui, "格子延迟 / Grid delay (ms):", &mut state.user_config.artifact_grid_delay);
                            delay_field(ui, "滚动延迟 / Scroll delay (ms):", &mut state.user_config.artifact_scroll_delay);
                            delay_field(ui, "Tab切换 / Tab switch (ms):", &mut state.user_config.artifact_tab_delay);
                            delay_field(ui, "打开延迟 / Open delay (ms):", &mut state.user_config.artifact_open_delay);
                        });
                });
            });

        ui.add_space(8.0);
        ui.separator();

        // === Advanced Options (collapsible) ===
        egui::CollapsingHeader::new("高级选项 / Advanced Options")
            .default_open(false)
            .show(ui, |ui| {
                ui.add_enabled_ui(!is_scanning, |ui| {
                    ui.checkbox(&mut state.verbose, "详细信息 / Verbose");
                    ui.checkbox(&mut state.continue_on_failure, "失败继续 / Continue on failure");
                    ui.checkbox(&mut state.dump_images, "保存截图 / Dump images");
                    ui.checkbox(&mut state.weapon_skip_delay, "跳过武器面板等待 / Skip weapon panel delay");
                    ui.checkbox(&mut state.artifact_skip_delay, "跳过圣遗物面板等待 / Skip artifact panel delay");

                    ui.add_space(8.0);
                    egui::Grid::new("max_count_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("最大角色数 / Max characters (0=all):");
                            let mut v = state.char_max_count as i64;
                            if ui.add(egui::DragValue::new(&mut v).range(0..=1000).speed(1)).changed() {
                                state.char_max_count = v.max(0) as usize;
                            }
                            ui.end_row();

                            ui.label("最大武器数 / Max weapons (0=all):");
                            let mut v = state.weapon_max_count as i64;
                            if ui.add(egui::DragValue::new(&mut v).range(0..=2000).speed(1)).changed() {
                                state.weapon_max_count = v.max(0) as usize;
                            }
                            ui.end_row();

                            ui.label("最大圣遗物数 / Max artifacts (0=all):");
                            let mut v = state.artifact_max_count as i64;
                            if ui.add(egui::DragValue::new(&mut v).range(0..=2000).speed(1)).changed() {
                                state.artifact_max_count = v.max(0) as usize;
                            }
                            ui.end_row();
                        });
                });
            });

        ui.add_space(12.0);
        ui.separator();

        // === Action Buttons ===
        ui.horizontal(|ui| {
            if is_scanning {
                if ui.button("⏹ 停止扫描 / Stop Scan").clicked() {
                    yas::utils::set_abort();
                }
                let status = state.scan_status.lock().unwrap().clone();
                if let TaskStatus::Running(phase) = status {
                    ui.spinner();
                    ui.label(phase);
                }
            } else {
                let any_selected = state.scan_characters || state.scan_weapons || state.scan_artifacts;
                if ui.add_enabled(any_selected, egui::Button::new("▶ 开始扫描 / Start Scan")).clicked() {
                    // Save config before scanning
                    let _ = yas_genshin::cli::save_config(&state.user_config);
                    *scan_handle = Some(worker::spawn_scan(state));
                }

                if ui.button("💾 保存配置 / Save Config").clicked() {
                    match yas_genshin::cli::save_config(&state.user_config) {
                        Ok(()) => log::info!("配置已保存 / Config saved"),
                        Err(e) => log::error!("保存失败 / Save failed: {}", e),
                    }
                }
            }
        });

        // Show completion/error status
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
    });
}

fn delay_field(ui: &mut egui::Ui, label: &str, value: &mut u64) {
    ui.label(label);
    let mut v = *value as i64;
    if ui
        .add(egui::DragValue::new(&mut v).range(0..=5000).speed(10))
        .changed()
    {
        *value = v.max(0) as u64;
    }
    ui.end_row();
}

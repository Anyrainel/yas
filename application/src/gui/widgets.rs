//! Shared UI widgets used by both scanner and manager tabs.

use eframe::egui;

use super::state::{AppState, Lang};

/// Numeric input for u64 values (clamped to 5000).
pub fn num_input_u64(ui: &mut egui::Ui, value: &mut u64, _width: f32) {
    ui.add(
        egui::DragValue::new(value)
            .range(0..=5000)
            .speed(0.0),
    );
}

/// A labeled group of delay fields rendered in a 2-column grid.
/// Each field is `(label, value, default)`. Values below their default get a
/// warning asterisk, and a footnote is shown underneath when any field is below default.
pub fn delay_group(ui: &mut egui::Ui, id: &str, category: &str, l: Lang, fields: &mut [(&str, &mut u64, u64)]) {
    ui.strong(category);
    let mut any_below = false;
    let warn_color = egui::Color32::from_rgb(255, 200, 50);
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            for (label, value, default) in fields.iter_mut() {
                let below = **value < *default;
                if below {
                    any_below = true;
                    ui.colored_label(warn_color, format!("  {}* (ms):", label));
                } else {
                    ui.label(format!("  {} (ms):", label));
                }
                num_input_u64(ui, value, 60.0);
                ui.end_row();
            }
        });
    if any_below {
        ui.colored_label(
            warn_color,
            l.t("* 低于默认值，可能导致扫描不稳定", "* Below default — may cause unreliable scans"),
        );
    }
}

/// Character names section — shared between scanner and manager tabs.
/// Shows the 4 renameable character name fields with required-field validation.
pub fn character_names_section(ui: &mut egui::Ui, state: &mut AppState, enabled: bool) {
    let l = state.lang;

    let names_header = if state.names_need_attention {
        egui::RichText::new(l.t("⚠ 角色名称", "⚠ Character Names"))
            .color(egui::Color32::from_rgb(255, 200, 50))
            .strong()
    } else {
        egui::RichText::new(l.t("角色名称", "Character Names")).strong()
    };
    ui.label(names_header);
    ui.add_space(2.0);
    ui.add_enabled_ui(enabled, |ui| {
        if state.names_need_attention {
            ui.colored_label(
                egui::Color32::from_rgb(255, 200, 50),
                l.t(
                    "请填写必填角色名称（旅行者、奇偶·男性、奇偶·女性），然后再次点击开始扫描。",
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

        // Two name fields per row
        let total_w = ui.available_width();
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
            ui.colored_label(label_color, l.t("奇偶·男性*", "Manekin*"));
            ui.add(egui::TextEdit::singleline(&mut state.user_config.manekin_name).desired_width(field_w));
            ui.add_space(16.0);
            let manekina_empty = state.names_need_attention && state.user_config.manekina_name.trim().is_empty();
            let label_color = if manekina_empty { egui::Color32::from_rgb(255, 100, 100) } else { required_color };
            ui.colored_label(label_color, l.t("奇偶·女性*", "Manekina*"));
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
}

/// Inventory delay fields — shared between scanner and manager tabs.
/// Renders as a delay_group with the 4 inventory timing fields.
pub fn inventory_delays(ui: &mut egui::Ui, state: &mut AppState, l: Lang) {
    let defaults = yas_genshin::cli::GoodUserConfig::default();
    delay_group(ui, "inv_delays", l.t("背包", "Inventory"), l, &mut [
        (l.t("翻页等待", "Page scroll"), &mut state.user_config.inv_scroll_delay, defaults.inv_scroll_delay),
        (l.t("标签切换", "Tab switch"), &mut state.user_config.inv_tab_delay, defaults.inv_tab_delay),
        (l.t("打开背包", "Open backpack"), &mut state.user_config.inv_open_delay, defaults.inv_open_delay),
        (l.t("武器面板延迟", "Weapon panel delay"), &mut state.user_config.weapon_panel_delay, defaults.weapon_panel_delay),
        (l.t("圣遗物面板超时", "Artifact panel timeout"), &mut state.user_config.artifact_panel_timeout, defaults.artifact_panel_timeout),
        (l.t("圣遗物额外延迟", "Artifact extra delay"), &mut state.user_config.artifact_extra_delay, defaults.artifact_extra_delay),
    ]);
}

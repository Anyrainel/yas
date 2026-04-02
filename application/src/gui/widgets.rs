//! Shared UI widgets used by both scanner and manager tabs.

use eframe::egui;

use super::state::{AppState, Lang};

/// Numeric text input for u64 values (no drag behavior, clamped to 5000).
pub fn num_input_u64(ui: &mut egui::Ui, value: &mut u64, width: f32) {
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

/// A labeled group of delay fields rendered in a 2-column grid.
pub fn delay_group(ui: &mut egui::Ui, id: &str, category: &str, fields: &mut [(&str, &mut u64)]) {
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
    delay_group(ui, "inv_delays", l.t("背包", "Inventory"), &mut [
        (l.t("翻页等待", "Page scroll"), &mut state.user_config.inv_scroll_delay),
        (l.t("标签切换", "Tab switch"), &mut state.user_config.inv_tab_delay),
        (l.t("打开背包", "Open backpack"), &mut state.user_config.inv_open_delay),
        (l.t("截图前等待", "Pre-capture"), &mut state.user_config.capture_delay),
    ]);
}

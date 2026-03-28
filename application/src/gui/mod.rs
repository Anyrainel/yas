pub mod state;
pub mod log_bridge;
pub mod worker;
pub mod scanner_tab;
pub mod manager_tab;
pub mod log_panel;

use eframe::egui;
use state::{AppState, Lang};
use worker::TaskHandle;

/// Launch the GUI application.
pub fn run_gui() {
    let state = AppState::new();

    // Init GUI logger (replaces env_logger in GUI mode)
    let logger = log_bridge::GuiLogger::new(state.log_lines.clone(), 2000);
    logger.init();

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon_64.png"))
        .expect("Failed to load window icon");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 600.0])
            .with_min_inner_size([600.0, 400.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "GOOD Scanner",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(GuiApp::new(state)))
        }),
    )
    .unwrap();
}

#[derive(PartialEq)]
enum ActiveTab {
    Scanner,
    Manager,
}

struct GuiApp {
    state: AppState,
    active_tab: ActiveTab,
    scan_handle: Option<TaskHandle>,
    server_handle: Option<TaskHandle>,
    manage_handle: Option<TaskHandle>,
}

impl GuiApp {
    fn new(state: AppState) -> Self {
        Self {
            state,
            active_tab: ActiveTab::Scanner,
            scan_handle: None,
            server_handle: None,
            manage_handle: None,
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Debounced auto-save: check if config changed and save after 300ms
        self.state.auto_save_tick();

        let l = self.state.lang;

        // Top bar with tabs + language toggle
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.active_tab,
                    ActiveTab::Scanner,
                    egui::RichText::new(l.t("扫描器", "Scanner")).size(20.0),
                );
                ui.selectable_value(
                    &mut self.active_tab,
                    ActiveTab::Manager,
                    egui::RichText::new(l.t("管理器", "Manager")).size(20.0),
                );

                // Right-aligned language toggle
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = match l {
                        Lang::Zh => "EN",
                        Lang::En => "中",
                    };
                    if ui.button(egui::RichText::new(label).size(16.0)).clicked() {
                        self.state.lang = match l {
                            Lang::Zh => Lang::En,
                            Lang::En => Lang::Zh,
                        };
                        self.state.user_config.lang = self.state.lang.to_str().to_string();
                    }
                });
            });
        });

        // Bottom panel: shared log area
        egui::TopBottomPanel::bottom("logs")
            .min_height(120.0)
            .default_height(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                log_panel::show(ui, &self.state);
            });

        // Check cross-tab running states for mutual exclusion
        let is_scan_running = self.scan_handle.as_ref().map_or(false, |h| !h.is_finished());
        let is_server_running = self.server_handle.as_ref().map_or(false, |h| !h.is_finished());
        let is_manage_running = self.manage_handle.as_ref().map_or(false, |h| !h.is_finished());

        // Central panel: active tab content
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                ActiveTab::Scanner => {
                    scanner_tab::show(
                        ui,
                        &mut self.state,
                        &mut self.scan_handle,
                        is_server_running || is_manage_running,
                    );
                }
                ActiveTab::Manager => {
                    manager_tab::show(
                        ui,
                        &mut self.state,
                        &mut self.server_handle,
                        &mut self.manage_handle,
                        is_scan_running,
                    );
                }
            }
        });

        // Request repaint while tasks are running (for status updates)
        let any_running = is_scan_running || is_server_running || is_manage_running;
        if any_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Load system CJK font for Chinese text rendering.
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Try to load Microsoft YaHei from Windows system fonts
    let cjk_font_paths = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ];

    for path in &cjk_font_paths {
        if let Ok(font_data) = std::fs::read(path) {
            fonts.font_data.insert(
                "system_cjk".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(font_data)),
            );
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .push("system_cjk".to_owned());
            fonts
                .families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("system_cjk".to_owned());
            break;
        }
    }

    ctx.set_fonts(fonts);
}

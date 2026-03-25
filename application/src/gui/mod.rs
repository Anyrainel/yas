pub mod state;
pub mod log_bridge;
pub mod worker;
pub mod scanner_tab;
pub mod manager_tab;
pub mod log_panel;

use eframe::egui;
use state::AppState;
use worker::TaskHandle;

/// Launch the GUI application.
pub fn run_gui() {
    let state = AppState::new();

    // Init GUI logger (replaces env_logger in GUI mode)
    let logger = log_bridge::GuiLogger::new(state.log_lines.clone(), 2000);
    logger.init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 600.0])
            .with_min_inner_size([600.0, 400.0]),
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
        // Top bar with tabs
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.active_tab,
                    ActiveTab::Scanner,
                    "扫描器 / Scanner",
                );
                ui.selectable_value(
                    &mut self.active_tab,
                    ActiveTab::Manager,
                    "管理器 / Manager",
                );
            });
        });

        // Bottom panel: shared log area
        egui::TopBottomPanel::bottom("logs")
            .min_height(120.0)
            .default_height(180.0)
            .resizable(true)
            .show(ctx, |ui| {
                log_panel::show(ui, &self.state);
            });

        // Central panel: active tab content
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                ActiveTab::Scanner => {
                    scanner_tab::show(ui, &mut self.state, &mut self.scan_handle);
                }
                ActiveTab::Manager => {
                    manager_tab::show(
                        ui,
                        &mut self.state,
                        &mut self.server_handle,
                        &mut self.manage_handle,
                    );
                }
            }
        });

        // Request repaint while tasks are running (for status updates)
        let any_running = self
            .scan_handle
            .as_ref()
            .map_or(false, |h| !h.is_finished())
            || self
                .server_handle
                .as_ref()
                .map_or(false, |h| !h.is_finished())
            || self
                .manage_handle
                .as_ref()
                .map_or(false, |h| !h.is_finished());

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

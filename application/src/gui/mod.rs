#[cfg(feature = "capture")]
pub mod capture_tab;
pub mod credits;
pub mod log_bridge;
pub mod log_panel;
pub mod manager_tab;
pub mod scanner_tab;
pub mod state;
pub mod update_banner;
pub mod widgets;
pub mod worker;

use eframe::egui;
use state::{AppState, Lang, UpdateState};
use worker::TaskHandle;

/// Launch the GUI application.
pub fn run_gui() {
    // Clean up leftover .old exe from a previous update
    genshin_scanner::updater::cleanup_old_exe();

    // Install global SEH handler early — protects ALL threads including main.
    #[cfg(target_os = "windows")]
    worker::install_seh_handler();

    let state = AppState::new();

    // Set global language from config
    yas::lang::set_lang(state.lang.to_str());

    // Init GUI logger (replaces env_logger in GUI mode)
    let logger = log_bridge::GuiLogger::new(
        state.scanner_log_lines.clone(),
        state.manager_log_lines.clone(),
        2000,
    );
    logger.init(state.verbose);

    // Install a panic hook that writes to the log file (the default hook
    // writes to stderr, which GUI users never see).  This covers panics on
    // ALL threads — worker, update, refresh, and GUI main.
    install_panic_hook();

    // Kick off background update check
    update_banner::spawn_check(genshin_scanner::updater::ASSET_SCANNER, &state.update_state);

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon_64.png"))
        .expect("Failed to load window icon");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 800.0])
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

/// Replace the default panic hook so panics on ANY thread are written
/// to the `log::error!` logger (visible in the GUI log panel + log file).
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown".to_string()
        };
        let location = info
            .location()
            .map(|l| format!(" at {}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        log::error!(
            "{}",
            yas::lang::localize(&format!(
                "程序崩溃: {}{} / Program panicked: {}{}",
                payload, location, payload, location,
            ))
        );
    }));
}

#[derive(PartialEq)]
enum ActiveTab {
    Scanner,
    Manager,
    #[cfg(feature = "capture")]
    Capture,
    Credits,
}

struct GuiApp {
    state: AppState,
    active_tab: ActiveTab,
    scan_handle: Option<TaskHandle>,
    server_handle: Option<TaskHandle>,
    #[cfg(feature = "capture")]
    capture_tab: capture_tab::CaptureTabState,
}

impl GuiApp {
    fn new(state: AppState) -> Self {
        #[cfg(feature = "capture")]
        let output_dir = state.output_dir.clone();
        Self {
            state,
            active_tab: ActiveTab::Scanner,
            scan_handle: None,
            server_handle: None,
            #[cfg(feature = "capture")]
            capture_tab: capture_tab::CaptureTabState::new(output_dir),
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
                    egui::RichText::new(l.t("管理器 (beta)", "Manager (beta)")).size(20.0),
                );
                #[cfg(feature = "capture")]
                ui.selectable_value(
                    &mut self.active_tab,
                    ActiveTab::Capture,
                    egui::RichText::new(l.t("抓包", "Capture")).size(20.0),
                );

                // Right-aligned: language toggle + credits tab
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
                        yas::lang::set_lang(self.state.lang.to_str());
                    }
                    ui.selectable_value(
                        &mut self.active_tab,
                        ActiveTab::Credits,
                        egui::RichText::new(l.t("致谢", "Credits")).size(20.0),
                    );
                });
            });
        });

        // Update banner (between tabs and content)
        update_banner::show(ctx, self.state.lang, &self.state.update_state);

        // Bottom panel: per-tab log area.
        // Manager tab shows manager logs; everything else shows scanner logs
        // (scanner tab, capture tab, credits, plus startup/update logs).
        let log_buf = match self.active_tab {
            ActiveTab::Manager => &self.state.manager_log_lines,
            _ => &self.state.scanner_log_lines,
        };
        egui::TopBottomPanel::bottom("logs")
            .min_height(120.0)
            .default_height(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                log_panel::show_with(ui, self.state.lang, log_buf);
            });

        // Check cross-tab running states for mutual exclusion
        let is_scan_running = self
            .scan_handle
            .as_ref()
            .map_or(false, |h| !h.is_finished());
        let is_server_running = self
            .server_handle
            .as_ref()
            .map_or(false, |h| !h.is_finished());
        #[cfg(feature = "capture")]
        let is_capture_busy = self.capture_tab.is_busy();
        #[cfg(not(feature = "capture"))]
        let is_capture_busy = false;

        // Central panel: active tab content
        egui::CentralPanel::default().show(ctx, |ui| match self.active_tab {
            ActiveTab::Scanner => {
                scanner_tab::show(
                    ui,
                    &mut self.state,
                    &mut self.scan_handle,
                    is_server_running,
                );
            },
            ActiveTab::Manager => {
                manager_tab::show(
                    ui,
                    &mut self.state,
                    &mut self.server_handle,
                    is_scan_running,
                );
            },
            #[cfg(feature = "capture")]
            ActiveTab::Capture => {
                capture_tab::show(
                    ui,
                    self.state.lang,
                    &mut self.capture_tab,
                    is_scan_running || is_server_running,
                );
            },
            ActiveTab::Credits => {
                credits::show(ui, l, credits::CreditSet::Scanner);
            },
        });

        // Request repaint while tasks or update check are in progress
        let update_busy = matches!(
            *self.state.update_state.lock().unwrap(),
            UpdateState::Checking | UpdateState::Downloading | UpdateState::ShowingDialog,
        );
        let any_running = is_scan_running || is_server_running || is_capture_busy || update_busy;
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

pub mod state;
pub mod log_bridge;
pub mod worker;
pub mod widgets;
pub mod scanner_tab;
pub mod manager_tab;
#[cfg(feature = "capture")]
pub mod capture_tab;
pub mod log_panel;
pub mod credits;

use std::path::PathBuf;

use eframe::egui;
use state::{AppState, Lang, UpdateState};
use worker::TaskHandle;

/// Launch the GUI application.
pub fn run_gui() {
    // Clean up leftover .old exe from a previous update
    yas_genshin::updater::cleanup_old_exe();

    let state = AppState::new();

    // Set global language from config
    yas::lang::set_lang(state.lang.to_str());

    // Init GUI logger (replaces env_logger in GUI mode)
    let logger = log_bridge::GuiLogger::new(
        state.scanner_log_lines.clone(),
        state.manager_log_lines.clone(),
        2000,
    );
    logger.init();

    // Kick off background update check
    {
        let update_state = state.update_state.clone();
        std::thread::spawn(move || {
            match yas_genshin::updater::check_for_update() {
                Ok(yas_genshin::updater::UpdateStatus::UpdateAvailable {
                    latest_version,
                    download_url,
                    ..
                }) => {
                    *update_state.lock().unwrap() = UpdateState::Available {
                        latest_version,
                        download_url,
                    };
                }
                Ok(_) => {
                    *update_state.lock().unwrap() = UpdateState::None;
                }
                Err(e) => {
                    log::debug!("更新检查失败 / Update check failed: {}", e);
                    *update_state.lock().unwrap() = UpdateState::None;
                }
            }
        });
    }

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
        show_update_banner(ctx, &self.state);

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
        let is_scan_running = self.scan_handle.as_ref().map_or(false, |h| !h.is_finished());
        let is_server_running = self.server_handle.as_ref().map_or(false, |h| !h.is_finished());
        #[cfg(feature = "capture")]
        let is_capture_busy = self.capture_tab.is_busy();
        #[cfg(not(feature = "capture"))]
        let is_capture_busy = false;

        // Central panel: active tab content
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                ActiveTab::Scanner => {
                    scanner_tab::show(
                        ui,
                        &mut self.state,
                        &mut self.scan_handle,
                        is_server_running,
                    );
                }
                ActiveTab::Manager => {
                    manager_tab::show(
                        ui,
                        &mut self.state,
                        &mut self.server_handle,
                        is_scan_running,
                    );
                }
                #[cfg(feature = "capture")]
                ActiveTab::Capture => {
                    capture_tab::show(
                        ui,
                        self.state.lang,
                        &mut self.capture_tab,
                        is_scan_running || is_server_running,
                    );
                }
                ActiveTab::Credits => {
                    credits::show(ui, l);
                }
            }
        });

        // Request repaint while tasks or update check are in progress
        let update_busy = matches!(
            *self.state.update_state.lock().unwrap(),
            UpdateState::Checking | UpdateState::Downloading | UpdateState::ShowingDialog,
        );
        let any_running =
            is_scan_running || is_server_running || is_capture_busy || update_busy;
        if any_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Show the update notification banner when an update is available.
fn show_update_banner(ctx: &egui::Context, state: &AppState) {
    let l = state.lang;
    let update_state = state.update_state.lock().unwrap().clone();

    let show = !matches!(update_state, UpdateState::None | UpdateState::Checking);
    if !show {
        return;
    }

    egui::TopBottomPanel::top("update_banner").show(ctx, |ui| {
        match update_state {
            UpdateState::Available {
                ref latest_version,
                ref download_url,
            } => {
                ui.horizontal(|ui| {
                    let current = yas_genshin::updater::current_version_display();
                    ui.label(
                        egui::RichText::new(l.t(
                            &format!("发现新版本: {} → {}", current, latest_version),
                            &format!("Update available: {} → {}", current, latest_version),
                        ))
                        .color(egui::Color32::from_rgb(255, 200, 50)),
                    );
                    if ui.button(l.t("下载更新", "Download Update")).clicked() {
                        let update_state_arc = state.update_state.clone();
                        let url = download_url.clone();
                        let lang = l;
                        *state.update_state.lock().unwrap() = UpdateState::Downloading;
                        std::thread::spawn(move || {
                            match yas_genshin::updater::download_and_replace(&url) {
                                Ok(exe_path) => {
                                    *update_state_arc.lock().unwrap() =
                                        UpdateState::ShowingDialog;
                                    show_restart_dialog(exe_path, update_state_arc, lang);
                                }
                                Err(e) => {
                                    *update_state_arc.lock().unwrap() =
                                        UpdateState::Failed(format!("{}", e));
                                }
                            }
                        });
                    }
                    if ui.button(l.t("跳过", "Skip")).clicked() {
                        *state.update_state.lock().unwrap() = UpdateState::None;
                    }
                });
            }
            UpdateState::Downloading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(l.t("正在下载更新...", "Downloading update..."));
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            UpdateState::ShowingDialog => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(l.t(
                            "更新已就绪...",
                            "Update ready...",
                        ))
                        .color(egui::Color32::from_rgb(100, 255, 100)),
                    );
                });
            }
            UpdateState::Ready => {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(l.t(
                            "更新已就绪，请重启程序。",
                            "Update ready. Please restart the application.",
                        ))
                        .color(egui::Color32::from_rgb(100, 255, 100)),
                    );
                });
            }
            UpdateState::Failed(ref msg) => {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(l.t(
                            &format!("更新失败: {}", msg),
                            &format!("Update failed: {}", msg),
                        ))
                        .color(egui::Color32::from_rgb(255, 100, 100)),
                    );
                    if ui.button(l.t("关闭", "Dismiss")).clicked() {
                        *state.update_state.lock().unwrap() = UpdateState::None;
                    }
                });
            }
            _ => {}
        }
    });
}

/// Show a native OS dialog asking the user to restart now or later.
/// Called from the download background thread (blocking is fine).
fn show_restart_dialog(
    exe_path: PathBuf,
    update_state: std::sync::Arc<std::sync::Mutex<UpdateState>>,
    lang: Lang,
) {
    let (title, description) = match lang {
        Lang::Zh => (
            "更新完成",
            "更新已下载完成。是否立即重启？",
        ),
        Lang::En => (
            "Update Complete",
            "The update has been downloaded. Restart now?",
        ),
    };

    let result = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();

    match result {
        rfd::MessageDialogResult::Yes => {
            log::info!("用户选择立即重启 / User chose to restart now");
            match std::process::Command::new(&exe_path).spawn() {
                Ok(_) => std::process::exit(0),
                Err(e) => {
                    log::error!(
                        "启动新版本失败 / Failed to launch new version: {}",
                        e
                    );
                    *update_state.lock().unwrap() = UpdateState::Ready;
                }
            }
        }
        _ => {
            log::info!("用户选择稍后重启 / User chose to restart later");
            *update_state.lock().unwrap() = UpdateState::Ready;
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

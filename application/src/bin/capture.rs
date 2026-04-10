//! Standalone GOOD Capture binary — packet-sniffing scanner in its own exe.
//!
//! Separated from GOODScanner.exe to avoid antivirus false positives caused by
//! mixing packet capture with input simulation in a single binary.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use eframe::egui;

use yas_application::gui::capture_tab::CaptureTabState;
use yas_application::gui::log_bridge;
use yas_application::gui::log_panel;
use yas_application::gui::state::{Lang, LogEntry};
use yas_application::gui::{capture_tab, state};

fn main() {
    let lang = {
        let cfg = yas_genshin::cli::load_config_or_default();
        state::Lang::from_str(&cfg.lang)
    };
    yas::lang::set_lang(lang.to_str());

    let log_lines: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::with_capacity(1000)));
    // Standalone capture binary: route both sources to the same buffer.
    let manager_log_lines: Arc<Mutex<Vec<LogEntry>>> = log_lines.clone();

    // Init GUI logger
    let logger = log_bridge::GuiLogger::new(log_lines.clone(), manager_log_lines, 2000);
    logger.init();

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon_64.png"))
        .expect("Failed to load window icon");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 500.0])
            .with_min_inner_size([500.0, 350.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };

    let output_dir = yas_genshin::cli::exe_dir().display().to_string();

    eframe::run_native(
        "GOOD Capture",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(CaptureApp {
                lang,
                log_lines,
                capture_tab: CaptureTabState::new(output_dir),
            }))
        }),
    )
    .unwrap();
}

struct CaptureApp {
    lang: Lang,
    log_lines: Arc<Mutex<Vec<LogEntry>>>,
    capture_tab: CaptureTabState,
}

impl eframe::App for CaptureApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let l = self.lang;

        // Top bar: title + language toggle
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(l.t("抓包", "Capture")).size(20.0),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = match l {
                        Lang::Zh => "EN",
                        Lang::En => "中",
                    };
                    if ui.button(egui::RichText::new(label).size(16.0)).clicked() {
                        self.lang = match l {
                            Lang::Zh => Lang::En,
                            Lang::En => Lang::Zh,
                        };
                        yas::lang::set_lang(self.lang.to_str());
                    }
                });
            });
        });

        // Bottom panel: log area
        egui::TopBottomPanel::bottom("logs")
            .min_height(100.0)
            .default_height(200.0)
            .resizable(true)
            .show(ctx, |ui| {
                log_panel::show_with(ui, self.lang, &self.log_lines);
            });

        // Central panel: capture tab content
        egui::CentralPanel::default().show(ctx, |ui| {
            capture_tab::show(ui, self.lang, &mut self.capture_tab, false);
        });

        // Request repaint while capture is busy
        if self.capture_tab.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Load system CJK font for Chinese text rendering.
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let cjk_font_paths = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
    ];

    for path in &cjk_font_paths {
        if let Ok(font_data) = std::fs::read(path) {
            fonts.font_data.insert(
                "system_cjk".to_owned(),
                Arc::new(egui::FontData::from_owned(font_data)),
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

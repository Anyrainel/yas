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
use yas_application::gui::{capture_tab, credits, state};

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
            .with_inner_size([620.0, 560.0])
            .with_min_inner_size([500.0, 400.0])
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
                active_tab: ActiveTab::Capture,
                log_lines,
                capture_tab: CaptureTabState::new(output_dir),
            }))
        }),
    )
    .unwrap();
}

#[derive(PartialEq)]
enum ActiveTab {
    Capture,
    Credits,
}

struct CaptureApp {
    lang: Lang,
    active_tab: ActiveTab,
    log_lines: Arc<Mutex<Vec<LogEntry>>>,
    capture_tab: CaptureTabState,
}

impl eframe::App for CaptureApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let l = self.lang;

        // Top bar: tabs + language toggle
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
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
                        self.lang = match l {
                            Lang::Zh => Lang::En,
                            Lang::En => Lang::Zh,
                        };
                        yas::lang::set_lang(self.lang.to_str());
                    }
                    ui.selectable_value(
                        &mut self.active_tab,
                        ActiveTab::Credits,
                        egui::RichText::new(l.t("致谢", "Credits")).size(20.0),
                    );
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

        // Central panel: active tab content
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                ActiveTab::Capture => {
                    capture_tab::show(ui, l, &mut self.capture_tab, false);
                    show_help_section(ui, l);
                }
                ActiveTab::Credits => {
                    credits::show(ui, l);
                }
            }
        });

        // Request repaint while capture is busy
        if self.capture_tab.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Collapsible help section with usage instructions and AV guidance.
fn show_help_section(ui: &mut egui::Ui, l: Lang) {
    ui.add_space(8.0);
    egui::CollapsingHeader::new(l.t("使用说明", "How to use"))
        .default_open(false)
        .show(ui, |ui| {
            let steps = match l {
                Lang::Zh => [
                    "1. 点击「开始抓包」后，软件开始监听网络数据包。",
                    "2. 如果游戏已在运行，请关闭并重新启动，登录进入游戏（过门）。",
                    "3. 软件会在收到角色和物品数据后自动停止并导出 JSON 文件。",
                    "4. 导出的文件可直接导入到 ggartifact.com 等工具中使用。",
                ],
                Lang::En => [
                    "1. Click 'Start Capture' to begin listening for network packets.",
                    "2. If the game is already running, close it, relaunch, and log in (enter door).",
                    "3. Once character and item data are received, capture stops automatically and exports a JSON file.",
                    "4. The exported file can be imported directly into ggartifact.com and similar tools.",
                ],
            };
            for step in &steps {
                ui.label(*step);
            }
        });

    egui::CollapsingHeader::new(l.t("杀毒软件误报说明", "Antivirus false positive info"))
        .default_open(false)
        .show(ui, |ui| {
            ui.label(l.t(
                "本程序使用网络抓包（pktmon）来读取游戏数据。\n\
                 某些杀毒软件可能会将此行为标记为可疑。\n\
                 这是误报——本程序不会修改游戏文件或内存，\n\
                 仅被动读取网络流量。",
                "This program uses packet capture (pktmon) to read game data.\n\
                 Some antivirus software may flag this behavior as suspicious.\n\
                 This is a false positive — the program does not modify game\n\
                 files or memory; it only passively reads network traffic.",
            ));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(l.t(
                    "如果被拦截，请将本程序添加到杀毒软件的白名单中。",
                    "If blocked, please add this program to your antivirus whitelist.",
                ))
                .weak()
                .size(11.0),
            );
        });
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

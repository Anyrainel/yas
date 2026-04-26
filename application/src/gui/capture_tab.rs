use std::sync::{Arc, Mutex};

use eframe::egui;

use super::state::{self, Lang};

use yas_genshin::capture::monitor::{CaptureCommand, CaptureState};
use yas_genshin::capture::player_data::CaptureExportSettings;
use yas_genshin::scanner::common::models::GoodExport;

/// Handle to the capture monitor running on a background tokio runtime.
pub struct CaptureHandle {
    _thread: std::thread::JoinHandle<()>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<CaptureCommand>,
}

impl CaptureHandle {
    pub fn send(&self, cmd: CaptureCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn is_finished(&self) -> bool {
        self._thread.is_finished()
    }
}

/// Pending export result (polled each frame).
struct PendingExport {
    rx: tokio::sync::oneshot::Receiver<anyhow::Result<GoodExport>>,
}

/// Lifecycle phases for the capture tab.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// Nothing running yet. Show Start button.
    Idle,
    /// Background thread initializing (downloading data cache, loading keys).
    Initializing,
    /// Capture active, waiting for game packets.
    Waiting,
    /// All data received — auto-exporting.
    Exporting,
    /// Done — file written.
    Done { summary: String, path: String },
    /// Something failed.
    Failed(String),
}

/// State specific to the capture tab (lives in GuiApp, not AppState).
pub struct CaptureTabState {
    pub handle: Option<CaptureHandle>,
    pub capture_state: Arc<Mutex<CaptureState>>,
    phase: Phase,
    pending_export: Option<PendingExport>,

    // Export settings
    pub include_characters: bool,
    pub include_weapons: bool,
    pub include_artifacts: bool,
    pub output_dir: String,

    // Advanced
    pub dump_packets: bool,
    pub data_cache_refresh: state::RefreshState,
}

impl CaptureTabState {
    pub fn new(output_dir: String) -> Self {
        Self {
            handle: None,
            capture_state: Arc::new(Mutex::new(CaptureState::default())),
            phase: Phase::Idle,
            pending_export: None,
            include_characters: true,
            include_weapons: true,
            include_artifacts: true,
            output_dir,
            dump_packets: false,
            data_cache_refresh: state::RefreshState::Idle,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Initializing | Phase::Waiting | Phase::Exporting
        )
    }
}

/// Spawn the capture monitor on a background thread with a tokio runtime.
fn spawn_capture(
    capture_state: Arc<Mutex<CaptureState>>,
    cmd_tx_out: &mut Option<tokio::sync::mpsc::UnboundedSender<CaptureCommand>>,
    dump_packets: bool,
) -> std::thread::JoinHandle<()> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    *cmd_tx_out = Some(cmd_tx.clone());

    let state = capture_state.clone();

    std::thread::spawn(move || {
        let state_for_crash = state.clone();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    yas::log_error!(
                        "创建运行时失败: {}",
                        "Failed to create runtime: {}",
                        e
                    );
                    if let Ok(mut s) = state.lock() {
                        s.error = Some(format!("{}", e));
                    }
                    return;
                }
            };

            rt.block_on(async {
                let monitor = match yas_genshin::capture::monitor::CaptureMonitor::new(
                    state.clone(),
                    dump_packets,
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        yas::log_error!(
                            "初始化抓包监控失败: {}",
                            "Failed to initialize capture monitor: {}",
                            e
                        );
                        if let Ok(mut s) = state.lock() {
                            s.error = Some(format!("{}", e));
                        }
                        return;
                    }
                };

                // Initialization succeeded — immediately start capture
                let _ = cmd_tx.send(CaptureCommand::StartCapture);

                monitor.run(cmd_rx).await;
            });
        }));

        if let Err(panic_info) = result {
            let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                format!("Capture crashed: {}", s)
            } else if let Some(s) = panic_info.downcast_ref::<String>() {
                format!("Capture crashed: {}", s)
            } else {
                "Capture crashed (unknown panic)".to_string()
            };
            yas::log_error!("抓包崩溃: {}", "Capture crashed: {}", msg);
            if let Ok(mut s) = state_for_crash.lock() {
                s.error = Some(msg);
            }
        }
    })
}

pub fn show(
    ui: &mut egui::Ui,
    l: Lang,
    tab: &mut CaptureTabState,
    game_busy: bool,
) {
    // --- Phase transitions driven by shared state ---
    update_phase(tab, l);

    let is_busy = tab.is_busy();

    // === Action bar (always visible at top) ===
    ui.add_space(4.0);
    action_bar(ui, l, tab, game_busy);
    if !is_busy {
        ui.colored_label(
            egui::Color32::from_rgb(120, 120, 120),
            l.t(
                "通过抓包获取游戏数据（角色/武器/圣遗物），需管理员权限。",
                "Capture game data (characters/weapons/artifacts) via packet sniffing. Requires admin.",
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

            // === Export Settings ===
            egui::CollapsingHeader::new(l.t("导出设置", "Export Settings"))
                .default_open(true)
                .show(ui, |ui| {
                    ui.add_enabled_ui(!is_busy, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut tab.include_characters, l.t("角色", "Characters"));
                            ui.add_space(12.0);
                            ui.checkbox(&mut tab.include_weapons, l.t("武器", "Weapons"));
                            ui.add_space(12.0);
                            ui.checkbox(&mut tab.include_artifacts, l.t("圣遗物", "Artifacts"));
                        });
                    });
                });

            // === Advanced settings ===
            egui::CollapsingHeader::new(l.t("高级设置", "Advanced"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.checkbox(
                        &mut tab.dump_packets,
                        l.t(
                            "转储所有解密数据包 → debug_capture/",
                            "Dump all decrypted packets → debug_capture/",
                        ),
                    );

                    tab.data_cache_refresh.poll();
                    ui.horizontal(|ui| {
                        let busy = tab.data_cache_refresh.is_running();
                        if ui.add_enabled(!busy, egui::Button::new(
                            l.t("刷新游戏数据缓存", "Refresh game data"),
                        )).clicked() {
                            tab.data_cache_refresh = state::RefreshState::Running(
                                std::thread::spawn(|| {
                                    yas_genshin::capture::data_cache::force_refresh()
                                        .map_err(|e| format!("{}", e))
                                }),
                            );
                        }
                        match &tab.data_cache_refresh {
                            state::RefreshState::Ok => {
                                ui.colored_label(egui::Color32::GREEN, "OK");
                            }
                            state::RefreshState::Failed(msg) => {
                                ui.colored_label(egui::Color32::RED, msg.as_str());
                            }
                            state::RefreshState::Running(_) => {
                                ui.spinner();
                            }
                            state::RefreshState::Idle => {}
                        }
                    });
                });

            // === Help / FAQ ===
            egui::CollapsingHeader::new(l.t("使用说明", "How to use"))
                .default_open(false)
                .show(ui, |ui| {
                    let steps = match l {
                        Lang::Zh => &[
                            "1. 点击「开始抓包」后，软件开始监听网络数据包。",
                            "2. 如果游戏已在运行，请关闭并重新启动，登录进入游戏（过门）。",
                            "3. 软件会在收到角色和物品数据后自动停止并导出 JSON 文件。",
                            "4. 导出的文件可直接导入到 ggartifact.com 等工具中使用。",
                        ] as &[&str],
                        Lang::En => &[
                            "1. Click 'Start Capture' to begin listening for network packets.",
                            "2. If the game is already running, close it, relaunch, and log in (enter door).",
                            "3. Once character and item data are received, capture stops automatically and exports a JSON file.",
                            "4. The exported file can be imported directly into ggartifact.com and similar tools.",
                        ],
                    };
                    for step in steps {
                        ui.label(*step);
                    }
                });
        });
}

/// Top action bar: start/stop button + inline status.
fn action_bar(
    ui: &mut egui::Ui,
    l: Lang,
    tab: &mut CaptureTabState,
    game_busy: bool,
) {
    match &tab.phase {
        Phase::Idle => {
            if game_busy {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 50),
                    l.t(
                        "其他任务正在运行，请等待完成",
                        "Another task is running. Please wait for it to finish.",
                    ),
                );
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !game_busy,
                        egui::Button::new(l.t("▶ 开始抓包", "▶ Start Capture")),
                    )
                    .clicked()
                {
                    tab.capture_state = Arc::new(Mutex::new(CaptureState::default()));
                    let mut cmd_tx = None;
                    let thread = spawn_capture(tab.capture_state.clone(), &mut cmd_tx, tab.dump_packets);
                    tab.handle = Some(CaptureHandle {
                        _thread: thread,
                        cmd_tx: cmd_tx.unwrap(),
                    });
                    tab.phase = Phase::Initializing;
                }
            });
        }

        Phase::Initializing => {
            ui.horizontal(|ui| {
                if ui.button(l.t("⏹ 停止抓包", "⏹ Stop Capture")).clicked() {
                    if let Some(ref h) = tab.handle {
                        h.send(CaptureCommand::StopCapture);
                    }
                    tab.phase = Phase::Idle;
                    tab.handle = None;
                }
                ui.spinner();
                ui.label(l.t(
                    "正在初始化（下载数据缓存）...",
                    "Initializing (downloading data cache)...",
                ));
            });
        }

        Phase::Waiting => {
            ui.horizontal(|ui| {
                if ui.button(l.t("⏹ 停止抓包", "⏹ Stop Capture")).clicked() {
                    if let Some(ref h) = tab.handle {
                        h.send(CaptureCommand::StopCapture);
                    }
                    tab.phase = Phase::Idle;
                    tab.handle = None;
                }
                ui.colored_label(
                    egui::Color32::from_rgb(100, 200, 100),
                    l.t(
                        "● 正在等待游戏数据...",
                        "● Waiting for game data...",
                    ),
                );
            });

            ui.colored_label(
                egui::Color32::from_rgb(120, 120, 120),
                l.t(
                    "请关闭游戏并重新启动，登录（过门）。",
                    "Please close the game, relaunch, and log in (enter door).",
                ),
            );

            // Show partial progress
            if let Ok(cs) = tab.capture_state.lock() {
                if cs.has_characters || cs.has_items {
                    let mut parts = Vec::new();
                    if cs.has_characters {
                        parts.push(match l {
                            Lang::Zh => format!("角色: {}", cs.character_count),
                            Lang::En => format!("Characters: {}", cs.character_count),
                        });
                    }
                    if cs.has_items {
                        parts.push(match l {
                            Lang::Zh => format!(
                                "武器: {}, 圣遗物: {}",
                                cs.weapon_count, cs.artifact_count
                            ),
                            Lang::En => format!(
                                "Weapons: {}, Artifacts: {}",
                                cs.weapon_count, cs.artifact_count
                            ),
                        });
                    }
                    ui.colored_label(
                        egui::Color32::from_rgb(100, 200, 100),
                        parts.join("  |  "),
                    );

                    let missing = match (cs.has_characters, cs.has_items) {
                        (true, false) => Some(l.t(
                            "等待物品数据...",
                            "Waiting for item data...",
                        )),
                        (false, true) => Some(l.t(
                            "等待角色数据...",
                            "Waiting for character data...",
                        )),
                        _ => None,
                    };
                    if let Some(hint) = missing {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 200, 50),
                            hint,
                        );
                    }
                }
            }
        }

        Phase::Exporting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(l.t("正在导出...", "Exporting..."));
            });
        }

        Phase::Done { summary, path } => {
            let summary = summary.clone();
            let path = path.clone();
            ui.horizontal(|ui| {
                if ui.button(l.t("↻ 重新抓包", "↻ Recapture")).clicked() {
                    tab.phase = Phase::Idle;
                    tab.handle = None;
                }
                ui.colored_label(
                    egui::Color32::from_rgb(100, 200, 100),
                    &summary,
                );
            });
            ui.label(
                egui::RichText::new(format!("→ {}", path))
                    .size(11.0)
                    .weak(),
            );
        }

        Phase::Failed(msg) => {
            let msg = msg.clone();
            ui.horizontal(|ui| {
                if ui.button(l.t("↻ 重试", "↻ Retry")).clicked() {
                    tab.phase = Phase::Idle;
                    tab.handle = None;
                }
                ui.colored_label(
                    egui::Color32::from_rgb(255, 100, 100),
                    &msg,
                );
            });
        }
    }
}

/// Drive phase transitions based on shared capture state.
fn update_phase(tab: &mut CaptureTabState, l: Lang) {
    // Poll pending export
    if let Some(ref mut pending) = tab.pending_export {
        match pending.rx.try_recv() {
            Ok(Ok(export)) => {
                let timestamp = yas_genshin::cli::chrono_timestamp();
                let filename = format!("genshin_export_{}.json", timestamp);
                let path = std::path::Path::new(&tab.output_dir).join(&filename);
                match serde_json::to_string_pretty(&export) {
                    Ok(json) => match std::fs::write(&path, &json) {
                        Ok(_) => {
                            let cc = export.characters.as_ref().map_or(0, |v| v.len());
                            let wc = export.weapons.as_ref().map_or(0, |v| v.len());
                            let ac = export.artifacts.as_ref().map_or(0, |v| v.len());
                            let summary = match l {
                                Lang::Zh => format!(
                                    "已导出: {} 角色, {} 武器, {} 圣遗物",
                                    cc, wc, ac
                                ),
                                Lang::En => format!(
                                    "Exported: {} characters, {} weapons, {} artifacts",
                                    cc, wc, ac
                                ),
                            };
                            yas::log_info!(
                                "{} → {}",
                                "{} → {}",
                                summary, path.display()
                            );
                            tab.phase = Phase::Done {
                                summary,
                                path: path.display().to_string(),
                            };
                        }
                        Err(e) => {
                            tab.phase = Phase::Failed(format!(
                                "{}: {}",
                                l.t("写入文件失败", "Failed to write file"),
                                e
                            ));
                        }
                    },
                    Err(e) => {
                        tab.phase = Phase::Failed(format!(
                            "{}: {}",
                            l.t("序列化失败", "Serialization failed"),
                            e
                        ));
                    }
                }
                tab.pending_export = None;
                return;
            }
            Ok(Err(e)) => {
                tab.phase = Phase::Failed(format!(
                    "{}: {}",
                    l.t("导出失败", "Export failed"),
                    e
                ));
                tab.pending_export = None;
                return;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                return; // still waiting
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                tab.phase = Phase::Failed(
                    l.t("导出通道关闭", "Export channel closed").into(),
                );
                tab.pending_export = None;
                return;
            }
        }
    }

    // Check for errors from background thread
    if matches!(tab.phase, Phase::Initializing | Phase::Waiting) {
        if let Ok(cs) = tab.capture_state.lock() {
            if let Some(ref err) = cs.error {
                tab.phase = Phase::Failed(err.clone());
                return;
            }
        }

        // Check if monitor thread died unexpectedly
        if tab.handle.as_ref().map_or(false, |h| h.is_finished()) {
            let has_error = tab
                .capture_state
                .lock()
                .map_or(false, |s| s.error.is_some());
            if !has_error {
                tab.phase = Phase::Failed(
                    l.t("抓包进程意外退出", "Capture process exited unexpectedly")
                        .into(),
                );
            }
            return;
        }
    }

    // Transition: Initializing → Waiting (when capture starts)
    if tab.phase == Phase::Initializing {
        if tab.capture_state.lock().map_or(false, |s| s.capturing) {
            tab.phase = Phase::Waiting;
        }
    }

    // Transition: Waiting → auto-export (when capture auto-stopped with complete data)
    if tab.phase == Phase::Waiting {
        if tab.capture_state.lock().map_or(false, |s| s.complete) {
            // Automatically trigger export
            let settings = CaptureExportSettings {
                include_characters: tab.include_characters,
                include_weapons: tab.include_weapons,
                include_artifacts: tab.include_artifacts,
                ..Default::default()
            };
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Some(ref h) = tab.handle {
                h.send(CaptureCommand::Export { settings, reply: tx });
                tab.pending_export = Some(PendingExport { rx });
                tab.phase = Phase::Exporting;
            }
        }
    }
}

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::state::{Lang, UpdateState};

/// Show the update notification banner when an update is available.
///
/// Call this from the `eframe::App::update` method, before the central panel.
/// `update_state` is the shared state that tracks the update lifecycle.
pub fn show(ctx: &egui::Context, l: Lang, update_state: &Arc<Mutex<UpdateState>>) {
    let state_snapshot = update_state.lock().unwrap().clone();

    let show = !matches!(state_snapshot, UpdateState::None | UpdateState::Checking);
    if !show {
        return;
    }

    egui::TopBottomPanel::top("update_banner").show(ctx, |ui| match state_snapshot {
        UpdateState::Available {
            ref latest_version,
            ref download_url,
        } => {
            ui.horizontal(|ui| {
                let current = genshin_scanner::updater::current_version_display();
                ui.label(
                    egui::RichText::new(l.t(
                        &format!("发现新版本: {} → {}", current, latest_version),
                        &format!("Update available: {} → {}", current, latest_version),
                    ))
                    .color(egui::Color32::from_rgb(255, 200, 50)),
                );
                if ui.button(l.t("下载更新", "Download Update")).clicked() {
                    let arc = update_state.clone();
                    let url = download_url.clone();
                    let lang = l;
                    *update_state.lock().unwrap() = UpdateState::Downloading;
                    std::thread::spawn(
                        move || match genshin_scanner::updater::download_and_replace(&url) {
                            Ok(exe_path) => {
                                *arc.lock().unwrap() = UpdateState::ShowingDialog;
                                show_restart_dialog(exe_path, arc, lang);
                            },
                            Err(e) => {
                                *arc.lock().unwrap() = UpdateState::Failed(format!("{}", e));
                            },
                        },
                    );
                }
                if ui.button(l.t("跳过", "Skip")).clicked() {
                    *update_state.lock().unwrap() = UpdateState::None;
                }
            });
        },
        UpdateState::Downloading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(l.t("正在下载更新...", "Downloading update..."));
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        },
        UpdateState::ShowingDialog => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new(l.t("更新已就绪...", "Update ready..."))
                        .color(egui::Color32::from_rgb(100, 255, 100)),
                );
            });
        },
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
        },
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
                    *update_state.lock().unwrap() = UpdateState::None;
                }
            });
        },
        _ => {},
    });
}

/// Spawn a background update check.  Returns immediately.
pub fn spawn_check(asset_name: &'static str, update_state: &Arc<Mutex<UpdateState>>) {
    let state = update_state.clone();
    std::thread::spawn(
        move || match genshin_scanner::updater::check_for_update(asset_name) {
            Ok(genshin_scanner::updater::UpdateStatus::UpdateAvailable {
                latest_version,
                download_url,
                ..
            }) => {
                *state.lock().unwrap() = UpdateState::Available {
                    latest_version,
                    download_url,
                };
            },
            Ok(_) => {
                *state.lock().unwrap() = UpdateState::None;
            },
            Err(e) => {
                yas::log_debug!("更新检查失败: {}", "Update check failed: {}", e);
                *state.lock().unwrap() = UpdateState::None;
            },
        },
    );
}

/// Show a native OS dialog asking the user to restart now or later.
fn show_restart_dialog(exe_path: PathBuf, update_state: Arc<Mutex<UpdateState>>, lang: Lang) {
    let (title, description) = match lang {
        Lang::Zh => ("更新完成", "更新已下载完成。是否立即重启？"),
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
            yas::log_info!("用户选择立即重启", "User chose to restart now");
            match std::process::Command::new(&exe_path).spawn() {
                Ok(_) => std::process::exit(0),
                Err(e) => {
                    yas::log_error!("启动新版本失败: {}", "Failed to launch new version: {}", e);
                    *update_state.lock().unwrap() = UpdateState::Ready;
                },
            }
        },
        _ => {
            yas::log_info!("用户选择稍后重启", "User chose to restart later");
            *update_state.lock().unwrap() = UpdateState::Ready;
        },
    }
}

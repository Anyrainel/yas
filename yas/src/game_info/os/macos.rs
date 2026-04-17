use crate::{common::utils::*, core::ui::Resolution};
use crate::game_info::{GameInfo, Platform};

pub fn get_game_info() -> GameInfo {
    let (pid, ui) = get_pid_and_ui();

    let (rect, window_title) = unsafe { find_window_by_pid(pid).unwrap() };

    log_info!("找到游戏窗口：{} (PID: {})", "Found game window: {} (PID: {})", window_title, pid);

    GameInfo {
        window: rect,
        is_cloud: false,
        ui,
        platform: Platform::MacOS
    }
}

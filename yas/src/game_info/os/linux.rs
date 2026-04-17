use anyhow::{Result, anyhow};

use crate::game_info::{GameInfo, Platform, UI, is_16x9};
use crate::positioning::Rect;

pub fn get_game_info() -> Result<GameInfo> {
    let window_id = String::from_utf8(
            std::process::Command::new("sh")
                .arg("-c")
                .arg(r#" xwininfo|grep "Window id"|cut -d " " -f 4 "#)
                .output()
                .unwrap()
                .stdout,
        )?;
    let window_id = window_id.trim_end_matches("\n");

    let position_size = String::from_utf8(
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&format!(r#" xwininfo -id {window_id}|cut -f 2 -d :|tr -cd "0-9\n"|grep -v "^$"|sed -n "1,2p;5,6p" "#))
                .output()
                .unwrap()
                .stdout,
        )?;

    let mut info = position_size.split("\n");

    let left = info.next().unwrap().parse().unwrap();
    let top = info.next().unwrap().parse().unwrap();
    let width = info.next().unwrap().parse().unwrap();
    let height = info.next().unwrap().parse().unwrap();

    let rect = Rect::new(left, top, width, height);

    if !is_16x9(rect.size()) {
        log_error!(
            "游戏窗口内部区域为 {}x{}，不是16:9比例。本工具仅支持16:9分辨率（如1920×1080、2560×1440、3840×2160）。\n\
             请在游戏设置中切换到16:9分辨率后重试。",
            "Game window client area is {}x{}, which is not 16:9. This tool only supports 16:9 aspect ratio \
             (e.g. 1920×1080, 2560×1440, 3840×2160).\n\
             Please switch to a 16:9 resolution in game settings and try again.",
            width, height,
        );
        return Err(anyhow!(
            "不支持的分辨率: {}x{}（内部区域）。请使用16:9分辨率（如1920×1080、2560×1440、3840×2160）。\n\
             / Unsupported resolution: {}x{} (client area). Only 16:9 is supported (e.g. 1920×1080, 2560×1440, 3840×2160).",
            width, height, width, height
        ));
    }

    Ok(GameInfo {
        window: rect.to_rect_i32(),
        is_cloud: false,
        ui: UI::Desktop,
        platform: Platform::Linux,
    })
}

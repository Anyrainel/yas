use std::io::stdin;
use crate::game_info::{GameInfo, ResolutionFamily, UI, Platform};
use crate::utils;
use anyhow::{Result, anyhow};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn is_window_cloud(title: &str) -> bool {
    title.starts_with("云")
}

/// Get the window class name (e.g. "UnityWndClass") for a given HWND.
fn get_window_class(hwnd: HWND) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    let mut buf: [u16; 256] = [0; 256];
    let len = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), 256) };
    if len > 0 {
        let s = std::ffi::OsString::from_wide(&buf[..len as usize]);
        s.into_string().ok()
    } else {
        None
    }
}

/// Known window classes for the actual game process.
/// The launcher shares the same title but uses a different class.
const GAME_WINDOW_CLASSES: &[&str] = &[
    "UnityWndClass",         // local Genshin Impact / 原神
    "Qt5152QWindowIcon",     // cloud 云·原神 (Qt-based)
];

fn get_window(window_names: &[&str]) -> Result<(HWND, bool)> {
    let handles = utils::iterate_window();
    let mut viable_handles: Vec<(HWND, String, String)> = Vec::new(); // (hwnd, title, class)
    for hwnd in handles.iter() {
        let title = utils::get_window_title(*hwnd);
        if let Some(t) = title {
            let trimmed = t.trim();

            for name in window_names.iter() {
                if trimmed == *name {
                    let class = get_window_class(*hwnd).unwrap_or_default();
                    viable_handles.push((*hwnd, String::from(trimmed), class));
                }
            }
        }
    }

    if viable_handles.is_empty() {
        return Err(anyhow!(
            "未找到游戏窗口，请确认原神已启动且未最小化。\n\
             如果游戏已运行，请检查是否被其他程序（如HoYoPlay启动器）遮挡。\n\
             / Game window not found. Please make sure Genshin Impact is running and not minimized.\n\
             If the game is running, check that it is not hidden behind the HoYoPlay launcher."
        ));
    }

    // Log all matches for diagnostics (helps debug launcher interference).
    for (hwnd, title, class) in &viable_handles {
        log_debug!(
            "匹配到窗口: title={:?}, class={:?}, hwnd={:?}",
            "Matched window: title={:?}, class={:?}, hwnd={:?}",
            title, class, hwnd,
        );
    }

    // Filter by known game window classes to exclude non-game windows
    // (e.g. the HoYoPlay launcher creates a window titled "原神" too).
    let game_only: Vec<_> = viable_handles.iter()
        .filter(|(_, _, class)| GAME_WINDOW_CLASSES.iter().any(|&known| class == known))
        .collect();

    if game_only.len() == 1 {
        let (hwnd, title, _) = game_only[0];
        return Ok((*hwnd, is_window_cloud(title)));
    }

    // Class filter found 0 or >1 — fall back to title-only list.
    if game_only.is_empty() && viable_handles.len() >= 1 {
        log_warn!(
            "标题匹配到 {} 个窗口但无已知游戏窗口类（可能是HoYoPlay启动器），将使用第一个",
            "{} windows matched by title but none have a known game class \
             (could be HoYoPlay launcher); using first match",
            viable_handles.len(),
        );
        for (hwnd, title, class) in &viable_handles {
            log_warn!("  窗口: title={:?}, class={:?}, hwnd={:?}", "  window: title={:?}, class={:?}, hwnd={:?}", title, class, hwnd);
        }
    }

    let candidates: Vec<_> = if game_only.is_empty() {
        viable_handles.iter().collect()
    } else {
        game_only
    };

    if candidates.len() == 1 {
        let (hwnd, title, _) = candidates[0];
        return Ok((*hwnd, is_window_cloud(title)));
    }

    // Still ambiguous — interactive selection (CLI) or first match (GUI).
    println!("{}", crate::lang::localize("找到多个符合名称的窗口，请手动选择窗口 / Multiple matching windows found, please select one:"));
    for (i, (_hwnd, title, class)) in candidates.iter().enumerate() {
        println!("{}: {} [{}]", i, title, class);
    }
    let mut index = String::new();
    let idx = match stdin().read_line(&mut index) {
        Ok(_) => index.trim().parse::<usize>().unwrap_or(0),
        Err(_) => 0,
    };
    let idx = idx.min(candidates.len() - 1);
    let (hwnd, title, _) = candidates[idx];
    Ok((*hwnd, is_window_cloud(title)))
}

pub fn get_game_info(window_names: &[&str]) -> Result<GameInfo> {
    utils::set_dpi_awareness();

    let (hwnd, is_cloud) = get_window(window_names)?;

    // Only restore if minimized — do NOT steal focus here.
    // Focus is handled later by GenshinGameController::focus_game_window()
    // after the user confirms they are ready.
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
            utils::sleep(500);
        }
    }

    let rect = utils::get_client_rect(hwnd)?;
    let resolution_family = ResolutionFamily::new(rect.to_rect_usize().size());
    if resolution_family.is_none() {
        return Err(anyhow!(
            "不支持的分辨率: {}x{}。请使用16:9分辨率（如1920×1080��2560×1440、3840×2160）。\n\
             / Unsupported resolution: {}x{}. Use a 16:9 resolution (e.g. 1920×1080, 2560×1440, 3840×2160).",
            rect.width, rect.height, rect.width, rect.height
        ));
    }

    Ok(GameInfo {
        window: rect,
        resolution_family: resolution_family.unwrap(),
        is_cloud,
        ui: UI::Desktop,
        platform: Platform::Windows
    })
}

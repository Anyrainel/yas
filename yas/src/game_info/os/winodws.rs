use std::io::stdin;
use crate::game_info::{GameInfo, ResolutionFamily, UI, Platform};
use crate::utils;
use anyhow::{Result, anyhow};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn is_window_cloud(title: &str) -> bool {
    title.starts_with("云")
}

fn get_window(window_names: &[&str]) -> Result<(HWND, bool)> {
    let handles = utils::iterate_window();
    let mut viable_handles = Vec::new();
    for hwnd in handles.iter() {
        let title = utils::get_window_title(*hwnd);
        if let Some(t) = title {
            let trimmed = t.trim();

            for name in window_names.iter() {
                if trimmed == *name {
                    // return Ok((*hwnd, false));
                    viable_handles.push((*hwnd, String::from(trimmed)));
                }
            }
        }
    }

    // cloud games
    // let cloud_game_names = [""]
    // for name in get_cloud_window_name() {
    //     let hwnd = utils::find_window_local(name);
    //     if let Ok(hwnd) = hwnd {
    //         return (hwnd, true);
    //     }
    // }

    if viable_handles.len() == 1 {
        return Ok((viable_handles[0].0, is_window_cloud(&viable_handles[0].1)));
    } else if viable_handles.len() == 0 {
        return Err(anyhow!("未找到游戏窗口，请确认{:?}已经开启 / Game window not found, please make sure {:?} is running", window_names, window_names));
    }

    println!("{}", crate::lang::localize("找到多个符合名称的窗口，请手动选择窗口 / Multiple matching windows found, please select one:"));
    for (i, (_hwnd, title)) in viable_handles.iter().enumerate() {
        println!("{}: {}", i, title);
    }
    let mut index = String::new();
    let _ = stdin().read_line(&mut index);

    let idx = index.trim().parse::<usize>()?;
    if idx < viable_handles.len() {
        let is_cloud = is_window_cloud(&viable_handles[idx].1);
        Ok((viable_handles[idx].0, is_cloud))
    } else {
        Err(anyhow!("索引{}超出范围 / Index {} out of range", idx, idx))
    }
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
        return Err(anyhow!("不支持的分辨率 / Resolution not supported: {}x{}", rect.width, rect.height));
    }

    Ok(GameInfo {
        window: rect,
        resolution_family: resolution_family.unwrap(),
        is_cloud,
        ui: UI::Desktop,
        platform: Platform::Windows
    })
}

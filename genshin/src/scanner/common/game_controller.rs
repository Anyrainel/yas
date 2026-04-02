use std::rc::Rc;
use std::time::SystemTime;

use anyhow::{anyhow, Result};
use image::RgbImage;
use log::debug;

use yas::cancel::CancelToken;
use yas::capture::{Capturer, GenericCapturer};
use yas::game_info::GameInfo;
use yas::ocr::ImageToText;
use yas::positioning::Rect;
use yas::system_control::SystemControl;
use yas::utils;

use super::coord_scaler::CoordScaler;

/// Shared game controller for all Genshin scanners.
///
/// Consolidates game_info, scaler, capturer, and system_control into a single
/// struct with high-level methods for navigation, capture, OCR, and
/// YAS-style panel-load detection.
///
/// All coordinate parameters use the 1920x1080 base resolution and are
/// automatically scaled by the internal `CoordScaler`.
pub struct GenshinGameController {
    pub game_info: GameInfo,
    pub scaler: CoordScaler,
    pub capturer: Rc<dyn Capturer<RgbImage>>,
    pub system_control: SystemControl,

    /// Running pixel pool value for panel-load detection.
    pool: f64,

    /// Per-run cancellation token.
    cancel: CancelToken,
}

/// Compute pixel pool: sum of red channel values.
/// Port of `calc_pool` from yas scanner_controller/repository_layout/controller.rs.
fn calc_pool(raw: &[u8]) -> f64 {
    let len = raw.len() / 3;
    let mut pool: f64 = 0.0;
    for i in 0..len {
        pool += raw[i * 3] as f64;
    }
    pool
}

/// Squared Euclidean color distance between two RGB pixels.
pub fn color_distance(c1: &image::Rgb<u8>, c2: &image::Rgb<u8>) -> usize {
    let r = c1.0[0] as i32 - c2.0[0] as i32;
    let g = c1.0[1] as i32 - c2.0[1] as i32;
    let b = c1.0[2] as i32 - c2.0[2] as i32;
    (r * r + g * g + b * b) as usize
}

impl GenshinGameController {
    pub fn new(game_info: GameInfo) -> Result<Self> {
        let window_size = game_info.window.to_rect_usize().size();
        let scaler = CoordScaler::new(window_size.width as u32, window_size.height as u32);

        Ok(Self {
            game_info,
            scaler,
            capturer: Rc::new(GenericCapturer::new()?),
            system_control: SystemControl::new(),
            pool: 0.0,
            cancel: CancelToken::new(),
        })
    }
}

// Cancellation methods.
impl GenshinGameController {
    /// Set the cancellation token for this run.
    pub fn set_cancel_token(&mut self, token: CancelToken) {
        self.cancel = token;
    }

    /// Get a clone of the current cancellation token.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Check if the current run has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Check RMB and cancel if pressed. Returns true if cancelled.
    pub fn check_rmb(&self) -> bool {
        self.cancel.check_rmb()
    }
}

// Focus methods.
impl GenshinGameController {
    /// Focus the game window using Win32 SetForegroundWindow.
    /// Ensures subsequent keyboard events go to Genshin, not the terminal.
    pub fn focus_game_window(&mut self) {
        #[cfg(target_os = "windows")]
        {
            // Re-find the window handle and bring it to front
            let window_names = ["\u{539F}\u{795E}", "Genshin Impact"]; // 原神
            let handles = utils::iterate_window();
            for hwnd in &handles {
                if let Some(title) = utils::get_window_title(*hwnd) {
                    let trimmed = title.trim();
                    if window_names.iter().any(|n| trimmed == *n) {
                        utils::show_window_and_set_foreground(*hwnd);
                        utils::sleep(500);
                        return;
                    }
                }
            }
        }
        // Fallback: just move mouse to game area
        let center_x = self.game_info.window.left + self.game_info.window.width / 2;
        let center_y = self.game_info.window.top + self.game_info.window.height / 2;
        self.system_control.mouse_move_to(center_x, center_y).unwrap();
        utils::sleep(300);
    }
}

// Return to main UI — adapted from BetterGenshinImpact's ReturnMainUiTask.
// Press Escape one at a time, verify after each press, loop up to 8 times.
impl GenshinGameController {
    /// Check if the game appears to be in the main world (HUD visible, no menu open).
    ///
    /// Detects the Paimon icon button in the top-left corner. In the main world,
    /// this area contains the bright Paimon icon. In any menu, it's covered by
    /// the menu's dark background or header.
    ///
    /// Uses pixel brightness sampling — not as robust as template matching but
    /// sufficient for the return-to-main-UI loop.
    pub fn is_likely_main_world(&self) -> bool {
        let image = match self.capture_game() {
            Ok(img) => img,
            Err(_) => return false,
        };

        // The Paimon icon at 1920x1080 is a bright white/cream circular button
        // centered around (58, 50) with radius ~25px.
        // Sample several points across the icon face area.
        let check_points: &[(f64, f64)] = &[
            (62.0, 51.0),  // Center of icon face
            (53.0, 47.0),  // Inner-left
            (49.0, 35.0),  // Upper portion
            (55.0, 70.0),  // Lower portion
            (67.0, 77.0),  // Lower-right
        ];

        let mut bright_count = 0;
        for &(bx, by) in check_points {
            let x = self.scaler.x(bx) as u32;
            let y = self.scaler.y(by) as u32;
            if x < image.width() && y < image.height() {
                let p = image.get_pixel(x, y);
                let brightness = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
                if brightness > 160 {
                    bright_count += 1;
                }
            }
        }

        bright_count >= 3
    }

    /// Return to the main world UI by pressing Escape one at a time and verifying.
    ///
    /// Adapted from BetterGenshinImpact's ReturnMainUiTask strategy:
    /// 1. Check if already in main UI — if so, return immediately.
    /// 2. Loop up to `max_attempts` times: press Escape, wait, check.
    /// 3. Final fallback: press Enter (dismiss dialogs) then Escape.
    ///
    /// Returns true if main UI was detected, false if still uncertain.
    pub fn return_to_main_ui(&mut self, max_attempts: u32) -> bool {
        if self.is_likely_main_world() {
            debug!("[return_to_main_ui] 已在主界面 / [return_to_main_ui] already in main world");
            return true;
        }

        for i in 0..max_attempts {
            if self.check_rmb() {
                return false;
            }
            self.key_press(enigo::Key::Escape);
            utils::sleep(900);

            if self.is_likely_main_world() {
                debug!("[return_to_main_ui] 按{}次Escape后到达主界面 / [return_to_main_ui] reached main world after {} Escape(s)", i + 1, i + 1);
                return true;
            }
        }

        if self.check_rmb() {
            return false;
        }

        // Fallback: Enter (dismiss any stuck dialog) + Escape
        debug!("[return_to_main_ui] 回退策略: Enter + Escape / [return_to_main_ui] fallback: Enter + Escape");
        self.key_press(enigo::Key::Return);
        utils::sleep(500);
        self.key_press(enigo::Key::Escape);
        utils::sleep(900);

        let result = self.is_likely_main_world();
        if result {
            debug!("[return_to_main_ui] 回退后到达主界面 / [return_to_main_ui] reached main world after fallback");
        } else {
            log::warn!("[return_to_main_ui] 尝试{}次+回退后可能仍未在主界面 / [return_to_main_ui] may not be in main world after {} attempts + fallback", max_attempts, max_attempts);
        }
        result
    }
}

// Navigation methods — all coordinates at 1920x1080 base, scaled by CoordScaler.
impl GenshinGameController {
    /// Click at a position specified in base 1920x1080 coordinates.
    pub fn click_at(&mut self, base_x: f64, base_y: f64) {
        let x = self.game_info.window.left as f64 + self.scaler.scale_x(base_x);
        let y = self.game_info.window.top as f64 + self.scaler.scale_y(base_y);
        log::debug!("[click_at] base=({:.0},{:.0}) -> screen=({},{}), window=({},{})",
            base_x, base_y, x as i32, y as i32,
            self.game_info.window.left, self.game_info.window.top);
        self.system_control.mouse_move_to(x as i32, y as i32).unwrap();
        self.system_control.mouse_click().unwrap();
    }

    /// Move mouse to a position specified in base 1920x1080 coordinates.
    pub fn move_to(&mut self, base_x: f64, base_y: f64) {
        let x = self.game_info.window.left as f64 + self.scaler.scale_x(base_x);
        let y = self.game_info.window.top as f64 + self.scaler.scale_y(base_y);
        self.system_control.mouse_move_to(x as i32, y as i32).unwrap();
    }

    /// Press a keyboard key.
    pub fn key_press(&mut self, key: enigo::Key) {
        self.system_control.key_press(key).unwrap();
    }

    /// Scroll the mouse wheel.
    pub fn mouse_scroll(&mut self, amount: i32) {
        self.system_control.mouse_scroll(amount, false).unwrap();
    }
}

// Capture and OCR methods.
impl GenshinGameController {
    /// Capture the full game window.
    pub fn capture_game(&self) -> Result<RgbImage> {
        self.capturer.capture_rect(self.game_info.window)
    }

    /// Capture a sub-region of the game window.
    /// Coordinates are in base 1920x1080 and will be scaled.
    pub fn capture_region(
        &self,
        base_x: f64,
        base_y: f64,
        base_w: f64,
        base_h: f64,
    ) -> Result<RgbImage> {
        let rect = Rect {
            left: self.scaler.scale_x(base_x) as i32,
            top: self.scaler.scale_y(base_y) as i32,
            width: self.scaler.scale_x(base_w) as i32,
            height: self.scaler.scale_y(base_h) as i32,
        };
        self.capturer
            .capture_relative_to(rect, self.game_info.window.origin())
    }

    /// OCR a region and return trimmed text.
    /// Coordinates are in base 1920x1080 and will be scaled.
    pub fn ocr_region(
        &self,
        ocr_model: &dyn ImageToText<RgbImage>,
        rect: (f64, f64, f64, f64),
    ) -> Result<String> {
        let (x, y, w, h) = rect;
        let im = self.capture_region(x, y, w, h)?;
        let text = ocr_model.image_to_text(&im, false)?;
        Ok(text.trim().to_string())
    }

    /// OCR a region with Y-offset support (for elixir artifacts, etc).
    pub fn ocr_region_shifted(
        &self,
        ocr_model: &dyn ImageToText<RgbImage>,
        rect: (f64, f64, f64, f64),
        y_shift: f64,
    ) -> Result<String> {
        let (x, y, w, h) = rect;
        self.ocr_region(ocr_model, (x, y + y_shift, w, h))
    }
}

// Screenshot save helpers.
impl GenshinGameController {
    /// Save the full game window as a PNG file.
    pub fn save_screenshot(&self, path: &str) -> Result<()> {
        let im = self.capture_game()?;
        im.save(path).map_err(|e| anyhow!("截图保存失败 / Failed to save screenshot: {}", e))?;
        debug!("[screenshot] 已保存完整截图: {} / [screenshot] saved full: {}", path, path);
        Ok(())
    }

    /// Save a sub-region of the game window as a PNG file.
    /// Coordinates are in base 1920x1080 and will be scaled.
    pub fn save_region_screenshot(
        &self,
        path: &str,
        base_x: f64,
        base_y: f64,
        base_w: f64,
        base_h: f64,
    ) -> Result<()> {
        let im = self.capture_region(base_x, base_y, base_w, base_h)?;
        im.save(path).map_err(|e| anyhow!("截图保存失败 / Failed to save screenshot: {}", e))?;
        debug!("[screenshot] 已保存区域截图({},{},{},{}) -> {} / [screenshot] saved region ({},{},{},{}) -> {}", base_x, base_y, base_w, base_h, path, base_x, base_y, base_w, base_h, path);
        Ok(())
    }
}

// Panel-load detection — ported from YAS controller.rs:wait_until_switched.
impl GenshinGameController {
    /// Wait until the detail panel has finished loading a new item.
    ///
    /// Monitors a "pool rect" region: captures the rect, computes the sum of
    /// red channel pixel values ("pixel pool"). When the pool changes
    /// (a different item's panel is rendering) then stabilizes (rendering
    /// complete), the method returns.
    ///
    /// This replaces fixed-delay waits with reactive detection — typically
    /// faster and more reliable.
    ///
    /// Port of `wait_until_switched` from YAS controller.rs:355-390.
    ///
    /// `pool_rect` is in base 1920x1080 coordinates.
    /// `timeout_ms` is the maximum wait time in milliseconds.
    pub fn wait_until_panel_loaded(
        &mut self,
        pool_rect: (f64, f64, f64, f64),
        timeout_ms: u64,
    ) -> Result<()> {
        if self.game_info.is_cloud {
            // Cloud games have variable latency; use a conservative fixed wait
            utils::sleep(300);
            return Ok(());
        }

        let now = SystemTime::now();
        let (px, py, pw, ph) = pool_rect;
        let rect = Rect {
            left: self.scaler.scale_x(px) as i32,
            top: self.scaler.scale_y(py) as i32,
            width: self.scaler.scale_x(pw) as i32,
            height: self.scaler.scale_y(ph) as i32,
        };

        let mut consecutive_stable = 0;
        let mut change_detected = false;
        let mut no_change_count = 0;

        while now.elapsed().unwrap().as_millis() < timeout_ms as u128 {
            let im = self
                .capturer
                .capture_relative_to(rect, self.game_info.window.origin())?;

            let pool = calc_pool(im.as_raw());

            if (pool - self.pool).abs() > 0.000001 {
                // Pool changed — panel is transitioning
                self.pool = pool;
                change_detected = true;
                consecutive_stable = 0;
                no_change_count = 0;
            } else if change_detected {
                // Pool stabilized after a change — panel is ready
                consecutive_stable += 1;
                if consecutive_stable >= 1 {
                    return Ok(());
                }
            } else {
                // No change at all — same item type or panel already loaded.
                // After a few checks with no change, assume panel is ready
                // (avoids 800ms timeout on duplicate items).
                no_change_count += 1;
                if no_change_count >= 2 {
                    return Ok(());
                }
            }
        }

        // Timeout — proceed anyway (better than hanging)
        debug!("[controller] 面板加载检测超时({}ms) / [controller] panel load detection timed out after {}ms", timeout_ms, timeout_ms);
        Ok(())
    }

    /// Capture the color of a single pixel at base 1920x1080 coordinates.
    /// Used for scroll flag detection.
    pub fn get_flag_color(&self, flag_x: f64, flag_y: f64) -> Result<image::Rgb<u8>> {
        let pos = yas::positioning::Pos {
            x: self.game_info.window.left + self.scaler.scale_x(flag_x) as i32,
            y: self.game_info.window.top + self.scaler.scale_y(flag_y) as i32,
        };
        self.capturer.capture_color(pos)
    }
}

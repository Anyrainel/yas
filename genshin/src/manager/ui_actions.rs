//! In-game UI interaction functions for the artifact manager.
//!
//! All coordinates are at 1920x1080 base resolution and are automatically scaled
//! by `GenshinGameController` via `CoordScaler`.

use anyhow::{bail, Result};
use image::RgbImage;
use log::{debug, info};

use yas::ocr::ImageToText;

use crate::scanner::common::constants::*;
use crate::scanner::common::coord_scaler::CoordScaler;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::ocr_factory;

use super::models::ArtifactTarget;

// ================================================================
// Calibrated coordinates for artifact manager UI interactions
// ================================================================

/// Lock icon clickable center (artifact detail panel in backpack view).
/// Calibrated from the dark rounded-rect background behind the lock icon.
const LOCK_BUTTON_X: f64 = 1696.0;
const LOCK_BUTTON_Y: f64 = 432.0;

/// "圣遗物" menu item on the character detail screen left sidebar.
const CHAR_ARTIFACT_MENU_X: f64 = 160.0;
const CHAR_ARTIFACT_MENU_Y: f64 = 293.0;

/// "替换" button on the character artifact circle view (bottom-right).
/// Clicking this opens the artifact selection list for the current slot.
const CHAR_REPLACE_BUTTON_X: f64 = 1720.0;
const CHAR_REPLACE_BUTTON_Y: f64 = 1010.0;

/// Slot type tab positions in the artifact selection view (top bar).
/// Calibrated from pixel analysis of the selection view screenshot.
const SEL_TAB_FLOWER: (f64, f64) = (80.0, 30.0);
const SEL_TAB_PLUME: (f64, f64) = (140.0, 30.0);
const SEL_TAB_SANDS: (f64, f64) = (215.0, 30.0);
const SEL_TAB_GOBLET: (f64, f64) = (310.0, 30.0);
const SEL_TAB_CIRCLET: (f64, f64) = (420.0, 30.0);

/// Selection grid layout (artifact selection list when clicking a character slot).
/// 4 columns, rows scroll vertically. Calibrated from selection view screenshot.
const SEL_COLS: usize = 4;
const SEL_ROWS: usize = 4; // visible rows per page
const SEL_FIRST_X: f64 = 89.0;
const SEL_FIRST_Y: f64 = 130.0;
const SEL_OFFSET_X: f64 = 141.0;
const SEL_OFFSET_Y: f64 = 167.0;

/// Scroll ticks per page in the selection grid.
const SEL_SCROLL_TICKS: i32 = 40;

/// "卸下"/"替换" button in the artifact selection view (bottom-right).
/// Same position for both: "卸下" when equipped item selected, "替换" otherwise.
/// Calibrated: button bg spans x=1467-1650, center at x≈1558, y≈1025.
const SEL_ACTION_BUTTON_X: f64 = 1558.0;
const SEL_ACTION_BUTTON_Y: f64 = 1025.0;

/// Selection view detail panel: level OCR region (single line).
/// "+20" badge text at approximately x=1460-1520, y=308-338.
/// Calibrated from pixel scan of the selection view screenshot.
const SEL_LEVEL_RECT: (f64, f64, f64, f64) = (1455.0, 305.0, 80.0, 35.0);

/// Selection view: first substat line (single line, for matching).
/// First substat is always at a fixed Y position regardless of substat count.
/// Calibrated from selection view screenshot: y=348-380.
const SEL_SUBSTAT1_RECT: (f64, f64, f64, f64) = (1280.0, 348.0, 350.0, 35.0);

// ================================================================
// Set filter panel coordinates (opens from selection view bottom bar)
// ================================================================

/// "套装筛选" bar center at bottom-left of selection view.
/// The bar spans x=33-549, y=983-1032. Clicking the text area opens the
/// set filter panel overlay. The dark funnel icon is at ~(98, 1008) but
/// clicking the text center is more reliable.
const FILTER_FUNNEL_X: f64 = 309.0;
const FILTER_FUNNEL_Y: f64 = 1008.0;

/// "清空条件" (Clear conditions) button at bottom-left of filter panel.
const FILTER_CLEAR_X: f64 = 120.0;
const FILTER_CLEAR_Y: f64 = 1019.0;

/// "确认筛选" (Confirm filter) button at bottom-right of filter panel.
const FILTER_CONFIRM_X: f64 = 1727.0;
const FILTER_CONFIRM_Y: f64 = 1019.0;

/// Close (X) button at top-right of filter panel.
const FILTER_CLOSE_X: f64 = 1841.0;
const FILTER_CLOSE_Y: f64 = 48.0;

/// First data row Y center in the set filter panel.
const FILTER_FIRST_ROW_Y: f64 = 236.5;
/// Vertical spacing between consecutive row centers.
const FILTER_ROW_SPACING: f64 = 81.5;
/// Number of visible rows per column before scrolling.
const FILTER_VISIBLE_ROWS: usize = 9;

/// OCR text region for set names in the left column.
/// The left column layout is: [radio] [icon] [set name] [count].
/// The icon ends around x=145-155. Starting at x=175 to avoid icon bleed.
/// Calibrated from pixel analysis and OCR testing.
const FILTER_LEFT_TEXT_X: f64 = 175.0;
const FILTER_LEFT_TEXT_W: f64 = 260.0;
/// OCR text region for set names in the right column.
/// The right column layout is: [count] [radio] [icon] [text].
/// The icon ends around x=795, text starts at ~x=800-815.
/// Calibrated from pixel analysis of filter panel screenshot.
const FILTER_RIGHT_TEXT_X: f64 = 800.0;
const FILTER_RIGHT_TEXT_W: f64 = 170.0;
/// Text region height for set name OCR.
const FILTER_TEXT_H: f64 = 35.0;

/// Click target X for selecting a set in the left column.
const FILTER_LEFT_CLICK_X: f64 = 260.0;
/// Click target X for selecting a set in the right column.
const FILTER_RIGHT_CLICK_X: f64 = 720.0;

/// Mouse position for scrolling in the filter panel (center of set list).
const FILTER_SCROLL_X: f64 = 300.0;
const FILTER_SCROLL_Y: f64 = 500.0;
/// Scroll ticks per page in the filter set list.
const FILTER_SCROLL_TICKS: i32 = 20;

// ================================================================
// Implementation
// ================================================================

/// Click the lock/unlock button on the artifact detail panel.
///
/// The lock icon is on the artifact detail panel (right side of backpack).
/// Detection pixels are at (1683,428) and (1708,428); the clickable center
/// of the dark rounded-rect background is at (1696, 432).
///
/// For elixir artifacts, the lock icon shifts down by `y_shift` (40px).
pub fn click_lock_button(ctrl: &mut GenshinGameController, y_shift: f64) -> Result<()> {
    ctrl.click_at(LOCK_BUTTON_X, LOCK_BUTTON_Y + y_shift);
    yas::utils::sleep(300); // wait for lock animation
    Ok(())
}

/// Open a specific character's detail screen from the main world.
///
/// Presses C to open the character roster, then cycles through characters
/// using the "next" button while OCR-ing the character name until the target
/// is found.
pub fn open_character_screen(
    ctrl: &mut GenshinGameController,
    char_key: &str,
    mappings: &MappingManager,
) -> Result<()> {
    // Reverse-lookup: GOOD key -> Chinese name(s) for matching
    let cn_names: Vec<String> = mappings
        .character_name_map
        .iter()
        .filter(|(_, v)| v.as_str() == char_key)
        .map(|(k, _)| k.clone())
        .collect();

    let ocr = ocr_factory::create_ocr_model("ppocrv4")?;

    // Ensure we're in main world first.
    // Use return_to_main_ui with generous attempts, then verify by
    // trying to open character screen and reading the name OCR.
    ctrl.return_to_main_ui(8);
    yas::utils::sleep(500);

    // Press C to open character roster. Try up to 3 times:
    // if OCR reads nothing, we might not be on the character screen.
    ctrl.focus_game_window();
    let mut char_screen_opened = false;
    for attempt in 0..3 {
        ctrl.key_press(enigo::Key::Layout('c'));
        yas::utils::sleep(1500);

        // Try OCR at the character name region to verify we're on the character screen.
        // The name should contain CJK characters (e.g., "水元素/芙宁娜").
        // If OCR reads only numbers/ASCII, we're not on the character screen.
        let name_text = ctrl.ocr_region(ocr.as_ref(), CHAR_NAME_RECT)
            .unwrap_or_default();
        let has_cjk = name_text.chars().any(|c| c >= '\u{4e00}' && c <= '\u{9fff}');
        if has_cjk {
            info!("[open_character_screen] 角色界面已打开（第{}次尝试），名称='{}' / character screen opened (attempt {}), name='{}'",
                attempt, name_text.trim(), attempt, name_text.trim());
            char_screen_opened = true;
            break;
        }
        // Not on character screen — try Escape + retry
        info!("[open_character_screen] 第{}次尝试失败，按Escape重试 / attempt {} failed, pressing Escape and retrying", attempt, attempt);
        ctrl.key_press(enigo::Key::Escape);
        yas::utils::sleep(800);
    }
    if !char_screen_opened {
        bail!("无法打开角色界面 / Cannot open character screen after 3 attempts");
    }

    let max_chars = 150; // safety limit (must exceed account roster size)
    let mut first_name: Option<String> = None;

    for i in 0..max_chars {
        if yas::utils::is_rmb_down() {
            bail!("用户中断 / User aborted");
        }

        // OCR character name
        let name_text = ctrl.ocr_region(ocr.as_ref(), CHAR_NAME_RECT)?;
        let name_trimmed = name_text.trim().to_string();
        debug!("[open_character_screen] #{}: OCR识别名称='{}' / OCR name = '{}'", i, name_trimmed, name_trimmed);

        // Check for full cycle (returned to first character).
        // Use cleaned names (stripped of trailing garbage) for robust comparison.
        if i > 0 {
            if let Some(ref first) = first_name {
                let cur_name = clean_char_name(&name_trimmed);
                let first_char_name = clean_char_name(first);
                debug!("[open_character_screen] #{}: 当前='{}' vs 首个='{}' / cur='{}' vs first='{}'", i, cur_name, first_char_name, cur_name, first_char_name);
                if !cur_name.is_empty() && cur_name == first_char_name {
                    bail!(
                        "角色 {} 未找到（已遍历全部角色）/ \
                         Character {} not found (cycled through all characters)",
                        char_key, char_key
                    );
                }
            }
        }
        if first_name.is_none() && !name_trimmed.is_empty() {
            first_name = Some(name_trimmed.clone());
        }

        // Extract and clean the character name part (strip element prefix + trailing garbage)
        let clean_name = clean_char_name(&name_trimmed);

        // Match against GOOD key directly (OCR might return English name)
        if name_trimmed.contains(char_key) {
            info!("[open_character_screen] 在位置{}找到{} / found {} at position {}", i, char_key, char_key, i);
            return Ok(());
        }

        // Match against Chinese name(s) — exact substring first, then fuzzy
        let mut found = false;
        for cn in &cn_names {
            if name_trimmed.contains(cn.as_str()) {
                info!("[open_character_screen] 在位置{}找到{}（中文: {}） / found {} (cn: {}) at position {}", i, char_key, cn, char_key, cn, i);
                return Ok(());
            }
            // Fuzzy match: allow 1 character difference for names >= 2 chars
            if fuzzy_char_match(&clean_name, cn) {
                info!("[open_character_screen] 在位置{}找到{}（中文: {}，模糊匹配'{}'） / found {} (cn: {}, fuzzy match '{}') at position {}",
                    i, char_key, cn, clean_name, char_key, cn, clean_name, i);
                found = true;
                break;
            }
        }
        if found {
            return Ok(());
        }

        // Try reverse: OCR text -> GOOD key via mapping (try cleaned name too)
        for try_name in &[&name_trimmed, &clean_name] {
            if let Some(matched_key) = mappings.character_name_map.get(try_name.as_str()) {
                if matched_key == char_key {
                    info!("[open_character_screen] 通过映射在位置{}找到{} / found {} via mapping at position {}", i, char_key, char_key, i);
                    return Ok(());
                }
            }
        }

        // Click next character
        ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
        yas::utils::sleep(400);
    }

    bail!(
        "角色 {} 未找到（达到最大遍历次数）/ \
         Character {} not found (max iterations reached)",
        char_key, char_key
    )
}

/// Click an artifact equipment slot on the character detail screen.
///
/// Navigates from the character detail screen to the artifact selection view
/// for the requested slot type:
/// 1. Clicks "圣遗物" in the left menu to show artifact circles
/// 2. Clicks "替换" button to open the selection list
/// 3. Clicks the appropriate slot tab (flower/plume/sands/goblet/circlet)
pub fn click_equipment_slot(
    ctrl: &mut GenshinGameController,
    slot_key: &str,
) -> Result<()> {
    let tab_pos = match slot_key {
        "flower" => SEL_TAB_FLOWER,
        "plume" => SEL_TAB_PLUME,
        "sands" => SEL_TAB_SANDS,
        "goblet" => SEL_TAB_GOBLET,
        "circlet" => SEL_TAB_CIRCLET,
        _ => bail!("未知栏位 / Unknown slot: {}", slot_key),
    };

    // Ensure focus before UI interactions
    ctrl.focus_game_window();
    yas::utils::sleep(200);

    // Step 1: Click 圣遗物 menu to show artifact circles
    debug!("[click_equipment_slot] 点击圣遗物菜单 / clicking artifact menu");
    ctrl.click_at(CHAR_ARTIFACT_MENU_X, CHAR_ARTIFACT_MENU_Y);
    yas::utils::sleep(1200); // wait for circle animation

    // Step 2: Click "替换" button to open the artifact selection list.
    // The button appears at the bottom-right of the character artifact view.
    debug!("[click_equipment_slot] 点击替换按钮({}, {}) / clicking replace button at ({}, {})", CHAR_REPLACE_BUTTON_X, CHAR_REPLACE_BUTTON_Y, CHAR_REPLACE_BUTTON_X, CHAR_REPLACE_BUTTON_Y);
    ctrl.click_at(CHAR_REPLACE_BUTTON_X, CHAR_REPLACE_BUTTON_Y);
    yas::utils::sleep(2000); // wait for selection view to load

    // Step 3: Click the correct slot tab
    debug!("[click_equipment_slot] 点击{}标签({}, {}) / clicking {} tab at ({}, {})", slot_key, tab_pos.0, tab_pos.1, slot_key, tab_pos.0, tab_pos.1);
    ctrl.click_at(tab_pos.0, tab_pos.1);
    yas::utils::sleep(800);

    Ok(())
}

/// Click the "卸下" (unequip) button to remove an artifact.
///
/// When the selection view opens with the currently equipped artifact selected
/// (always the first item), the "卸下" button appears at the bottom.
pub fn click_unequip_button(ctrl: &mut GenshinGameController) -> Result<()> {
    ctrl.click_at(SEL_ACTION_BUTTON_X, SEL_ACTION_BUTTON_Y);
    yas::utils::sleep(500);
    Ok(())
}

/// Apply a set filter in the artifact selection view to narrow the grid.
///
/// Opens the set filter panel (via the funnel icon), clears any existing filter,
/// finds the target set by OCR-ing set names in the two-column list, selects it,
/// and confirms.
///
/// The filter panel has a two-column scrollable list of artifact set names with
/// counts. Each row shows: [radio] [count] [icon] [set name]. We OCR each row's
/// text region and match against the Chinese name of the target set.
///
/// Returns `Ok(true)` if the set was found and filter applied, `Ok(false)` if not.
pub fn apply_set_filter(
    ctrl: &mut GenshinGameController,
    set_key: &str,
    mappings: &MappingManager,
    ocr: &dyn ImageToText<RgbImage>,
) -> Result<bool> {
    // Reverse lookup: GOOD key → Chinese name
    let cn_name = match mappings.artifact_set_map.iter()
        .find(|(_, v)| v.as_str() == set_key)
        .map(|(k, _)| k.clone())
    {
        Some(name) => name,
        None => {
            info!("[set_filter] 套装'{}'未在映射中找到，跳过筛选 / set key '{}' not found in mappings, skipping filter", set_key, set_key);
            return Ok(false);
        }
    };

    info!("[set_filter] 正在筛选{}（{}） / applying filter for {} ({})", set_key, cn_name, set_key, cn_name);

    // Open filter panel
    ctrl.click_at(FILTER_FUNNEL_X, FILTER_FUNNEL_Y);
    yas::utils::sleep(1500);

    // Clear existing filters
    ctrl.click_at(FILTER_CLEAR_X, FILTER_CLEAR_Y);
    yas::utils::sleep(500);

    // Scan rows to find and select the target set.
    // The filter panel uses clean golden text on dark background —
    // use direct OCR (no binarization, which is marginal for golden text
    // at brightness ~170 vs threshold 160).
    let max_scrolls = 2; // most sets fit on 1-2 pages
    for scroll in 0..=max_scrolls {
        for row in 0..FILTER_VISIBLE_ROWS {
            let y = FILTER_FIRST_ROW_Y + row as f64 * FILTER_ROW_SPACING;
            let text_y = y - FILTER_TEXT_H / 2.0;

            // Check left column
            let left_text = ctrl.ocr_region(
                ocr,
                (FILTER_LEFT_TEXT_X, text_y, FILTER_LEFT_TEXT_W, FILTER_TEXT_H),
            ).unwrap_or_default();

            if left_text.contains(&cn_name) {
                debug!("[set_filter] 在左列第{}行找到'{}' (OCR: '{}') / found '{}' in left col row {} (OCR: '{}')", row, cn_name, left_text, cn_name, row, left_text);
                ctrl.click_at(FILTER_LEFT_CLICK_X, y);
                yas::utils::sleep(300);
                ctrl.click_at(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
                yas::utils::sleep(800);
                info!("[set_filter] 已应用筛选: {} / filter applied for {}", set_key, set_key);
                return Ok(true);
            }

            // Check right column
            let right_text = ctrl.ocr_region(
                ocr,
                (FILTER_RIGHT_TEXT_X, text_y, FILTER_RIGHT_TEXT_W, FILTER_TEXT_H),
            ).unwrap_or_default();

            if right_text.contains(&cn_name) {
                debug!("[set_filter] 在右列第{}行找到'{}' (OCR: '{}') / found '{}' in right col row {} (OCR: '{}')", row, cn_name, right_text, cn_name, row, right_text);
                ctrl.click_at(FILTER_RIGHT_CLICK_X, y);
                yas::utils::sleep(300);
                ctrl.click_at(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
                yas::utils::sleep(800);
                info!("[set_filter] 已应用筛选: {} / filter applied for {}", set_key, set_key);
                return Ok(true);
            }
        }

        // Scroll down to see more sets
        if scroll < max_scrolls {
            ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
            yas::utils::sleep(50);
            ctrl.mouse_scroll(FILTER_SCROLL_TICKS);
            yas::utils::sleep(500);
        }
    }

    // Set not found — close filter panel without applying
    info!("[set_filter] 套装'{}'（{}）未在筛选列表中找到 / set '{}' ({}) not found in filter list", set_key, cn_name, set_key, cn_name);
    ctrl.click_at(FILTER_CLOSE_X, FILTER_CLOSE_Y);
    yas::utils::sleep(500);
    Ok(false)
}

/// Clear the set filter in the artifact selection view.
///
/// Opens the filter panel, clears all selections, and confirms.
pub fn clear_set_filter(ctrl: &mut GenshinGameController) -> Result<()> {
    ctrl.click_at(FILTER_FUNNEL_X, FILTER_FUNNEL_Y);
    yas::utils::sleep(1500);
    ctrl.click_at(FILTER_CLEAR_X, FILTER_CLEAR_Y);
    yas::utils::sleep(300);
    ctrl.click_at(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
    yas::utils::sleep(800);
    Ok(())
}

/// Find and click a target artifact in the artifact selection list.
///
/// After `click_equipment_slot` opens the selection view for the right slot type,
/// this function:
/// 1. Applies a set filter to narrow the grid to the target set
/// 2. Iterates through the filtered grid, matching each artifact by:
///    a. Level (single-line OCR of "+20" badge — fast filter)
///    b. First substat value (single-line OCR — matches against any target substat)
///
/// The slot type is already filtered by the tab selection. With the set filter,
/// only artifacts from the target set are shown, making the search much faster.
/// The combination of set + slot + level + any matching substat value is
/// almost always unique.
///
/// PaddleOCR is a single-line recognition model, so each field is OCR'd
/// from a narrow single-line crop with binarization (brightness threshold)
/// to handle the semi-transparent panel background over the character model.
///
/// Returns `Ok(true)` if found and equipped, `Ok(false)` if not found.
pub fn find_and_click_artifact_in_selection(
    ctrl: &mut GenshinGameController,
    target: &ArtifactTarget,
    ocr: &dyn ImageToText<RgbImage>,
    _scaler: &CoordScaler,
    mappings: &MappingManager,
) -> Result<bool> {
    // Apply set filter to narrow down the grid
    let filter_applied = apply_set_filter(ctrl, &target.set_key, mappings, ocr)?;
    if filter_applied {
        // Re-click slot tab after filter (filter panel open/close may reset the tab)
        let tab_pos = match target.slot_key.as_str() {
            "flower" => SEL_TAB_FLOWER,
            "plume" => SEL_TAB_PLUME,
            "sands" => SEL_TAB_SANDS,
            "goblet" => SEL_TAB_GOBLET,
            "circlet" => SEL_TAB_CIRCLET,
            _ => SEL_TAB_FLOWER,
        };
        ctrl.click_at(tab_pos.0, tab_pos.1);
        yas::utils::sleep(800);
        info!("[selection] 已应用套装筛选，重新点击栏位标签，扫描筛选后的列表 / set filter applied, re-clicked slot tab, scanning filtered grid");
    } else {
        info!("[selection] 未应用套装筛选，扫描完整列表 / set filter not applied, scanning full grid");
    }

    let max_pages = 20; // safety limit
    let mut total_checked = 0;
    let mut consecutive_empty = 0;

    info!("[selection] 开始网格扫描: set={}, lv={} / starting grid scan for set={}, lv={}", target.set_key, target.level, target.set_key, target.level);

    for page in 0..max_pages {
        for row in 0..SEL_ROWS {
            for col in 0..SEL_COLS {
                if yas::utils::is_rmb_down() {
                    bail!("用户中断 / User aborted");
                }

                let x = SEL_FIRST_X + col as f64 * SEL_OFFSET_X;
                let y = SEL_FIRST_Y + row as f64 * SEL_OFFSET_Y;

                // Click item to show detail panel
                ctrl.click_at(x, y);
                yas::utils::sleep(300);

                // Step 1: OCR level (single-line "+20" badge)
                let level_text = match ocr_region_enhanced(ctrl, ocr, SEL_LEVEL_RECT) {
                    Ok(t) => t,
                    Err(e) => {
                        debug!("[selection] 等级OCR失败({},{})：{} / level OCR failed at ({},{}): {}", row, col, e, row, col, e);
                        total_checked += 1;
                        continue;
                    }
                };

                let level = parse_level(&level_text);
                if level < 0 {
                    consecutive_empty += 1;
                    if consecutive_empty >= SEL_COLS {
                        info!("[selection] 连续{}个无法读取的栏位，停止扫描 / {} consecutive unreadable slots, stopping", consecutive_empty, consecutive_empty);
                        return Ok(false);
                    }
                    continue;
                }
                consecutive_empty = 0;
                total_checked += 1;

                debug!("[selection] ({},{}) 等级='{}' 解析={} / level='{}' parsed={}", row, col, level_text, level, level_text, level);

                if level != target.level {
                    continue; // Quick skip — wrong level
                }

                // Step 2: Level matches! OCR the first substat line and match
                // against the target's substats. The combination of
                // slot (already filtered by tab) + level + first substat
                // is almost always unique in an inventory.
                let sub_text = match ocr_region_enhanced(ctrl, ocr, SEL_SUBSTAT1_RECT) {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                if sub_text.is_empty() {
                    continue;
                }

                // Check if this substat text matches any of the target's substats.
                // OCR text looks like "·生命值+8.7%" or "·暴击伤害+14.8%"
                // Match "+VALUE%" or "+VALUE" at the end to avoid partial matches
                // (e.g., "11" matching "11.7").
                let expected_vals: Vec<String> = target.substats.iter().map(|s| {
                    if s.value.fract() == 0.0 {
                        format!("{}", s.value as i32)
                    } else {
                        format!("{}", s.value)
                    }
                }).collect();
                let substat_matched = expected_vals.iter().any(|v| {
                    // Check for "+VALUE%" or "+VALUE" followed by end/non-digit
                    let with_plus = format!("+{}", v);
                    if let Some(pos) = sub_text.find(&with_plus) {
                        let after = pos + with_plus.len();
                        // Value must be followed by '%', end of string, or non-digit
                        if after >= sub_text.len() {
                            return true;
                        }
                        let next_char = sub_text[after..].chars().next().unwrap();
                        return next_char == '%' || !next_char.is_ascii_digit();
                    }
                    false
                });

                if substat_matched {
                    info!(
                        "[selection] 匹配成功: page={} row={} col={}（已检查{}个），lv={}，sub='{}' / MATCH at page={} row={} col={} (checked {}), lv={}, sub='{}'",
                        page, row, col, total_checked, level, sub_text,
                        page, row, col, total_checked, level, sub_text
                    );
                    // Click "替换" (Replace) button
                    ctrl.click_at(SEL_ACTION_BUTTON_X, SEL_ACTION_BUTTON_Y);
                    yas::utils::sleep(800);
                    return Ok(true);
                } else {
                    debug!("[selection] ({},{}) lv={} 副词条不匹配: '{}' 期望值={:?} / sub mismatch: '{}' expected_vals={:?}", row, col, level, sub_text, expected_vals, sub_text, expected_vals);
                }
            }
        }

        // Scroll to next page
        let center_x = SEL_FIRST_X + 1.5 * SEL_OFFSET_X;
        let center_y = SEL_FIRST_Y + 1.5 * SEL_OFFSET_Y;
        ctrl.move_to(center_x, center_y);
        yas::utils::sleep(50);
        ctrl.mouse_scroll(SEL_SCROLL_TICKS);
        yas::utils::sleep(300);
    }

    info!("[selection] 检查了{}个圣遗物后未找到目标 / target not found after checking {} artifacts", total_checked, total_checked);
    Ok(false)
}

/// Parse a level string like "+20" or "20" into an i32.
fn parse_level(text: &str) -> i32 {
    let cleaned = text.trim().replace('+', "").replace(' ', "");
    cleaned.parse::<i32>().unwrap_or(-1)
}

/// Verify that the expected artifact is now equipped in the given slot.
///
/// Currently a no-op that returns Ok(true) — trusts that the click succeeded.
/// Can be enhanced later with re-click verification if reliability issues arise.
pub fn verify_artifact_equipped(
    _ctrl: &GenshinGameController,
    _slot_key: &str,
    _target: &ArtifactTarget,
    _scaler: &CoordScaler,
) -> Result<bool> {
    // For now, trust the equip click succeeded.
    // TODO: implement verification by re-clicking the slot and checking the detail panel.
    Ok(true)
}

/// Leave the character screen and return to main world.
///
/// Delegates to `ctrl.return_to_main_ui()`.
pub fn leave_character_screen(ctrl: &mut GenshinGameController) -> Result<()> {
    ctrl.return_to_main_ui(4);
    Ok(())
}

/// OCR a region with binarization for semi-transparent backgrounds.
///
/// The selection view detail panel has text overlaid on the character model
/// with a semi-transparent dark blue background. PaddleOCR is a single-line
/// recognition model that fails on noisy backgrounds (it picks up character
/// model noise as garbled text, or misses the actual text entirely).
///
/// Fix: always binarize — threshold brightness to create clean black text on
/// white background, then run OCR on the clean image.
fn ocr_region_enhanced(
    ctrl: &GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    rect: (f64, f64, f64, f64),
) -> Result<String> {
    let (x, y, w, h) = rect;
    let im = ctrl.capture_region(x, y, w, h)?;

    // Binarize: text is white/golden (brightness > 160), background is dark.
    // Create black-on-white (standard OCR input).
    let mut binarized = im;
    for pixel in binarized.pixels_mut() {
        let brightness = (pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3;
        if brightness > 160 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else {
            pixel[0] = 255;
            pixel[1] = 255;
            pixel[2] = 255;
        }
    }

    let text = ocr.image_to_text(&binarized, false)?;
    Ok(text.trim().to_string())
}

/// Extract the character name from an OCR'd name region.
/// The format is "X元素 / 角色名" (e.g., "水元素 / 芙宁娜").
/// Returns just the character name part, or the full text if no separator found.
fn extract_char_name(text: &str) -> String {
    let trimmed = text.trim();
    // Try splitting on various separators OCR produces between element and name:
    // "/" (ASCII), "／" (fullwidth), "丨" (CJK vertical U+4E28), "|", "1"
    for sep in &['/', '／', '丨', '|'] {
        if let Some(pos) = trimmed.rfind(*sep) {
            let after = &trimmed[pos + sep.len_utf8()..].trim();
            if !after.is_empty() {
                return after.to_string();
            }
        }
    }
    // Also try splitting on "元素" directly — the name follows the element type
    if let Some(pos) = trimmed.rfind("元素") {
        let after = &trimmed[pos + "元素".len()..].trim();
        if !after.is_empty() {
            return after.to_string();
        }
    }
    trimmed.to_string()
}

/// Clean a character name: extract the name part and strip trailing
/// OCR garbage (dots, punctuation, etc.).
fn clean_char_name(text: &str) -> String {
    let name = extract_char_name(text);
    // Strip trailing non-CJK characters (dots, punctuation, spaces, etc.)
    let chars: Vec<char> = name.chars().collect();
    let mut end = chars.len();
    while end > 0 {
        let c = chars[end - 1];
        // Keep CJK unified ideographs, common CJK ranges
        if c >= '\u{4e00}' && c <= '\u{9fff}' {
            break;
        }
        // Also keep katakana/hiragana for Japanese-origin names
        if c >= '\u{3040}' && c <= '\u{30ff}' {
            break;
        }
        end -= 1;
    }
    chars[..end].iter().collect()
}

/// Fuzzy match two character names: returns true if they differ by at most
/// 1 character and are at least 2 characters long.
/// Uses character-level comparison on the Unicode chars.
fn fuzzy_char_match(ocr_name: &str, expected: &str) -> bool {
    let ocr_chars: Vec<char> = ocr_name.chars().collect();
    let exp_chars: Vec<char> = expected.chars().collect();
    // Must be same length and at least 2 characters
    if ocr_chars.len() != exp_chars.len() || ocr_chars.len() < 2 {
        return false;
    }
    let diffs = ocr_chars.iter().zip(exp_chars.iter())
        .filter(|(a, b)| a != b)
        .count();
    diffs <= 1
}

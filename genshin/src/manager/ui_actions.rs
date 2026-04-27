//! In-game UI interaction functions for the artifact manager.
//!
//! All coordinates are at 1920x1080 base resolution and are automatically scaled
//! by `GenshinGameController` via `CoordScaler`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use anyhow::{bail, Result};
use image::RgbImage;
use yas::{log_debug, log_info, log_warn};

use yas::ocr::ImageToText;

use crate::scanner::common::constants::*;
use crate::scanner::common::coord_scaler::CoordScaler;
use crate::scanner::common::equip_parser;
use crate::scanner::common::fuzzy_match::fuzzy_match_map;
use crate::scanner::common::game_controller::GenshinGameController;
use crate::scanner::common::mappings::MappingManager;
use crate::scanner::common::models::{GoodArtifact, GoodSubStat};
use crate::scanner::common::roll_solver::{self, OcrCandidate, SolverInput};
use crate::scanner::common::stat_parser;

// ================================================================
// Configurable delays (thread-local, set once at manager startup)
// ================================================================

/// Manager delay configuration. All values in milliseconds.
///
/// The four base values scale proportionally to all UI waits:
/// - `transition` (1500ms): screen open/load — char screen, selection view, filter panel
/// - `action` (800ms): button click results — equip, escape, filter close, lock
/// - `cell` (100ms): per grid cell click during artifact search
/// - `scroll` (400ms): scroll settle after page navigation
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManagerDelays {
    pub transition: u64,
    pub action: u64,
    pub cell: u64,
    pub scroll: u64,
}

impl Default for ManagerDelays {
    fn default() -> Self {
        Self { transition: 1500, action: 800, cell: 100, scroll: 400 }
    }
}

thread_local! {
    static MGR_DELAYS: RefCell<ManagerDelays> = RefCell::new(ManagerDelays::default());
}

/// Set manager delays for the current thread.
/// Call this on the execution thread before any manager operations.
pub fn set_manager_delays(d: ManagerDelays) {
    MGR_DELAYS.with(|cell| *cell.borrow_mut() = d);
}

// Delay accessors (pub(crate) so equip_manager can use them).
// Return u32 to match yas::utils::sleep() signature.
pub(crate) fn d_transition() -> u32 { MGR_DELAYS.with(|c| c.borrow().transition as u32) }
pub(crate) fn d_action() -> u32 { MGR_DELAYS.with(|c| c.borrow().action as u32) }
pub(crate) fn d_cell() -> u32 { MGR_DELAYS.with(|c| c.borrow().cell as u32) }
pub(crate) fn d_scroll() -> u32 { MGR_DELAYS.with(|c| c.borrow().scroll as u32) }

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
const SEL_TAB_FLOWER: (f64, f64) = (80.0, 28.0);
const SEL_TAB_PLUME: (f64, f64) = (200.0, 28.0);
const SEL_TAB_SANDS: (f64, f64) = (325.0, 28.0);
const SEL_TAB_GOBLET: (f64, f64) = (425.0, 28.0);
const SEL_TAB_CIRCLET: (f64, f64) = (525.0, 28.0);

/// Selection grid layout (artifact selection list when clicking a character slot).
/// 4 columns, rows scroll vertically. Calibrated from selection view screenshot.
const SEL_COLS: usize = 4;
const SEL_ROWS: usize = 5; // visible rows per page
const SEL_FIRST_X: f64 = 89.0;
const SEL_FIRST_Y: f64 = 130.0;
const SEL_OFFSET_X: f64 = 141.0;
const SEL_OFFSET_Y: f64 = 167.0;

/// Temporary fixed delay for per-artifact clicks in character selection grid.
const SEL_GRID_CELL_DELAY_MS: u32 = 60;

/// Scroll ticks per page in the selection grid.
/// Matches backpack SCROLL_TICKS_PER_PAGE (49 for 5 rows).
const SEL_SCROLL_TICKS: i32 = 49;

/// "卸下"/"装备" button in the artifact selection view (bottom-right).
/// "卸下" when the selected artifact is currently equipped on this character.
/// "装备" otherwise.
/// Calibrated: button bg spans x=1467-1650, center at x≈1558, y≈1025.
const SEL_ACTION_BUTTON_X: f64 = 1558.0;
const SEL_ACTION_BUTTON_Y: f64 = 1025.0;

/// OCR region for the action button text ("卸下" vs "装备").
const SEL_ACTION_BUTTON_RECT: (f64, f64, f64, f64) = (1476.0, 992.0, 165.0, 45.0);

/// Owner label in selection view, e.g. "叶洛亚已装备".
/// 4K coordinates (3036, 1830) - (3500, 1910), divided by 2 for 1080p base.
const SEL_EQUIP_OWNER_RECT: (f64, f64, f64, f64) = (1518.0, 915.0, 232.0, 40.0);

/// Confirmation dialog "确认" button when equipping an artifact already on another character.
/// 4K coordinates (2356, 1510) → 1080p = (1178, 755).
/// Safe to always click: no-op when the dialog is not present.
const SEL_CONFIRM_BUTTON_X: f64 = 1178.0;
const SEL_CONFIRM_BUTTON_Y: f64 = 755.0;

// ----------------------------------------------------------------
// Selection view per-line crop regions (base 1920x1080).
// Calibrated via pixel analysis of selection view screenshots.
// ----------------------------------------------------------------

/// Main stat name (e.g., "攻击力", "暴击率").
const SEL_MAIN_STAT_RECT: (f64, f64, f64, f64) = (1440.0, 217.0, 250.0, 30.0);

/// Level badge ("+0", "+20").
const SEL_LEVEL_RECT: (f64, f64, f64, f64) = (1443.0, 310.0, 100.0, 26.0);

/// Star pixel positions for rarity detection (center of star at widest row).
const SEL_STAR4_POS: (f64, f64) = (1578.0, 280.0);
const SEL_STAR5_POS: (f64, f64) = (1611.0, 280.0);

/// Substat lines 0-3. 34px vertical spacing. Line 3 is wider for "(待激活)".
const SEL_SUB_RECTS: [(f64, f64, f64, f64); 4] = [
    (1460.0, 349.0, 256.0, 30.0),  // sub0
    (1460.0, 383.0, 256.0, 30.0),  // sub1
    (1460.0, 417.0, 256.0, 30.0),  // sub2
    (1460.0, 451.0, 336.0, 30.0),  // sub3 (wider for 待激活)
];
const SEL_SUB_SPACING: f64 = 34.0;

/// Set name (e.g., "纺月的夜歌: (0)"). Position assumes 4 substats.
/// For fewer subs, Y shifts up by SEL_SUB_SPACING per missing sub.
const SEL_SET_NAME_RECT: (f64, f64, f64, f64) = (1430.0, 489.0, 300.0, 30.0);

/// Full detail panel capture region for async grid scan.
/// Covers all OCR regions: main stat (y=217) through set name (y=519).
/// Captured once per cell, sub-regions are cropped from this in the OCR thread.
const SEL_PANEL_X: f64 = 1430.0;
const SEL_PANEL_Y: f64 = 210.0;
const SEL_PANEL_W: f64 = 390.0;
const SEL_PANEL_H: f64 = 320.0;

/// Value tolerance for substat matching (same as hard-match in matching.rs).
const VALUE_TOLERANCE: f64 = 0.1;

// ================================================================
// Set filter panel coordinates (opens from selection view bottom bar)
// ================================================================

/// "套装筛选" bar center at bottom-left of selection view.
/// The bar spans x=33-549, y=983-1032. Clicking the text area opens the
/// set filter panel overlay. The dark funnel icon is at ~(98, 1008) but
/// clicking the text center is more reliable.
pub const FILTER_FUNNEL_X: f64 = 309.0;
pub const FILTER_FUNNEL_Y: f64 = 1008.0;

/// "清空条件" (Clear conditions) button at bottom-left of filter panel.
pub const FILTER_CLEAR_X: f64 = 120.0;
pub const FILTER_CLEAR_Y: f64 = 1019.0;

/// "确认筛选" (Confirm filter) button at bottom-right of filter panel.
pub const FILTER_CONFIRM_X: f64 = 1727.0;
pub const FILTER_CONFIRM_Y: f64 = 1019.0;

/// Close (X) button at top-right of filter panel.
pub const FILTER_CLOSE_X: f64 = 1841.0;
pub const FILTER_CLOSE_Y: f64 = 48.0;

/// First data row Y center in the set filter panel.
/// Was 236.5 which skipped the actual first row (风起之日). Moved up by one
/// row spacing to capture all rows including the top one.
pub const FILTER_FIRST_ROW_Y: f64 = 155.0;
/// Vertical spacing between consecutive row centers.
pub const FILTER_ROW_SPACING: f64 = 81.5;
/// Number of visible rows per column before scrolling.
pub const FILTER_VISIBLE_ROWS: usize = 10;

/// OCR text region for set names in the left column.
/// The left column layout is: [radio] [icon] [set name] [count].
/// The icon ends around x=140. Starting at x=150 to capture the full first
/// character (x=175 was cutting it off at 4K resolution).
pub const FILTER_LEFT_TEXT_X: f64 = 150.0;
pub const FILTER_LEFT_TEXT_W: f64 = 250.0;
/// OCR text region for set names in the right column.
/// The right column layout is: [count] [radio] [icon] [text].
/// The icon ends around x=795, text starts at ~x=800-815.
/// Calibrated from pixel analysis of filter panel screenshot.
pub const FILTER_RIGHT_TEXT_X: f64 = 800.0;
pub const FILTER_RIGHT_TEXT_W: f64 = 250.0;
/// Text region height for set name OCR (+1px top, +2px bottom for tolerance).
pub const FILTER_TEXT_H: f64 = 40.0;
/// Upward offset from row center to text region top (asymmetric: 1px more top, 2px more bottom).
pub const FILTER_TEXT_TOP_OFFSET: f64 = 19.0;

/// Click target X for selecting a set in the left column.
pub const FILTER_LEFT_CLICK_X: f64 = 260.0;
/// Click target X for selecting a set in the right column.
pub const FILTER_RIGHT_CLICK_X: f64 = 720.0;

/// Mouse position for scrolling in the filter panel (center of set list).
pub const FILTER_SCROLL_X: f64 = 300.0;
pub const FILTER_SCROLL_Y: f64 = 500.0;
/// Scroll ticks per page in the filter set list.
/// Each tick is sent individually with small delays (like backpack_scanner).
/// 49 overshot badly (elastic bounce). 18 was ~5 rows. 32 was slightly short.
/// 35 (~8% more than 32) for a full page at 4K.
pub const FILTER_SCROLL_TICKS: i32 = 41;

/// Delay between batches of scroll ticks (ms).
pub const SCROLL_TICK_DELAY_MS: u32 = 20;
/// Delay after scroll completes to let animation settle (ms).
pub const SCROLL_SETTLE_MS: u32 = 400;

// ================================================================
// Implementation
// ================================================================

/// Scroll by sending individual ticks with small delays.
///
/// Genshin's scroll handling requires 1-tick-at-a-time input with
/// periodic delays — a single large `mouse_scroll(N)` call barely moves.
fn scroll_ticks(ctrl: &mut GenshinGameController, ticks: i32) {
    scroll_ticks_dir(ctrl, ticks, 1);
}

/// Scroll with explicit direction (+1 = up/backpack convention, -1 = down/selection grid).
fn scroll_ticks_dir(ctrl: &mut GenshinGameController, ticks: i32, direction: i32) {
    for i in 0..ticks {
        ctrl.mouse_scroll(direction);
        if (i + 1) % 5 == 0 {
            yas::utils::sleep(SCROLL_TICK_DELAY_MS);
        }
    }
    yas::utils::sleep(SCROLL_SETTLE_MS);
}

/// Click the lock/unlock button on the artifact detail panel.
///
/// The lock icon is on the artifact detail panel (right side of backpack).
/// Detection pixels are at (1683,428) and (1708,428); the clickable center
/// of the dark rounded-rect background is at (1696, 432).
///
/// For elixir artifacts, the lock icon shifts down by `y_shift` (40px).
pub fn click_lock_button(ctrl: &mut GenshinGameController, y_shift: f64) -> Result<()> {
    ctrl.click_at(LOCK_BUTTON_X, LOCK_BUTTON_Y + y_shift);
    yas::utils::sleep(d_action() * 3 / 8); // wait for lock animation
    Ok(())
}

/// Open a specific character's detail screen from the main world.
///
/// Press C to open the character screen, with retry and OCR verification.
///
/// Verifies by checking for element prefix ("X元素") and a known character name
/// in the name region. This distinguishes the character screen from the system
/// menu (which also shows CJK text like the Traveler's custom name).
pub fn ensure_character_screen(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
) -> Result<()> {
    ctrl.focus_game_window();
    for attempt in 0..3 {
        ctrl.key_press(enigo::Key::Layout('c'));
        yas::utils::sleep(d_transition());

        let name_text = ctrl.ocr_region(ocr, CHAR_NAME_RECT).unwrap_or_default();
        if is_character_screen_name(&name_text, mappings) {
            log_debug!("[ensure_character_screen] 已打开（第{}次尝试），名称='{}'", "opened (attempt {}), name='{}'", attempt, name_text.trim());
            return Ok(());
        }
        log_debug!(
            "[ensure_character_screen] 第{}次尝试失败（OCR: '{}'），按Escape重试",
            "attempt {} failed (OCR: '{}'), pressing Escape and retrying", attempt, name_text.trim());
        ctrl.key_press(enigo::Key::Escape);
        yas::utils::sleep(d_action());
    }
    bail!("Cannot open character screen after 3 attempts");
}

/// Check if OCR text from CHAR_NAME_RECT looks like a character screen name.
/// Format: "X元素／角色名" (e.g., "风元素／法尔伽").
/// Requires element prefix AND a known character name from mappings.
fn is_character_screen_name(text: &str, mappings: &MappingManager) -> bool {
    let text = text.trim();
    // Must contain element indicator
    if !text.contains("元素") {
        return false;
    }
    // Check if any known character name appears in the text
    for cn_name in mappings.character_name_map.keys() {
        if text.contains(cn_name.as_str()) {
            return true;
        }
    }
    false
}

/// Presses C to open the character roster, then cycles through characters
/// using the "next" button while OCR-ing the character name until the target
/// is found.
pub fn open_character_screen(
    ctrl: &mut GenshinGameController,
    char_key: &str,
    mappings: &MappingManager,
    ocr: &dyn ImageToText<RgbImage>,
) -> Result<()> {
    // Reverse-lookup: GOOD key -> Chinese name(s) for matching
    let cn_names: Vec<String> = mappings
        .character_name_map
        .iter()
        .filter(|(_, v)| v.as_str() == char_key)
        .map(|(k, _)| k.clone())
        .collect();

    // Ensure we're in main world first, then open character screen.
    ctrl.return_to_main_ui(8);
    yas::utils::sleep(d_action() * 5 / 8);
    ensure_character_screen(ctrl, ocr, mappings)?;

    let max_chars = 150; // safety limit (must exceed account roster size)
    let mut first_name: Option<String> = None;

    for i in 0..max_chars {
        if ctrl.check_rmb() {
            bail!("{}", ctrl.cancel_token().reason().unwrap());
        }

        // OCR character name
        let name_text = ctrl.ocr_region(ocr, CHAR_NAME_RECT)?;
        let name_trimmed = name_text.trim().to_string();
        log_debug!("[open_character_screen] #{}: OCR识别名称='{}'", "#{}: OCR name='{}'", i, name_trimmed);

        // Check for full cycle (returned to first character).
        // Use cleaned names (stripped of trailing garbage) for robust comparison.
        if i > 0 {
            if let Some(ref first) = first_name {
                let cur_name = clean_char_name(&name_trimmed);
                let first_char_name = clean_char_name(first);
                log_debug!(
                    "[open_character_screen] #{}: 当前='{}' vs 首个='{}'",
                    "#{}: cur='{}' vs first='{}'", i, cur_name, first_char_name);
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
            log_info!("[open_character_screen] 找到{}，位置{}", "found {} at position {}", char_key, i);
            return Ok(());
        }

        // Match against Chinese name(s) — exact substring first, then fuzzy
        let mut found = false;
        for cn in &cn_names {
            if name_trimmed.contains(cn.as_str()) {
                log_info!("[open_character_screen] 找到{}（中文: {}），位置{}", "found {} (cn: {}) at position {}", char_key, cn, i);
                return Ok(());
            }
            // Fuzzy match: allow 1 character difference for names >= 2 chars
            if fuzzy_char_match(&clean_name, cn) {
                log_info!(
                    "[open_character_screen] 找到{}（中文: {}，模糊匹配'{}'），位置{}",
                    "found {} (cn: {}, fuzzy match '{}') at position {}", char_key, cn, clean_name, i);
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
                    log_info!("[open_character_screen] 通过映射找到{}，位置{}", "found {} via mapping at position {}", char_key, i);
                    return Ok(());
                }
            }
        }

        // Click next character
        ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
        yas::utils::sleep(d_scroll());
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
    yas::utils::sleep(d_cell() * 2);

    // Step 1: Click 圣遗物 menu to show artifact circles
    ctrl.click_at(CHAR_ARTIFACT_MENU_X, CHAR_ARTIFACT_MENU_Y);
    yas::utils::sleep(d_transition() * 4 / 5); // wait for circle animation

    // Step 2: Click "替换" button to open the artifact selection list.
    ctrl.click_at(CHAR_REPLACE_BUTTON_X, CHAR_REPLACE_BUTTON_Y);
    yas::utils::sleep(d_transition() * 4 / 3); // wait for selection view to load

    // Step 3: Click the correct slot tab
    ctrl.click_at(tab_pos.0, tab_pos.1);
    yas::utils::sleep(d_action());

    Ok(())
}

/// Click the "卸下" (unequip) button to remove an artifact.
///
/// When the selection view opens with the currently equipped artifact selected
/// (always the first item), the "卸下" button appears at the bottom.
pub fn click_unequip_button(ctrl: &mut GenshinGameController) -> Result<()> {
    ctrl.click_at(SEL_ACTION_BUTTON_X, SEL_ACTION_BUTTON_Y);
    yas::utils::sleep(d_action() * 5 / 8);
    Ok(())
}

/// Detect a target set in the currently visible filter rows.
/// Returns `Some((click_x, y))` with the click coordinates if found, `None` otherwise.
pub fn detect_set_in_visible_rows(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    set_key: &str,
    mappings: &MappingManager,
) -> Result<Option<(f64, f64)>> {
    let mut detected_keys: Vec<String> = Vec::new();
    let mut result = None;

    for row in 0..FILTER_VISIBLE_ROWS {
        if ctrl.check_rmb() {
            bail!("Cancelled by user (right-click)");
        }
        let y = FILTER_FIRST_ROW_Y + row as f64 * FILTER_ROW_SPACING;
        let text_y = y - FILTER_TEXT_TOP_OFFSET;

        // Check left column
        let left_text = ctrl.ocr_region(
            ocr,
            (FILTER_LEFT_TEXT_X, text_y, FILTER_LEFT_TEXT_W, FILTER_TEXT_H),
        ).unwrap_or_default();

        if let Some(key) = fuzzy_match_map(&left_text, &mappings.artifact_set_map) {
            detected_keys.push(key.clone());
            if key == set_key && result.is_none() {
                log_debug!(
                    "[set_filter] found '{}' in left col row {} (OCR: '{}')",
                    "[set_filter] found '{}' in left col row {} (OCR: '{}')", set_key, row, left_text);
                result = Some((FILTER_LEFT_CLICK_X, y));
            }
        }

        // Check right column
        let right_text = ctrl.ocr_region(
            ocr,
            (FILTER_RIGHT_TEXT_X, text_y, FILTER_RIGHT_TEXT_W, FILTER_TEXT_H),
        ).unwrap_or_default();

        if let Some(key) = fuzzy_match_map(&right_text, &mappings.artifact_set_map) {
            detected_keys.push(key.clone());
            if key == set_key && result.is_none() {
                log_debug!(
                    "[set_filter] found '{}' in right col row {} (OCR: '{}')",
                    "[set_filter] found '{}' in right col row {} (OCR: '{}')", set_key, row, right_text);
                result = Some((FILTER_RIGHT_CLICK_X, y));
            }
        }
    }

    log_debug!(
        "[set_filter] visible scan: {} sets detected: {:?}",
        "[set_filter] visible scan: {} sets detected: {:?}", detected_keys.len(), detected_keys);
    Ok(result)
}

/// Scroll the filter panel to the top and search for a set by scrolling down.
/// Returns `Some((click_x, y))` if found, `None` if not found after all scrolls.
pub fn find_set_in_filter_panel(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    set_key: &str,
    mappings: &MappingManager,
) -> Result<Option<(f64, f64)>> {
    // Quick scan: check currently visible rows first
    if let Some(pos) = detect_set_in_visible_rows(ctrl, ocr, set_key, mappings)? {
        return Ok(Some(pos));
    }

    // Scroll to top
    ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
    yas::utils::sleep(50);
    let scroll_up_ticks = 96;
    scroll_ticks_dir(ctrl, scroll_up_ticks, -1);
    yas::utils::sleep(d_cell() + 50);

    // Scan + scroll down
    let max_scrolls = 3;
    for scroll in 0..=max_scrolls {
        if let Some(pos) = detect_set_in_visible_rows(ctrl, ocr, set_key, mappings)? {
            return Ok(Some(pos));
        }
        if scroll < max_scrolls {
            ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
            yas::utils::sleep(50);
            scroll_ticks(ctrl, FILTER_SCROLL_TICKS);
        }
    }
    Ok(None)
}

/// Debug info for one OCR region in the filter panel.
pub struct FilterOcrHit {
    /// Base coordinates (1080p) of the OCR region.
    pub base_rect: (f64, f64, f64, f64),
    /// Row index (0-based).
    pub row: usize,
    /// "left" or "right" column.
    pub column: &'static str,
    /// Raw OCR text.
    pub ocr_text: String,
    /// Matched GOOD key (if any).
    pub matched_key: Option<String>,
}

/// Scan visible filter rows and return debug info for every OCR region.
/// Also returns the match position if the target set_key was found.
pub fn detect_set_in_visible_rows_debug(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    set_key: &str,
    mappings: &MappingManager,
) -> Result<(Option<(f64, f64)>, Vec<FilterOcrHit>)> {
    let mut hits: Vec<FilterOcrHit> = Vec::new();
    let mut result = None;

    for row in 0..FILTER_VISIBLE_ROWS {
        if ctrl.check_rmb() {
            bail!("Cancelled by user (right-click)");
        }
        let y = FILTER_FIRST_ROW_Y + row as f64 * FILTER_ROW_SPACING;
        let text_y = y - FILTER_TEXT_TOP_OFFSET;

        // Left column
        let left_text = ctrl.ocr_region(
            ocr,
            (FILTER_LEFT_TEXT_X, text_y, FILTER_LEFT_TEXT_W, FILTER_TEXT_H),
        ).unwrap_or_default();
        let left_key = fuzzy_match_map(&left_text, &mappings.artifact_set_map);
        if left_key.as_deref() == Some(set_key) && result.is_none() {
            result = Some((FILTER_LEFT_CLICK_X, y));
        }
        hits.push(FilterOcrHit {
            base_rect: (FILTER_LEFT_TEXT_X, text_y, FILTER_LEFT_TEXT_W, FILTER_TEXT_H),
            row, column: "left",
            ocr_text: left_text.trim().to_string(),
            matched_key: left_key,
        });

        // Right column
        let right_text = ctrl.ocr_region(
            ocr,
            (FILTER_RIGHT_TEXT_X, text_y, FILTER_RIGHT_TEXT_W, FILTER_TEXT_H),
        ).unwrap_or_default();
        let right_key = fuzzy_match_map(&right_text, &mappings.artifact_set_map);
        if right_key.as_deref() == Some(set_key) && result.is_none() {
            result = Some((FILTER_RIGHT_CLICK_X, y));
        }
        hits.push(FilterOcrHit {
            base_rect: (FILTER_RIGHT_TEXT_X, text_y, FILTER_RIGHT_TEXT_W, FILTER_TEXT_H),
            row, column: "right",
            ocr_text: right_text.trim().to_string(),
            matched_key: right_key,
        });
    }

    Ok((result, hits))
}

/// Draw colored boxes on a screenshot for each OCR hit.
/// Green = matched a known set, red = no match, blue border = target set found.
pub fn annotate_filter_screenshot(
    img: &mut RgbImage,
    hits: &[FilterOcrHit],
    scaler: &CoordScaler,
    target_key: &str,
) {
    let w = img.width() as i32;
    let h = img.height() as i32;

    for hit in hits {
        let (bx, by, bw, bh) = hit.base_rect;
        let x1 = scaler.scale_x(bx) as i32;
        let y1 = scaler.scale_y(by) as i32;
        let x2 = (scaler.scale_x(bx + bw) as i32).min(w - 1);
        let y2 = (scaler.scale_y(by + bh) as i32).min(h - 1);

        let color: [u8; 3] = if hit.matched_key.as_deref() == Some(target_key) {
            [0, 120, 255] // blue = target found
        } else if hit.matched_key.is_some() {
            [0, 200, 0] // green = matched some set
        } else {
            [200, 0, 0] // red = no match
        };

        // Draw rectangle border (2px thick)
        for thickness in 0..2 {
            let t = thickness as i32;
            // Top and bottom edges
            for x in x1..=x2 {
                if y1 + t >= 0 && y1 + t < h && x >= 0 && x < w {
                    img.put_pixel(x as u32, (y1 + t) as u32, image::Rgb(color));
                }
                if y2 - t >= 0 && y2 - t < h && x >= 0 && x < w {
                    img.put_pixel(x as u32, (y2 - t) as u32, image::Rgb(color));
                }
            }
            // Left and right edges
            for y in y1..=y2 {
                if x1 + t >= 0 && x1 + t < w && y >= 0 && y < h {
                    img.put_pixel((x1 + t) as u32, y as u32, image::Rgb(color));
                }
                if x2 - t >= 0 && x2 - t < w && y >= 0 && y < h {
                    img.put_pixel((x2 - t) as u32, y as u32, image::Rgb(color));
                }
            }
        }
    }
}

/// Like `find_set_in_filter_panel` but saves annotated debug screenshots at each step.
///
/// Follows the exact same logic (quick scan, then full scan if not found).
/// Captures a screenshot + draws OCR region boxes at every scan position.
/// `debug_prefix` is the file path prefix (e.g., "debug_images/set_filter_test/ArchaicPetra").
pub fn find_set_in_filter_panel_debug(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    set_key: &str,
    mappings: &MappingManager,
    debug_prefix: &str,
) -> Result<Option<(f64, f64)>> {
    let mut page_idx = 0;

    // Helper: capture, OCR, annotate, save
    let save_page = |ctrl: &mut GenshinGameController, ocr: &dyn ImageToText<RgbImage>,
                     set_key: &str, mappings: &MappingManager,
                     prefix: &str, idx: usize| -> Result<Option<(f64, f64)>> {
        let mut img = ctrl.capture_game()?;
        let (found, hits) = detect_set_in_visible_rows_debug(ctrl, ocr, set_key, mappings)?;
        annotate_filter_screenshot(&mut img, &hits, &ctrl.scaler, set_key);
        let path = format!("{}_{}.png", prefix, idx);
        let _ = img.save(&path);
        Ok(found)
    };

    // Quick scan at current position
    let found = save_page(ctrl, ocr, set_key, mappings, debug_prefix, page_idx)?;
    if found.is_some() {
        return Ok(found);
    }
    page_idx += 1;

    // Scroll to top
    ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
    yas::utils::sleep(50);
    let scroll_up_ticks = 96;
    scroll_ticks_dir(ctrl, scroll_up_ticks, -1);
    yas::utils::sleep(d_cell() + 50);

    // Scan + scroll down
    let max_scrolls = 3;
    for scroll in 0..=max_scrolls {
        let found = save_page(ctrl, ocr, set_key, mappings, debug_prefix, page_idx)?;
        page_idx += 1;
        if found.is_some() {
            return Ok(found);
        }
        if scroll < max_scrolls {
            ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
            yas::utils::sleep(50);
            scroll_ticks(ctrl, FILTER_SCROLL_TICKS);
        }
    }
    Ok(None)
}
fn click_filter_set_and_confirm(
    ctrl: &mut GenshinGameController,
    click_x: f64,
    y: f64,
) -> Result<()> {
    ctrl.click_at(click_x, y);
    yas::utils::sleep(d_action());
    ctrl.click_at(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
    yas::utils::sleep(d_transition() * 4 / 5);
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
            log_info!("[set_filter] 套装'{}'未在映射中找到，跳过筛选", "set key '{}' not found in mappings, skipping filter", set_key);
            return Ok(false);
        }
    };

    log_info!("[set_filter] 正在筛选{}（{}）", "applying filter for {} ({})", set_key, cn_name);

    // Open filter panel
    ctrl.click_at(FILTER_FUNNEL_X, FILTER_FUNNEL_Y);
    yas::utils::sleep(d_transition() * 2 / 3);

    // Clear existing filters
    ctrl.click_at(FILTER_CLEAR_X, FILTER_CLEAR_Y);
    yas::utils::sleep(d_action() * 3 / 8);

    // Find the set in the filter panel (quick scan → scroll to top → scroll down)
    match find_set_in_filter_panel(ctrl, ocr, set_key, mappings)? {
        Some((click_x, y)) => {
            click_filter_set_and_confirm(ctrl, click_x, y)?;
            Ok(true)
        }
        None => {
            log_info!("[set_filter] 套装'{}'（{}）未在筛选列表中找到", "set '{}' ({}) not found in filter list", set_key, cn_name);
            ctrl.click_at(FILTER_CLOSE_X, FILTER_CLOSE_Y);
            yas::utils::sleep(d_action() * 5 / 8);
            Ok(false)
        }
    }
}

/// Clear the set filter in the artifact selection view.
///
/// Opens the filter panel, clears all selections, and confirms.
pub fn clear_set_filter(ctrl: &mut GenshinGameController) -> Result<()> {
    ctrl.click_at(FILTER_FUNNEL_X, FILTER_FUNNEL_Y);
    yas::utils::sleep(d_transition());
    ctrl.click_at(FILTER_CLEAR_X, FILTER_CLEAR_Y);
    yas::utils::sleep(d_action() * 5 / 8);
    ctrl.move_to(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
    yas::utils::sleep(d_cell());
    ctrl.click_at(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
    yas::utils::sleep(d_transition());
    Ok(())
}

/// Apply a set filter for multiple sets at once.
///
/// Opens the filter panel, clears existing filters, selects all matching sets
/// (without confirming between each), then confirms once. The filter panel
/// supports multi-select — clicking multiple rows selects them all.
///
/// Returns the number of sets successfully selected.
pub fn apply_multi_set_filter(
    ctrl: &mut GenshinGameController,
    set_keys: &[&str],
    mappings: &MappingManager,
    ocr: &dyn ImageToText<RgbImage>,
) -> Result<usize> {
    // Reverse lookup all set keys → Chinese names
    let cn_targets: Vec<(String, String)> = set_keys.iter().filter_map(|&key| {
        mappings.artifact_set_map.iter()
            .find(|(_, v)| v.as_str() == key)
            .map(|(k, _)| (key.to_string(), k.clone()))
    }).collect();

    if cn_targets.is_empty() {
        log_info!("[set_filter] 无有效套装映射", "no valid set mappings found");
        return Ok(0);
    }

    log_info!("[set_filter] 正在筛选{}个套装: {:?}", "applying filter for {} sets: {:?}", cn_targets.len(), set_keys);

    // Open filter panel
    ctrl.click_at(FILTER_FUNNEL_X, FILTER_FUNNEL_Y);
    yas::utils::sleep(d_transition() * 2 / 3);

    // Clear existing filters
    ctrl.click_at(FILTER_CLEAR_X, FILTER_CLEAR_Y);
    yas::utils::sleep(d_action() * 3 / 8);

    // Scroll filter list back to top (4× one page to cover worst case)
    ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
    yas::utils::sleep(50);
    let scroll_up_ticks = 96;
    scroll_ticks_dir(ctrl, scroll_up_ticks, -1);
    // Extra settle for over-scroll bounce recovery
    yas::utils::sleep(d_cell() + 50);

    let mut found_count = 0;
    let mut remaining_keys: Vec<String> = cn_targets.iter().map(|(key, _)| key.clone()).collect();

    let max_scrolls = 5;
    for scroll in 0..=max_scrolls {
        if remaining_keys.is_empty() {
            break;
        }
        for row in 0..FILTER_VISIBLE_ROWS {
            if remaining_keys.is_empty() {
                break;
            }
            let y = FILTER_FIRST_ROW_Y + row as f64 * FILTER_ROW_SPACING;
            let text_y = y - FILTER_TEXT_TOP_OFFSET;

            // Check left column
            let left_text = ctrl.ocr_region(
                ocr,
                (FILTER_LEFT_TEXT_X, text_y, FILTER_LEFT_TEXT_W, FILTER_TEXT_H),
            ).unwrap_or_default();

            if let Some(matched_key) = fuzzy_match_map(&left_text, &mappings.artifact_set_map) {
                if let Some(pos) = remaining_keys.iter().position(|k| *k == matched_key) {
                    log_debug!(
                        "[set_filter] 左列找到'{}' 第{}行 (OCR: '{}')",
                        "found '{}' in left col row {} (OCR: '{}')", matched_key, row, left_text);
                    ctrl.click_at(FILTER_LEFT_CLICK_X, y);
                    yas::utils::sleep(d_action() * 3 / 8);
                    remaining_keys.remove(pos);
                    found_count += 1;
                    continue;
                }
            }

            // Check right column
            let right_text = ctrl.ocr_region(
                ocr,
                (FILTER_RIGHT_TEXT_X, text_y, FILTER_RIGHT_TEXT_W, FILTER_TEXT_H),
            ).unwrap_or_default();

            if let Some(matched_key) = fuzzy_match_map(&right_text, &mappings.artifact_set_map) {
                if let Some(pos) = remaining_keys.iter().position(|k| *k == matched_key) {
                    log_debug!(
                        "[set_filter] 右列找到'{}' 第{}行 (OCR: '{}')",
                        "found '{}' in right col row {} (OCR: '{}')", matched_key, row, right_text);
                    ctrl.click_at(FILTER_RIGHT_CLICK_X, y);
                    yas::utils::sleep(d_action() * 3 / 8);
                    remaining_keys.remove(pos);
                    found_count += 1;
                }
            }
        }

        // Scroll down to see more sets
        if scroll < max_scrolls && !remaining_keys.is_empty() {
            ctrl.move_to(FILTER_SCROLL_X, FILTER_SCROLL_Y);
            yas::utils::sleep(50);
            scroll_ticks(ctrl, FILTER_SCROLL_TICKS);
        }
    }

    if found_count > 0 {
        ctrl.click_at(FILTER_CONFIRM_X, FILTER_CONFIRM_Y);
        yas::utils::sleep(d_action());
        log_info!("[set_filter] 已应用{}个套装筛选", "applied {} set filters", found_count);
    } else {
        ctrl.click_at(FILTER_CLOSE_X, FILTER_CLOSE_Y);
        yas::utils::sleep(d_action() * 5 / 8);
    }

    Ok(found_count)
}

/// Check if the currently displayed artifact in the selection view matches the target.
///
/// Performs full matching: level, main stat key, all substat keys + values
/// (with tolerance), unactivated substats, and set name.
///
/// Returns `Ok(true)` if the current artifact matches the target.
pub fn check_current_artifact_matches(
    ctrl: &GenshinGameController,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
) -> Result<bool> {
    full_match_detail_panel(ctrl, target, ocr, mappings, "check_current")
}

/// OCR the selected artifact's owner label in the selection view.
///
/// Returns `(raw_ocr_text, GOOD_character_key)`. The key is empty when the
/// label is absent or cannot be matched to a known character.
pub fn read_selected_artifact_owner(
    ctrl: &GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
) -> Result<(String, String)> {
    let raw_text = ocr_region_enhanced(ctrl, ocr, SEL_EQUIP_OWNER_RECT).unwrap_or_default();
    let owner_key = equip_parser::parse_equip_owner(&raw_text, &mappings.character_name_map);
    log_debug!(
        "[selection] 当前圣遗物归属OCR='{}' -> '{}'",
        "[selection] selected artifact owner OCR='{}' -> '{}'",
        raw_text,
        owner_key
    );
    Ok((raw_text, owner_key))
}

/// Equip the currently selected artifact in the selection view.
///
/// Returns `Ok(true)` when the equip/replace button was clicked. Returns
/// `Ok(false)` when the button is "卸下", meaning it is already on this character.
pub fn equip_selected_artifact(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    tag: &str,
) -> Result<bool> {
    click_equip_button_safe_at(ctrl, ocr, tag, None)
}

/// Click a slot tab in the artifact selection view (without re-opening the selection).
///
/// Use this when already in the selection view to switch between slot tabs.
pub fn click_slot_tab(ctrl: &mut GenshinGameController, slot_key: &str) -> Result<()> {
    let tab_pos = match slot_key {
        "flower" => SEL_TAB_FLOWER,
        "plume" => SEL_TAB_PLUME,
        "sands" => SEL_TAB_SANDS,
        "goblet" => SEL_TAB_GOBLET,
        "circlet" => SEL_TAB_CIRCLET,
        _ => bail!("未知栏位 / Unknown slot: {}", slot_key),
    };
    ctrl.click_at(tab_pos.0, tab_pos.1);
    yas::utils::sleep(d_action());
    Ok(())
}

/// Debug info for one cell processed during grid scan.
#[derive(Clone)]
pub struct GridCellDebug {
    pub page: usize,
    pub row: usize,
    pub col: usize,
    pub level_text: String,
    pub level: i32,
    pub full_ocr: bool,
    pub match_result: Option<bool>, // None = not attempted, Some(true) = matched
    pub ocr_details: String,        // human-readable OCR summary
    pub panel_image: RgbImage,
}

/// Scan the artifact selection grid to find and equip a target artifact.
///
/// Assumes the set filter and slot tab are already applied.
///
/// **Async design**: Within each page (20 cells), the main thread clicks cells
/// and captures the detail panel image (60ms per cell). A background OCR thread
/// processes captures concurrently:
/// - OCR the level first (fast reject for non-matching levels)
/// - For level-matched cells (or row 0 for fingerprint): full OCR + match
/// - On match: signals the main thread to stop clicking early
///
/// Pages are processed synchronously (wait for OCR to finish before scrolling).
///
/// Returns `Ok(true)` if found (and equipped if `equip` is true).
/// Debug images are collected in `debug_out` if provided; caller decides whether to save/clear.
pub fn find_artifact_in_grid(
    ctrl: &mut GenshinGameController,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
    equip: bool,
) -> Result<bool> {
    find_artifact_in_grid_inner(ctrl, target, ocr, mappings, equip, None, false)
}

/// Like `find_artifact_in_grid` but with dump_images support.
pub fn find_artifact_in_grid_with_dump(
    ctrl: &mut GenshinGameController,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
    equip: bool,
    dump_images: bool,
) -> Result<bool> {
    find_artifact_in_grid_inner(ctrl, target, ocr, mappings, equip, None, dump_images)
}

/// Like `find_artifact_in_grid` but collects debug info for each cell.
pub fn find_artifact_in_grid_debug(
    ctrl: &mut GenshinGameController,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
    equip: bool,
    debug_out: &mut Vec<GridCellDebug>,
) -> Result<bool> {
    find_artifact_in_grid_inner(ctrl, target, ocr, mappings, equip, Some(debug_out), false)
}

fn find_artifact_in_grid_inner(
    ctrl: &mut GenshinGameController,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
    equip: bool,
    mut debug_out: Option<&mut Vec<GridCellDebug>>,
    dump_images: bool,
) -> Result<bool> {
    let max_pages = 50;
    let mut total_checked: usize = 0;

    log_debug!("[grid_scan] 开始搜索: set={} slot={} lv={}", "starting: set={} slot={} lv={}", target.set_key, target.slot_key, target.level);

    // Scroll to top
    {
        let cx = SEL_FIRST_X + 1.5 * SEL_OFFSET_X;
        let cy = SEL_FIRST_Y + 1.5 * SEL_OFFSET_Y;
        ctrl.click_at(cx, cy);
        yas::utils::sleep(d_cell());
        ctrl.move_to(cx, cy);
        yas::utils::sleep(50);
        scroll_ticks_dir(ctrl, 100, -1); // UP (negative = up)
    }

    let mut prev_fingerprint = String::new();
    let mut same_fp_count = 0;
    let mut prev_retry_count: Option<usize> = None;
    let mut same_retry_count = 0;

    for page in 0..max_pages {
        if ctrl.check_rmb() {
            bail!("{}", ctrl.cancel_token().reason().unwrap());
        }

        // --- Async page scan ---
        let found_flag = Arc::new(AtomicBool::new(false));
        let match_cell: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
        let page_fingerprint: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let page_ended_flag = Arc::new(AtomicBool::new(false));
        let cells_checked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cell_debugs: Arc<Mutex<Vec<GridCellDebug>>> = Arc::new(Mutex::new(Vec::new()));
        // Cells where level matched but full match failed (candidates for retry)
        let retry_cells: Arc<Mutex<Vec<(usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));

        let (tx, rx) = mpsc::channel::<(usize, usize, RgbImage)>();

        // Scoped thread: OCR worker borrows ocr/target/mappings from this stack frame.
        {
            let found = found_flag.clone();
            let match_result = match_cell.clone();
            let fp_out = page_fingerprint.clone();
            let ended = page_ended_flag.clone();
            let checked = cells_checked.clone();
            let debugs = cell_debugs.clone();
            let retries = retry_cells.clone();
            let collect_debug = debug_out.is_some();

            std::thread::scope(|s| {
                // OCR thread
                s.spawn(move || {
                    // Track full OCR fingerprint per cell for empty detection.
                    // Empty cells show the same panel as the previous click, so
                    // ALL OCR fields are identical (not just level).
                    let mut prev_cell_fingerprint = String::new();
                    let mut consecutive_same: usize = 0;

                    for (row, col, panel_img) in rx {
                        // If already found, just drain the channel
                        if found.load(Ordering::SeqCst) {
                            continue;
                        }

                        // OCR level
                        let level_img = crop_from_panel(&panel_img, SEL_LEVEL_RECT);
                        let level_text = ocr.image_to_text(&level_img, false)
                            .unwrap_or_default().trim().to_string();
                        let level = parse_level(&level_text);

                        // Row 0: collect level texts as page fingerprint
                        if row == 0 {
                            let mut fp = fp_out.lock().unwrap();
                            if !fp.is_empty() { fp.push(','); }
                            fp.push_str(&level_text);
                        }

                        // OCR first substat to build cell fingerprint (level + sub0).
                        // Two different artifacts at the same level will almost always
                        // have different first substats, while empty cells show
                        // identical data to the previous click.
                        let sub0_img = crop_from_panel(&panel_img, SEL_SUB_RECTS[0]);
                        let sub0_text = ocr.image_to_text(&sub0_img, false)
                            .unwrap_or_default().trim().to_string();
                        let cell_fingerprint = format!("{}|{}", level_text, sub0_text);

                        // Empty cell detection: OCR failed, or this cell produced
                        // exactly the same level+sub0 as the previous cell (panel
                        // unchanged because the click landed on empty space).
                        let is_empty = level < 0
                            || (!(row == 0 && col == 0) && cell_fingerprint == prev_cell_fingerprint);
                        prev_cell_fingerprint = cell_fingerprint;

                        if is_empty {
                            consecutive_same += 1;
                            if collect_debug {
                                debugs.lock().unwrap().push(GridCellDebug {
                                    page, row, col,
                                    level_text: level_text.clone(),
                                    level,
                                    full_ocr: false,
                                    match_result: None,
                                    ocr_details: "(empty cell)".to_string(),
                                    panel_image: panel_img,
                                });
                            }
                            if consecutive_same >= SEL_COLS {
                                ended.store(true, Ordering::SeqCst);
                            }
                            continue;
                        }
                        consecutive_same = 0;
                        checked.fetch_add(1, Ordering::SeqCst);

                        // Full match only when level matches (fingerprint now uses level texts)
                        let level_matches = level == target.level;
                        let do_full = level_matches;

                        let (matched, details) = if do_full {
                            match full_match_from_panel_verbose(&panel_img, target, ocr, mappings) {
                                Ok((verdict, details)) => {
                                    match verdict {
                                        MatchVerdict::Match => {
                                            log_debug!(
                                                "[grid_async] 匹配成功 page={} row={} col={}",
                                                "MATCH at page={} row={} col={}", page, row, col);
                                            *match_result.lock().unwrap() = Some((row, col));
                                            found.store(true, Ordering::SeqCst);
                                            (Some(true), details)
                                        }
                                        MatchVerdict::CleanReject => {
                                            // OCR succeeded, just a different artifact — no retry needed
                                            (Some(false), details)
                                        }
                                        MatchVerdict::DirtyReject => {
                                            // OCR/solver failed — retry with fresh capture
                                            retries.lock().unwrap().push((row, col));
                                            (Some(false), details)
                                        }
                                    }
                                }
                                Err(e) => {
                                    log_debug!("[grid_async] match error ({},{}): {}", "[grid_async] match error ({},{}): {}", row, col, e);
                                    if level_matches {
                                        retries.lock().unwrap().push((row, col));
                                    }
                                    (None, format!("error: {}", e))
                                }
                            }
                        } else {
                            (None, format!("level only: OCR='{}' parsed={} target={}", level_text, level, target.level))
                        };

                        if collect_debug {
                            debugs.lock().unwrap().push(GridCellDebug {
                                page, row, col,
                                level_text: level_text.clone(),
                                level,
                                full_ocr: do_full,
                                match_result: matched,
                                ocr_details: details,
                                panel_image: panel_img,
                            });
                        }
                    }
                });

                // Main thread: click cells and capture panel images
                for row in 0..SEL_ROWS {
                    for col in 0..SEL_COLS {
                        if found_flag.load(Ordering::SeqCst) {
                            break;
                        }

                        let x = SEL_FIRST_X + col as f64 * SEL_OFFSET_X;
                        let y = SEL_FIRST_Y + row as f64 * SEL_OFFSET_Y;
                        ctrl.click_at(x, y);
                        yas::utils::sleep(SEL_GRID_CELL_DELAY_MS);

                        match ctrl.capture_region(SEL_PANEL_X, SEL_PANEL_Y, SEL_PANEL_W, SEL_PANEL_H) {
                            Ok(img) => {
                                let _ = tx.send((row, col, img));
                            }
                            Err(e) => {
                                log_debug!("[grid_scan] capture failed ({},{}): {}", "[grid_scan] capture failed ({},{}): {}", row, col, e);
                            }
                        }
                    }
                    if found_flag.load(Ordering::SeqCst) {
                        break;
                    }
                }
                drop(tx); // Signal OCR thread: no more captures

                // Scope exit joins the OCR thread automatically
            });
        }

        total_checked += cells_checked.load(Ordering::SeqCst);

        // Collect debug info from this page
        if let Some(ref mut out) = debug_out {
            match Arc::try_unwrap(cell_debugs) {
                Ok(mutex) => out.extend(mutex.into_inner().unwrap_or_default()),
                Err(arc) => out.extend(arc.lock().unwrap_or_else(|e| e.into_inner()).drain(..)),
            }
        }

        // Check match
        if let Some((row, col)) = *match_cell.lock().unwrap() {
            log_debug!("[grid_scan] 已找到 page={} ({},{})，共检查{}", "found at page={} ({},{}) after {} checked", page, row, col, total_checked);

            // Re-click the matched cell to select it
            let x = SEL_FIRST_X + col as f64 * SEL_OFFSET_X;
            let y = SEL_FIRST_Y + row as f64 * SEL_OFFSET_Y;
            ctrl.click_at(x, y);
            yas::utils::sleep(d_cell() * 2);

            if equip {
                if !click_equip_button_safe(ctrl, ocr, "grid_scan", page, row, col)? {
                    return Ok(true); // already equipped, treated as success
                }
            }
            return Ok(true);
        }

        // --- Retry: re-click cells where level matched but solver/match failed ---
        // Background floating objects can corrupt OCR; a fresh capture may succeed.
        let retries_vec = match Arc::try_unwrap(retry_cells) {
            Ok(mutex) => mutex.into_inner().unwrap_or_default(),
            Err(arc) => arc.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        if !retries_vec.is_empty() {
            log_debug!("[grid_scan] 重试第{}页的{}个格子", "retrying on page {}, {} cells", page, retries_vec.len());
            for (row, col) in &retries_vec {
                if ctrl.check_rmb() {
                    bail!("{}", ctrl.cancel_token().reason().unwrap());
                }
                let x = SEL_FIRST_X + *col as f64 * SEL_OFFSET_X;
                let y = SEL_FIRST_Y + *row as f64 * SEL_OFFSET_Y;
                ctrl.click_at(x, y);
                yas::utils::sleep(d_cell() * 2); // longer wait for fresh background frame

                match ctrl.capture_region(SEL_PANEL_X, SEL_PANEL_Y, SEL_PANEL_W, SEL_PANEL_H) {
                    Ok(panel_img) => {
                        // Dump per-field binarized crops if enabled
                        if dump_images {
                            dump_panel_crops(&panel_img, page, *row, *col);
                        }

                        match full_match_from_panel_verbose(&panel_img, target, ocr, mappings) {
                            Ok((MatchVerdict::Match, details)) => {
                                log_debug!(
                                    "[grid_scan] 重试匹配成功 page={} ({},{}):\n{}",
                                    "RETRY MATCH at page={} ({},{}):\n{}", page, row, col, details);
                                if equip {
                                    if !click_equip_button_safe(ctrl, ocr, "grid_retry", page, *row, *col)? {
                                        return Ok(true); // already equipped
                                    }
                                }
                                return Ok(true);
                            }
                            Ok((_, details)) => {
                                log_debug!("[grid_scan] 重试失败 ({},{}): {}", "retry failed ({},{}): {}", row, col, details);
                                if let Some(ref mut out) = debug_out {
                                    out.push(GridCellDebug {
                                        page, row: *row, col: *col,
                                        level_text: String::from("(retry)"),
                                        level: target.level,
                                        full_ocr: true,
                                        match_result: Some(false),
                                        ocr_details: format!("RETRY: {}", details),
                                        panel_image: panel_img,
                                    });
                                }
                            }
                            Err(e) => {
                                log_debug!("[grid_scan] retry error ({},{}): {}", "[grid_scan] retry error ({},{}): {}", row, col, e);
                            }
                        }
                    }
                    Err(e) => {
                        log_debug!(
                            "[grid_scan] retry capture failed ({},{}): {}",
                            "[grid_scan] retry capture failed ({},{}): {}", row, col, e);
                    }
                }
            }
        }

        // Check end of list (empty cells)
        if page_ended_flag.load(Ordering::SeqCst) {
            log_debug!("[grid_scan] 列表结束（空格子），共检查{}", "page ended (empty cells) after {} checked", total_checked);
            return Ok(false);
        }

        // Retry-count-based loop detection: if the same number of retries
        // repeats for 2+ consecutive pages, we're stuck on the same page
        // (scrolling had no effect because there aren't enough items).
        let cur_retry_count = retries_vec.len();
        if page > 0 {
            if Some(cur_retry_count) == prev_retry_count && cur_retry_count > 0 {
                same_retry_count += 1;
                if same_retry_count >= 2 {
                    log_debug!(
                        "[grid_scan] 连续{}页重试数相同（{}），列表结束",
                        "{} consecutive pages with same retry count ({}), end of list", same_retry_count + 1, cur_retry_count);
                    return Ok(false);
                }
            } else {
                same_retry_count = 0;
            }
        }
        prev_retry_count = Some(cur_retry_count);

        // Fingerprint-based end-of-list detection
        let fp = page_fingerprint.lock().unwrap().clone();
        if page > 0 && fp == prev_fingerprint {
            same_fp_count += 1;
            if same_fp_count >= 2 {
                log_debug!("[grid_scan] 指纹连续{}次未变化，列表结束", "fingerprint unchanged {} times, end of list", same_fp_count);
                return Ok(false);
            }
        } else {
            same_fp_count = 0;
        }
        prev_fingerprint = fp;

        // Scroll to next page
        let cx = SEL_FIRST_X + 1.5 * SEL_OFFSET_X;
        let cy = SEL_FIRST_Y + 1.5 * SEL_OFFSET_Y;
        ctrl.click_at(cx, cy);
        yas::utils::sleep(d_cell());
        ctrl.move_to(cx, cy);
        yas::utils::sleep(50);
        scroll_ticks_dir(ctrl, SEL_SCROLL_TICKS, 1); // DOWN (positive = down)
    }

    log_debug!("[grid_scan] 未找到，共检查{}", "not found after {} checked", total_checked);
    Ok(false)
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
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    _scaler: &CoordScaler,
    mappings: &MappingManager,
) -> Result<bool> {
    // Apply set filter to narrow down the grid
    let filter_applied = apply_set_filter(ctrl, &target.set_key, mappings, ocr)?;
    if filter_applied {
        click_slot_tab(ctrl, &target.slot_key)?;
        log_info!("[selection] 已应用套装筛选", "set filter applied, scanning filtered grid");
    } else {
        log_info!("[selection] 未应用套装筛选", "set filter not applied, scanning full grid");
    }

    find_artifact_in_grid(ctrl, target, ocr, mappings, true)
}

/// Full-match the selection view detail panel against a target artifact.
///
/// Checks all identity fields: level, main stat key, all substat keys + values
/// (including unactivated), and set name. Substat values allow tolerance
/// (0.15 for percentage stats, 0.6 for flat stats) to handle OCR rounding.
///
/// Returns `Ok(true)` if all fields match.
fn full_match_detail_panel(
    ctrl: &GenshinGameController,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
    tag: &str,
) -> Result<bool> {
    // 1. Level — skip rejection if OCR failed (-1), since caller may have already verified
    let level_text = ocr_region_enhanced(ctrl, ocr, SEL_LEVEL_RECT).unwrap_or_default();
    let level = parse_level(&level_text);
    if level >= 0 && level != target.level {
        log_debug!("[{}] level不匹配: OCR={} 期望={}", "[{}] level mismatch: OCR={} expected={}", tag, level, target.level);
        return Ok(false);
    }

    // 2. Main stat key (slot-aware: flower=hp, plume=atk, others use fixup)
    let main_text = ocr_region_enhanced(ctrl, ocr, SEL_MAIN_STAT_RECT).unwrap_or_default();
    if !main_text.is_empty() {
        let fixup_key = |raw_key: &str| -> String {
            match target.slot_key.as_str() {
                "flower" => "hp".to_string(),
                "plume" => "atk".to_string(),
                _ => stat_parser::main_stat_key_fixup(raw_key),
            }
        };
        if let Some(parsed) = stat_parser::parse_stat_from_text(&main_text) {
            let ocr_key = fixup_key(&parsed.key);
            if ocr_key != target.main_stat_key {
                log_debug!(
                    "[{}] 主词条不匹配: OCR='{}' -> '{}' 期望='{}'",
                    "[{}] main stat mismatch: OCR='{}' -> '{}' expected='{}'", tag, main_text, ocr_key, target.main_stat_key);
                return Ok(false);
            }
        } else {
            // Try extracting just the key
            if let Some((key, _has_pct, _)) = stat_parser::try_extract_stat_key(&main_text) {
                let ocr_key = fixup_key(&key);
                if ocr_key != target.main_stat_key {
                    log_debug!("[{}] 主词条key不匹配: '{}' vs '{}'", "[{}] main stat key mismatch: '{}' vs '{}'", tag, ocr_key, target.main_stat_key);
                    return Ok(false);
                }
            }
            // If we can't parse the main stat at all, skip this check rather than
            // rejecting (OCR might have failed on this region).
        }
    }

    // 3. All substats (active + unactivated)
    let sub_rects = SEL_SUB_RECTS;
    let mut ocr_stats: Vec<(String, f64, bool)> = Vec::new(); // (key, value, inactive)

    for (idx, rect) in sub_rects.iter().enumerate() {
        let text = ocr_region_enhanced(ctrl, ocr, *rect).unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        if let Some(parsed) = stat_parser::parse_stat_from_text(&text) {
            log_debug!(
                "[{}] 副词条{} OCR='{}' -> key='{}' val={} inactive={}",
                "[{}] substat {} OCR='{}' -> key='{}' val={} inactive={}", tag, idx, text, parsed.key, parsed.value, parsed.inactive);
            ocr_stats.push((parsed.key, parsed.value, parsed.inactive));
        } else {
            log_debug!("[{}] 副词条{} OCR='{}' 解析失败", "[{}] substat {} OCR='{}' parse failed", tag, idx, text);
        }
    }

    // Build combined target substats: active + unactivated
    let mut all_target_subs: Vec<(&GoodSubStat, bool)> = Vec::new();
    for sub in &target.substats {
        all_target_subs.push((sub, false));
    }
    for sub in &target.unactivated_substats {
        all_target_subs.push((sub, true));
    }

    // Match: every target substat must have a corresponding OCR substat with
    // matching key and value within tolerance. We use greedy 1:1 matching.
    let mut ocr_used = vec![false; ocr_stats.len()];
    let mut match_count = 0;

    for (target_sub, target_inactive) in &all_target_subs {
        let mut found = false;
        for (i, (ocr_key, ocr_val, ocr_inactive)) in ocr_stats.iter().enumerate() {
            if ocr_used[i] {
                continue;
            }
            if ocr_key != &target_sub.key {
                continue;
            }
            // Check inactive flag consistency
            if *ocr_inactive != *target_inactive {
                continue;
            }
            // Check value with tolerance
            if (ocr_val - target_sub.value).abs() <= VALUE_TOLERANCE {
                ocr_used[i] = true;
                found = true;
                match_count += 1;
                break;
            }
        }
        if !found {
            log_debug!(
                "[{}] 副词条不匹配: key='{}' val={} inactive={}",
                "[{}] substat not matched: key='{}' val={} inactive={}", tag, target_sub.key, target_sub.value, target_inactive);
            return Ok(false);
        }
    }

    // Also check that we don't have MORE OCR substats than target expects
    // (would indicate a different artifact with extra substats)
    if ocr_stats.len() > all_target_subs.len() + 1 {
        // Allow +1 tolerance for OCR noise (e.g., set bonus text parsed as a sub)
        log_debug!(
            "[{}] OCR副词条过多: {} vs 期望{}",
            "[{}] too many OCR substats: {} vs expected {}", tag, ocr_stats.len(), all_target_subs.len());
        return Ok(false);
    }

    // 4. Set name — adjust Y based on how many subs were actually parsed
    let missing_subs = 4_usize.saturating_sub(ocr_stats.len());
    let set_rect = (
        SEL_SET_NAME_RECT.0,
        SEL_SET_NAME_RECT.1 - (missing_subs as f64 * SEL_SUB_SPACING),
        SEL_SET_NAME_RECT.2,
        SEL_SET_NAME_RECT.3,
    );
    let set_text = ocr_region_enhanced(ctrl, ocr, set_rect).unwrap_or_default();
    if !set_text.is_empty() {
        // Strip trailing punctuation (e.g., "风起之日：")
        let cleaned = set_text.trim()
            .trim_end_matches('：').trim_end_matches(':')
            .trim_end_matches('；').trim_end_matches(';')
            .trim();
        if let Some(ocr_set_key) = fuzzy_match_map(cleaned, &mappings.artifact_set_map) {
            if ocr_set_key != target.set_key {
                log_debug!(
                    "[{}] 套装不匹配: OCR='{}' -> '{}' 期望='{}'",
                    "[{}] set mismatch: OCR='{}' -> '{}' expected='{}'", tag, set_text, ocr_set_key, target.set_key);
                return Ok(false);
            }
        }
        // If fuzzy_match_map returns None, skip the set check (OCR too garbled).
        // The combination of level + main stat + all substats is already very unique.
    }

    log_info!(
        "[{}] 全匹配成功: lv={}, main={}, {}个副词条, set={}",
        "[{}] full match OK: lv={}, main={}, {} substats, set={}", tag, target.level, target.main_stat_key, match_count, target.set_key);
    Ok(true)
}

/// Get the value tolerance for a stat key.
///
/// Parse a level string like "+20" or "20" into an i32.
/// Handles OCR noise like "+0、", "+20.", "+0：" by stripping non-digit chars.
fn parse_level(text: &str) -> i32 {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() { -1 } else { digits.parse::<i32>().unwrap_or(-1) }
}

/// Crop a sub-region from a captured panel image for OCR.
///
/// The panel image was captured at `(SEL_PANEL_X, SEL_PANEL_Y, SEL_PANEL_W, SEL_PANEL_H)`.
/// Sub-region coordinates are in the same 1920x1080 base as the panel rect.
/// No binarization — PaddleOCR handles the semi-transparent background directly.
fn crop_from_panel(
    panel: &RgbImage,
    sub_rect: (f64, f64, f64, f64),
) -> RgbImage {
    let scale_x = panel.width() as f64 / SEL_PANEL_W;
    let scale_y = panel.height() as f64 / SEL_PANEL_H;
    let x = ((sub_rect.0 - SEL_PANEL_X) * scale_x).round().max(0.0) as u32;
    let y = ((sub_rect.1 - SEL_PANEL_Y) * scale_y).round().max(0.0) as u32;
    let w = (sub_rect.2 * scale_x).round() as u32;
    let h = (sub_rect.3 * scale_y).round() as u32;

    // Clamp to panel bounds
    let x = x.min(panel.width().saturating_sub(1));
    let y = y.min(panel.height().saturating_sub(1));
    let w = w.min(panel.width() - x);
    let h = h.min(panel.height() - y);

    let cropped = image::imageops::crop_imm(panel, x, y, w, h).to_image();
    min_channel_preprocess(&cropped)
}

/// Min-channel preprocessing: converts each pixel to grayscale using the
/// minimum of R, G, B channels, then expands to RGB.
///
/// This dramatically improves OCR on bright animated backgrounds (Cryo/Anemo):
/// - White text has high values in ALL channels → min is high → bright
/// - Blue/cyan background has low R channel → min is low → dark
/// Result: white-on-dark-gray, much better contrast for PaddleOCR.
fn min_channel_preprocess(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y);
            let min_val = p[0].min(p[1]).min(p[2]);
            out.put_pixel(x, y, image::Rgb([min_val, min_val, min_val]));
        }
    }
    out
}

/// Save binarized OCR crops from a panel image during grid scan retry.
/// Check the action button text ("装备" vs "卸下") and click only if it says "装备".
/// Returns Ok(true) if equip was clicked, Ok(false) if already equipped (卸下).
/// Bails with debug image if OCR cannot read either text.
fn click_equip_button_safe(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    tag: &str,
    page: usize,
    row: usize,
    col: usize,
) -> Result<bool> {
    click_equip_button_safe_at(ctrl, ocr, tag, Some((page, row, col)))
}

fn click_equip_button_safe_at(
    ctrl: &mut GenshinGameController,
    ocr: &dyn ImageToText<RgbImage>,
    tag: &str,
    grid_pos: Option<(usize, usize, usize)>,
) -> Result<bool> {
    let btn_text = ctrl.ocr_region(ocr, SEL_ACTION_BUTTON_RECT).unwrap_or_default();
    let btn_clean: String = btn_text.chars().filter(|c| !c.is_whitespace()).collect();
    let pos_msg = grid_pos
        .map(|(page, row, col)| format!(" at p{}_r{}_c{}", page, row, col))
        .unwrap_or_default();

    if btn_clean.contains("卸") {
        log_debug!(
            "[{}] 按钮='{}' (卸下){}，已装备",
            "[{}] button='{}' (unequip){}, already equipped",
            tag,
            btn_clean,
            pos_msg
        );
        return Ok(false);
    }

    if btn_clean.contains("装") || btn_clean.contains("替") {
        ctrl.click_at(SEL_ACTION_BUTTON_X, SEL_ACTION_BUTTON_Y);
        yas::utils::sleep(d_action());
        // Confirm dialog if artifact is on another character ("替换" case)
        ctrl.click_at(SEL_CONFIRM_BUTTON_X, SEL_CONFIRM_BUTTON_Y);
        yas::utils::sleep(d_action() * 5 / 8);
        return Ok(true);
    }

    // Neither detected — save debug image and bail
    log_warn!(
        "[{}] 按钮OCR='{}'{}，预期为「装备」或「卸下」",
        "[{}] button OCR='{}'{}, expected '装备' or '卸下'",
        tag,
        btn_clean,
        pos_msg
    );
    let dir = std::path::Path::new("debug_images/grid_scan");
    let _ = std::fs::create_dir_all(dir);
    let suffix = grid_pos
        .map(|(page, row, col)| format!("p{}_r{}_c{}", page, row, col))
        .unwrap_or_else(|| "selected".to_string());
    if let Ok(btn_img) = ctrl.capture_region(
        SEL_ACTION_BUTTON_RECT.0, SEL_ACTION_BUTTON_RECT.1,
        SEL_ACTION_BUTTON_RECT.2, SEL_ACTION_BUTTON_RECT.3,
    ) {
        let _ = btn_img.save(dir.join(format!("btn_{}.png", suffix)));
    }
    if let Ok(full) = ctrl.capture_game() {
        let _ = full.save(dir.join(format!("btn_full_{}.png", suffix)));
    }
    bail!("button OCR failed: '{}', debug images saved", btn_clean)
}

/// Filenames use `p{page}_r{row}_c{col}_{field}.png` to correlate with
/// log lines like `[grid_scan] retry failed (row,col): ...`.
fn dump_panel_crops(panel: &RgbImage, page: usize, row: usize, col: usize) {
    let dir = std::path::Path::new("debug_images/grid_scan");
    let _ = std::fs::create_dir_all(dir);
    let prefix = format!("p{}_r{}_c{}", page, row, col);

    let fields: &[(&str, (f64, f64, f64, f64))] = &[
        ("level", SEL_LEVEL_RECT),
        ("main", SEL_MAIN_STAT_RECT),
        ("sub0", SEL_SUB_RECTS[0]),
        ("sub1", SEL_SUB_RECTS[1]),
        ("sub2", SEL_SUB_RECTS[2]),
        ("sub3", SEL_SUB_RECTS[3]),
        ("set", SEL_SET_NAME_RECT),
    ];

    for (name, rect) in fields {
        let binarized = crop_from_panel(panel, *rect);
        let _ = binarized.save(dir.join(format!("{}_{}.png", prefix, name)));
    }
}

/// Detect artifact rarity from star pixel colors in the selection view panel.
/// Returns 5, 4, or 3.
fn detect_sel_rarity(panel: &RgbImage) -> i32 {
    let is_star = |pos: (f64, f64)| -> bool {
        let scale_x = panel.width() as f64 / SEL_PANEL_W;
        let scale_y = panel.height() as f64 / SEL_PANEL_H;
        let px = ((pos.0 - SEL_PANEL_X) * scale_x).round() as u32;
        let py = ((pos.1 - SEL_PANEL_Y) * scale_y).round() as u32;
        if px < panel.width() && py < panel.height() {
            let p = panel.get_pixel(px, py);
            p[0] > 150 && p[1] > 100 && p[2] < 100 // star-yellow
        } else {
            false
        }
    };
    if is_star(SEL_STAR5_POS) { 5 }
    else if is_star(SEL_STAR4_POS) { 4 }
    else { 3 }
}

/// Full-match a pre-captured panel image against a target artifact.
///
/// Uses solver to validate substats, adjusts set name Y based on sub count.
/// Ignores elixirCrafted and location (not relevant for selection view).
/// Returns `(is_match, details_string)`.
/// Result of a panel match attempt.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MatchVerdict {
    /// All fields matched — this is the target artifact.
    Match,
    /// OCR and solver succeeded, but fields differ — definitely not the target.
    /// No retry needed.
    CleanReject,
    /// OCR or solver failed — might be the target with corrupted OCR.
    /// Retry with a fresh capture may help.
    DirtyReject,
}

fn full_match_from_panel_verbose(
    panel: &RgbImage,
    target: &GoodArtifact,
    ocr: &dyn ImageToText<RgbImage>,
    mappings: &MappingManager,
) -> Result<(MatchVerdict, String)> {
    let mut details = String::new();
    use std::fmt::Write;

    // 1. Rarity (pixel check)
    let rarity = detect_sel_rarity(panel);
    let _ = writeln!(details, "rarity: detected={} target={}", rarity, target.rarity);
    if rarity != target.rarity {
        let _ = writeln!(details, "=> REJECT: rarity mismatch");
        return Ok((MatchVerdict::CleanReject, details));
    }

    // 2. Level
    let level_img = crop_from_panel(panel, SEL_LEVEL_RECT);
    let level_text = ocr.image_to_text(&level_img, false).unwrap_or_default();
    let level_text = level_text.trim().to_string();
    let level = parse_level(&level_text);
    let _ = writeln!(details, "level: OCR='{}' parsed={} target={}", level_text, level, target.level);
    if level >= 0 && level != target.level {
        let _ = writeln!(details, "=> REJECT: level mismatch");
        return Ok((MatchVerdict::CleanReject, details));
    }

    // 3. Main stat (slot-aware: flower=hp, plume=atk, others use fixup)
    let main_img = crop_from_panel(panel, SEL_MAIN_STAT_RECT);
    let main_text = ocr.image_to_text(&main_img, false).unwrap_or_default();
    let main_text = main_text.trim().to_string();
    let _ = write!(details, "main: OCR='{}' ", main_text);
    let fixup_main = |raw_key: &str| -> String {
        match target.slot_key.as_str() {
            "flower" => "hp".to_string(),
            "plume" => "atk".to_string(),
            _ => stat_parser::main_stat_key_fixup(raw_key),
        }
    };
    if !main_text.is_empty() {
        if let Some(parsed) = stat_parser::parse_stat_from_text(&main_text) {
            let ocr_key = fixup_main(&parsed.key);
            let _ = writeln!(details, "=> key='{}' target='{}'", ocr_key, target.main_stat_key);
            if ocr_key != target.main_stat_key {
                let _ = writeln!(details, "=> REJECT: main stat mismatch");
                return Ok((MatchVerdict::CleanReject, details));
            }
        } else if let Some((key, _has_pct, _)) = stat_parser::try_extract_stat_key(&main_text) {
            let ocr_key = fixup_main(&key);
            let _ = writeln!(details, "=> key='{}' target='{}'", ocr_key, target.main_stat_key);
            if ocr_key != target.main_stat_key {
                let _ = writeln!(details, "=> REJECT: main stat key mismatch");
                return Ok((MatchVerdict::CleanReject, details));
            }
        } else {
            let _ = writeln!(details, "=> (parse failed, skipping check)");
        }
    } else {
        let _ = writeln!(details, "=> (empty)");
    }

    // 4. Substats — OCR all 4 lines, build solver candidates
    let mut sub_candidates: Vec<Vec<OcrCandidate>> = Vec::new();
    let mut parsed_sub_count: usize = 0;

    for (idx, rect) in SEL_SUB_RECTS.iter().enumerate() {
        let img = crop_from_panel(panel, *rect);
        let text = ocr.image_to_text(&img, false).unwrap_or_default();
        let text = text.trim().to_string();
        if text.is_empty() {
            let _ = writeln!(details, "sub{}: (empty, stopping)", idx);
            break;
        }
        // Check for "2件套" stop marker (set bonus text, not a substat)
        if text.contains("件套") {
            let _ = writeln!(details, "sub{}: OCR='{}' => stop marker", idx, text);
            break;
        }
        // Selection view OCR sometimes reads decimal point as colon;
        // normalize here (not in shared stat_parser) to avoid side effects
        // on inventory scanner's unactivated stat detection.
        let text = text.replace(':', ".");
        if let Some(parsed) = stat_parser::parse_stat_from_text(&text) {
            // Truncate to game precision — OCR noise can add trailing
            // digits (e.g. "22.0%" read as "22.09"). Game values are
            // exactly 1 decimal for percent stats, integer for flat stats.
            // Truncate (floor toward zero) instead of rounding to avoid
            // noise digit pushing the value to the next tenth.
            let value = if parsed.key.ends_with('_') {
                (parsed.value * 10.0).trunc() / 10.0
            } else {
                parsed.value.trunc()
            };
            let _ = writeln!(details, "sub{}: OCR='{}' => key='{}' val={} inactive={}",
                idx, text, parsed.key, value, parsed.inactive);
            sub_candidates.push(vec![OcrCandidate {
                key: parsed.key,
                value,
                inactive: parsed.inactive,
            }]);
            parsed_sub_count += 1;
        } else {
            // Parse failed — likely set name bleeding into sub line; stop here
            let _ = writeln!(details, "sub{}: OCR='{}' => (parse failed, stopping)", idx, text);
            break;
        }
    }

    // 5. Solver validation
    let effective_level = if level >= 0 { level } else { target.level };
    let solver_input = SolverInput {
        rarity,
        level_candidates: vec![effective_level],
        substat_candidates: sub_candidates,
    };
    let solver_result = roll_solver::solve(&solver_input);
    let solved_subs = match &solver_result {
        Some(result) => {
            let _ = writeln!(details, "solver: OK, total_rolls={} init={}",
                result.total_rolls, result.initial_substat_count);
            &result.substats
        }
        None => {
            let _ = writeln!(details, "solver: FAILED (substats not mechanically valid)");
            let _ = writeln!(details, "=> REJECT: solver failed");
            return Ok((MatchVerdict::DirtyReject, details));
        }
    };

    // 6. Match solved substats against target
    let mut all_target_subs: Vec<(&GoodSubStat, bool)> = Vec::new();
    for sub in &target.substats { all_target_subs.push((sub, false)); }
    for sub in &target.unactivated_substats { all_target_subs.push((sub, true)); }

    let _ = write!(details, "target subs: ");
    for (sub, inactive) in &all_target_subs {
        let _ = write!(details, "{}={}{} ", sub.key, sub.value, if *inactive { "(inactive)" } else { "" });
    }
    let _ = writeln!(details);

    // Greedy 1:1 matching using solver-validated values
    let mut solved_used = vec![false; solved_subs.len()];
    for (target_sub, target_inactive) in &all_target_subs {
        let mut found = false;
        for (i, solved) in solved_subs.iter().enumerate() {
            if solved_used[i] { continue; }
            if solved.key != target_sub.key { continue; }
            if solved.inactive != *target_inactive { continue; }
            if (solved.value - target_sub.value).abs() <= VALUE_TOLERANCE {
                solved_used[i] = true;
                found = true;
                break;
            }
        }
        if !found {
            let _ = writeln!(details, "=> REJECT: substat key='{}' val={} not matched",
                target_sub.key, target_sub.value);
            return Ok((MatchVerdict::CleanReject, details));
        }
    }

    // 7. Set name — adjust Y based on actual sub count
    let missing_subs = 4_usize.saturating_sub(parsed_sub_count);
    let set_rect = (
        SEL_SET_NAME_RECT.0,
        SEL_SET_NAME_RECT.1 - (missing_subs as f64 * SEL_SUB_SPACING),
        SEL_SET_NAME_RECT.2,
        SEL_SET_NAME_RECT.3,
    );
    let set_img = crop_from_panel(panel, set_rect);
    let set_text = ocr.image_to_text(&set_img, false).unwrap_or_default();
    let set_text = set_text.trim().to_string();
    let _ = write!(details, "set: OCR='{}' (y_adj=-{}) ", set_text, missing_subs as f64 * SEL_SUB_SPACING);
    if !set_text.is_empty() {
        let cleaned = set_text
            .trim_end_matches('：').trim_end_matches(':')
            .trim_end_matches('；').trim_end_matches(';')
            .trim();
        if let Some(ocr_set_key) = fuzzy_match_map(cleaned, &mappings.artifact_set_map) {
            let _ = writeln!(details, "=> '{}' target='{}'", ocr_set_key, target.set_key);
            if ocr_set_key != target.set_key {
                let _ = writeln!(details, "=> REJECT: set mismatch");
                return Ok((MatchVerdict::CleanReject, details));
            }
        } else {
            let _ = writeln!(details, "=> (no match, skipping)");
        }
    } else {
        let _ = writeln!(details, "=> (empty)");
    }

    let _ = writeln!(details, "=> MATCH: lv={} main={} subs={}/{} set={}",
        target.level, target.main_stat_key, solved_subs.len(), all_target_subs.len(), target.set_key);
    Ok((MatchVerdict::Match, details))
}

/// Verify that the expected artifact is now equipped in the given slot.
///
/// Currently a no-op that returns Ok(true) — trusts that the click succeeded.
/// Can be enhanced later with re-click verification if reliability issues arise.
pub fn verify_artifact_equipped(
    _ctrl: &GenshinGameController,
    _slot_key: &str,
    _target: &GoodArtifact,
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

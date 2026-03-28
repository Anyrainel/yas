# Artifact Manager — UI Placeholder Implementation Guide

This document contains everything needed to implement the placeholder functions in `genshin/src/manager/ui_actions.rs` and complete the lock toggle pass in `genshin/src/manager/lock_manager.rs`. These placeholders require access to a running Genshin Impact client at 1920x1080 resolution for coordinate calibration.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [How the Manager Works](#how-the-manager-works)
3. [Controller API Reference](#controller-api-reference)
4. [Coordinate System](#coordinate-system)
5. [Existing Constants](#existing-constants)
6. [Placeholder Functions](#placeholder-functions)
7. [Lock Manager Pass 2 Completion](#lock-manager-pass-2-completion)
8. [Testing](#testing)
9. [File Map](#file-map)

---

## Architecture Overview

```
Web Frontend (browser)
    |  POST /manage  (JSON instructions)
    v
HTTP Server (tiny_http, 127.0.0.1:port)
    |
    v
ArtifactManager (orchestrator.rs)
    |-- Phase 1: LockManager  (iterate backpack, match & toggle locks)
    '-- Phase 2: EquipManager (navigate character screens, equip/unequip)
            |
            v
      GenshinGameController + BackpackScanner + OCR (shared with scanner)
```

The manager is at `genshin/src/manager/` (sibling to `scanner/`, not nested). It shares game interaction logic from `scanner/common/`.

## How the Manager Works

### Phase 1 — Lock Changes (`lock_manager.rs`)

**Two-pass architecture** (required because `BackpackScanner::scan_grid` borrows `ctrl` mutably):

- **Pass 1** (implemented): Opens artifact backpack, iterates every artifact via `BackpackScanner::scan_grid`. For each item, captures the detail panel screenshot, runs `GoodArtifactScanner::identify_artifact()` for OCR identification, then matches against pending lock instructions via `matching::find_best_match()`. Records `PendingToggle` entries (with `grid_index` and `y_shift`) for artifacts that need lock state changes. Also stores all scanned artifacts for Phase 2 reuse.

- **Pass 2** (needs work): After `scan_grid` releases the mutable borrow on `ctrl`, navigates back to each matched artifact's grid position and calls `click_lock_button()`. **Currently just calls `click_lock_button()` without re-navigating to the correct grid position** — see [Lock Manager Pass 2 Completion](#lock-manager-pass-2-completion).

### Phase 2 — Equip Changes (`equip_manager.rs`)

Groups instructions by target character. For each character:

1. `open_character_screen(ctrl, char_key, mappings)` — navigate from world to the character's detail screen
2. `click_equipment_slot(ctrl, slot_key)` — click the artifact slot (flower/plume/sands/goblet/circlet)
3. `find_and_click_artifact_in_selection(ctrl, target, ocr, scaler, mappings)` — scan the selection list grid, find the target artifact via OCR + matching, click it to equip

For unequip (`location = ""`): navigates to the **current owner** character (from Phase 1 scan data), clicks the slot, then `click_unequip_button()`.

The game auto-swaps artifacts when equipping to a new character, so no explicit unequip is needed for equip operations.

### Orchestrator (`orchestrator.rs`)

Runs Phase 1, returns to main UI (`ctrl.return_to_main_ui(4)`), then runs Phase 2. Handles abort detection (`yas::utils::was_aborted()`) between phases.

## Controller API Reference

All methods are on `GenshinGameController` (`genshin/src/scanner/common/game_controller.rs`).

### Navigation

| Method | Description |
|--------|-------------|
| `ctrl.click_at(base_x, base_y)` | Move mouse + 20ms delay + left click. Coordinates are 1920x1080 base. |
| `ctrl.move_to(base_x, base_y)` | Move mouse without clicking. |
| `ctrl.key_press(enigo::Key::Layout('c'))` | Press a keyboard key. Use `Layout('c')` for letter keys, `Escape` for Esc, `Return` for Enter. |
| `ctrl.mouse_scroll(amount)` | Scroll wheel. Positive = down. |
| `ctrl.focus_game_window()` | Bring game window to foreground via Win32 API. |
| `ctrl.return_to_main_ui(max_attempts)` | Press Escape up to N times, verifies main world via Paimon icon brightness. Returns `bool`. |
| `ctrl.is_likely_main_world()` | Check if game is in main world (Paimon icon brightness check). |

### Capture & OCR

| Method | Description |
|--------|-------------|
| `ctrl.capture_game()` | Capture full game window as `RgbImage`. Returns `Result<RgbImage>`. |
| `ctrl.capture_region(x, y, w, h)` | Capture sub-region. All coords in 1920x1080 base. |
| `ctrl.ocr_region(model, (x, y, w, h))` | Capture region + OCR, returns trimmed `String`. |
| `ctrl.ocr_region_shifted(model, rect, y_shift)` | Same but with Y offset (for elixir artifacts). |
| `ctrl.wait_until_panel_loaded(pool_rect, timeout_ms)` | Reactive wait for panel transition by monitoring pixel sum changes. `pool_rect` is `(x, y, w, h)` in base coords. |
| `ctrl.get_flag_color(x, y)` | Get single pixel color at base coords. Returns `Result<Rgb<u8>>`. |

### Pixel Detection Helpers (`scanner/common/pixel_utils.rs`)

| Function | Description |
|----------|-------------|
| `pixel_utils::detect_artifact_lock(image, scaler, y_shift)` | Returns `bool` — true if locked. Checks pixels at (1683,428) and (1708,428) with y_shift. |
| `pixel_utils::detect_artifact_rarity(image, scaler)` | Returns rarity (3/4/5) from star pixel colors. |
| `pixel_utils::is_star_yellow(image, scaler, x, y)` | Check single pixel for star yellow (R>150, G>100, B<100). |
| `pixel_utils::is_pixel_dark(image, scaler, x, y)` | Check if pixel brightness < 128. |

### Utilities

| Function | Description |
|----------|-------------|
| `yas::utils::sleep(ms)` | Thread sleep in milliseconds. Takes `u32`. |
| `yas::utils::is_rmb_down()` | Check if right mouse button is pressed (user abort). |
| `yas::utils::was_aborted()` | Check if abort was triggered at any point. |

### BackpackScanner (`scanner/common/backpack_scanner.rs`)

| Method | Description |
|--------|-------------|
| `BackpackScanner::new(ctrl)` | Creates scanner, borrows `ctrl` mutably. |
| `scanner.open_backpack(delay_ms)` | Press B to open backpack. |
| `scanner.select_tab("artifact", delay_ms)` | Click artifact tab. |
| `scanner.read_item_count(ocr_model)` | OCR the "X/Y" item count. Returns `Result<(i32, i32)>`. |
| `scanner.scaler()` | Access the controller's `CoordScaler`. |
| `scanner.scan_grid(total, config, start_at, needs_retry, callback)` | Iterate grid items. `needs_retry` is `FnMut(&RgbImage) -> bool` for two-phase icon detection (e.g., `pixel_utils::is_artifact_icon_ambiguous`). Callback receives `GridEvent::Item(index, image)` or `GridEvent::PageScrolled`. Return `ScanAction::Continue` or `ScanAction::Stop`. |

### OCR Model Creation

```rust
use crate::scanner::common::ocr_factory;
let ocr_model = ocr_factory::create_ocr_model("ppocrv5")?;  // For levels
let substat_model = ocr_factory::create_ocr_model("ppocrv4")?;  // For everything else
```

### Artifact Identification

```rust
use crate::scanner::artifact::GoodArtifactScanner;
let result = GoodArtifactScanner::identify_artifact(
    ocr_model.as_ref(),       // &dyn ImageToText<RgbImage>
    substat_ocr.as_ref(),     // &dyn ImageToText<RgbImage>
    &captured_image,           // &RgbImage (full game capture)
    &scaler,                   // &CoordScaler
    &mappings,                 // &MappingManager
)?;
// Returns Option<GoodArtifact> — None for <=3-star artifacts
```

## Coordinate System

All coordinates use **1920x1080 as base resolution**. The `CoordScaler` automatically scales them to the actual game window size at runtime. When calibrating, take screenshots at 1920x1080 and read pixel coordinates directly.

The game window origin (0,0) is the top-left corner of the game client area.

## Existing Constants

From `genshin/src/scanner/common/constants.rs`:

```rust
// Backpack grid layout
GRID_COLS = 8;
GRID_ROWS = 5;           // 5 visible rows per page
GRID_FIRST_X = 180.0;    // First item center X
GRID_FIRST_Y = 253.0;    // First item center Y
GRID_OFFSET_X = 145.0;   // Horizontal spacing between items
GRID_OFFSET_Y = 166.0;   // Vertical spacing between items

// Scroll
SCROLL_TICKS_PER_PAGE = 49;        // Mouse wheel ticks per 5-row page
SCROLL_CORRECTION_INTERVAL = 3;    // Subtract 1 tick every N pages

// Artifact detail panel positions
ARTIFACT_LOCK_POS1 = (1683.0, 428.0);   // Lock icon pixel check position 1
ARTIFACT_LOCK_POS2 = (1708.0, 428.0);   // Lock icon pixel check position 2
ARTIFACT_EQUIP_RECT = (1357.0, 999.0, 419.0, 50.0);  // "Equipped by X" OCR region

// Character screen
CHAR_NEXT_POS = (1845.0, 525.0);   // "Next character" button
CHAR_NAME_RECT = (128.0, 18.0, 330.0, 60.0);  // Character name OCR region

// Backpack tabs
TAB_WEAPON = (585.0, 50.0);
TAB_ARTIFACT = (675.0, 50.0);

// Panel pool rect (for wait_until_panel_loaded)
PANEL_POOL_RECT = (1400.0, 300.0, 300.0, 200.0);  // In backpack_scanner.rs

// Elixir
ELIXIR_SHIFT = 40.0;  // Purple banner pushes content down by 40px
```

## Placeholder Functions

### 1. `click_lock_button` — Difficulty: Easy

**File:** `genshin/src/manager/ui_actions.rs`

**What it does:** Clicks the lock/unlock toggle on the artifact detail panel (the right-side panel shown when an artifact is selected in the backpack).

**Calibration:**
1. Open artifact backpack at 1920x1080
2. Click on a **locked** artifact so its detail panel shows
3. Screenshot — find the lock icon's **clickable center** coordinates
4. The detection pixels are at (1683, 428) and (1708, 428) — the clickable center is likely near there but may differ
5. Test clicking at those coordinates to see if the lock toggles
6. Test on an **elixir** artifact (purple banner) — lock icon shifts down by `y_shift` (40px)

**Implementation:**
```rust
pub fn click_lock_button(ctrl: &mut GenshinGameController, y_shift: f64) -> Result<()> {
    // Replace with calibrated coordinates
    const LOCK_CLICK_X: f64 = 1683.0;  // calibrate!
    const LOCK_CLICK_Y: f64 = 428.0;   // calibrate!
    ctrl.click_at(LOCK_CLICK_X, LOCK_CLICK_Y + y_shift);
    yas::utils::sleep(300); // wait for lock animation
    Ok(())
}
```

**Verification:** After clicking, capture the screen and call `pixel_utils::detect_artifact_lock(image, scaler, y_shift)` to confirm the lock state changed.

---

### 2. `click_equipment_slot` — Difficulty: Easy

**What it does:** On the character detail screen, clicks one of the 5 artifact slot icons to open the artifact selection list for that slot.

**Calibration:**
1. Open any character's detail screen at 1920x1080
2. Screenshot — identify the **center** of each of the 5 artifact slot icons
3. The 5 slots follow GOOD naming: `flower`, `plume`, `sands`, `goblet`, `circlet`

**Implementation:**
```rust
pub fn click_equipment_slot(
    ctrl: &mut GenshinGameController,
    slot_key: &str,
) -> Result<()> {
    let (x, y) = match slot_key {
        "flower"  => (TODO_X, TODO_Y),  // calibrate each!
        "plume"   => (TODO_X, TODO_Y),
        "sands"   => (TODO_X, TODO_Y),
        "goblet"  => (TODO_X, TODO_Y),
        "circlet" => (TODO_X, TODO_Y),
        _ => bail!("Unknown slot: {}", slot_key),
    };
    ctrl.click_at(x, y);
    yas::utils::sleep(500); // wait for selection list to open
    Ok(())
}
```

---

### 3. `click_unequip_button` — Difficulty: Easy

**What it does:** Clicks the "取下" (unequip/remove) button to remove an artifact from a character's slot.

**Calibration:**
1. Open character detail, click an equipped artifact slot
2. Look for a "取下" button in the artifact detail or at the bottom of the selection panel
3. Screenshot — find the button center coordinates

**Implementation:**
```rust
pub fn click_unequip_button(ctrl: &mut GenshinGameController) -> Result<()> {
    const UNEQUIP_X: f64 = TODO;  // calibrate!
    const UNEQUIP_Y: f64 = TODO;
    ctrl.click_at(UNEQUIP_X, UNEQUIP_Y);
    yas::utils::sleep(500);
    Ok(())
}
```

---

### 4. `open_character_screen` — Difficulty: Medium

**What it does:** From the main world, navigates to a specific character's detail screen.

**Context:** The character scanner already does this. It presses `C` to open the roster, then uses `CHAR_NEXT_POS = (1845.0, 525.0)` to cycle through characters. Character names are OCR'd at `CHAR_NAME_RECT = (128.0, 18.0, 330.0, 60.0)`.

`char_key` is a GOOD format key like `"Furina"` or `"Nahida"`. The `mappings.character_name_map` maps Chinese names to GOOD keys. You need to reverse-lookup: find the Chinese name for the given GOOD key.

**Approach A — Sequential cycling (simpler, slower):**
```rust
pub fn open_character_screen(
    ctrl: &mut GenshinGameController,
    char_key: &str,
    mappings: &MappingManager,
) -> Result<()> {
    // Reverse-lookup: GOOD key -> Chinese name
    let cn_name = mappings.character_name_map.iter()
        .find(|(_, v)| v.as_str() == char_key)
        .map(|(k, _)| k.clone());

    // Need an OCR model for character name reading
    let ocr = ocr_factory::create_ocr_model("ppocrv4")?;

    // Press C to open roster
    ctrl.key_press(enigo::Key::Layout('c'));
    yas::utils::sleep(1500);

    let max_chars = 80; // safety limit
    for _ in 0..max_chars {
        if yas::utils::is_rmb_down() {
            bail!("User aborted");
        }

        // OCR character name at CHAR_NAME_RECT
        let name_text = ctrl.ocr_region(ocr.as_ref(), CHAR_NAME_RECT)?;

        // Check if name matches (either GOOD key or Chinese name)
        // The OCR returns Chinese text, so match against cn_name
        if let Some(ref cn) = cn_name {
            if name_text.contains(cn.as_str()) {
                return Ok(());
            }
        }
        // Also try fuzzy matching against character_name_map
        if let Some(matched_key) = mappings.character_name_map.get(&name_text) {
            if matched_key == char_key {
                return Ok(());
            }
        }

        // Click next
        ctrl.click_at(CHAR_NEXT_POS.0, CHAR_NEXT_POS.1);
        yas::utils::sleep(300);
    }
    bail!("Character {} not found after cycling through roster", char_key)
}
```

**Approach B — Roster list with direct click (faster, harder):**
The left sidebar shows character avatars. If you can identify the grid positions and either icon-match or OCR name labels, you can click directly. This requires discovering the sidebar layout coordinates.

**Important:** The character scanner in `genshin/src/scanner/character/scanner.rs` already has code for cycling characters. Look at how it uses `CHAR_NEXT_POS` and OCR for reference.

---

### 5. `find_and_click_artifact_in_selection` — Difficulty: Hard

**What it does:** After clicking an equipment slot (function #2), the game shows a scrollable grid of artifacts for that slot type. This function scans the grid to find and click the target artifact.

**What you need to discover via screenshots:**

1. **Grid layout of the selection list.** This is NOT the main backpack grid. It may have different dimensions, spacing, and position. Take a screenshot with the selection list open and measure:
   - How many columns and rows?
   - First item center coordinates?
   - Spacing between items?

2. **Detail panel behavior.** When you click an item in the selection list, does a detail panel appear (like the main backpack)? If so, you can reuse `GoodArtifactScanner::identify_artifact()` on the captured panel.

3. **Scroll behavior.** How many scroll ticks per page? Is there a scroll indicator?

4. **Equip action.** After clicking an artifact in the selection list:
   - Does it equip immediately?
   - Is there a confirmation dialog?
   - Is there a "swap?" dialog if another character has it?

**Implementation approach:**
```rust
pub fn find_and_click_artifact_in_selection(
    ctrl: &mut GenshinGameController,
    target: &ArtifactTarget,
    ocr: &dyn ImageToText<RgbImage>,
    scaler: &CoordScaler,
    mappings: &MappingManager,
) -> Result<bool> {
    // Create substat OCR model (v4)
    let substat_ocr = ocr_factory::create_ocr_model("ppocrv4")?;

    // Calibrated constants for the selection list grid
    const SEL_COLS: usize = TODO;
    const SEL_ROWS: usize = TODO;
    const SEL_FIRST_X: f64 = TODO;
    const SEL_FIRST_Y: f64 = TODO;
    const SEL_OFFSET_X: f64 = TODO;
    const SEL_OFFSET_Y: f64 = TODO;
    const SEL_SCROLL_TICKS: i32 = TODO;

    let max_pages = 20; // safety limit
    for _page in 0..max_pages {
        for row in 0..SEL_ROWS {
            for col in 0..SEL_COLS {
                if yas::utils::is_rmb_down() {
                    bail!("User aborted");
                }

                let x = SEL_FIRST_X + col as f64 * SEL_OFFSET_X;
                let y = SEL_FIRST_Y + row as f64 * SEL_OFFSET_Y;

                // Click item to show detail panel
                ctrl.click_at(x, y);
                ctrl.wait_until_panel_loaded(PANEL_POOL_RECT, 400)?;
                yas::utils::sleep(100);

                // Capture and identify
                let image = ctrl.capture_game()?;
                match GoodArtifactScanner::identify_artifact(
                    ocr, substat_ocr.as_ref(), &image, scaler, mappings,
                ) {
                    Ok(Some(artifact)) => {
                        if matching::match_score(&artifact, target).is_some() {
                            // Found it! The click may have already equipped it,
                            // or there may be an "equip" button to click.
                            // Calibrate this behavior.
                            return Ok(true);
                        }
                    }
                    Ok(None) => {
                        // Empty slot or low-rarity — may mean end of list
                    }
                    Err(e) => {
                        log::warn!("Selection list OCR failed: {}", e);
                    }
                }
            }
        }

        // Scroll to next page
        // Check if we've reached the end (e.g., last item was empty)
        ctrl.mouse_scroll(SEL_SCROLL_TICKS);
        yas::utils::sleep(300);
    }

    Ok(false) // Not found
}
```

**Performance:** For ~60 artifacts per slot type at ~200ms per item, this takes ~12 seconds — acceptable.

**Alternative:** If the selection list shows set icons, level badges, and rarity stars per item WITHOUT clicking, you could do a faster visual pre-filter (check rarity stars, then only OCR items that match rarity). But the click-and-OCR approach is more reliable and reuses existing code.

---

### 6. `verify_artifact_equipped` — Difficulty: Medium (optional, can defer)

**What it does:** After equipping, verifies the correct artifact is now in the slot. This is optional — you can trust the click succeeded initially and add verification later if reliability issues arise.

**Approaches:**
1. OCR the artifact name visible on the character screen
2. Click the slot again to re-open its detail panel, then use `identify_artifact`
3. Compare before/after screenshots at the slot position

---

## Lock Manager Pass 2 Completion

**File:** `genshin/src/manager/lock_manager.rs`, around line 220-250.

**Current state:** Pass 2 just calls `click_lock_button(ctrl, toggle.y_shift)` in a loop without first navigating back to the correct grid position. This won't work because after `scan_grid` completes, the cursor is at an arbitrary position.

**What needs to happen:** Before each `click_lock_button()` call, navigate back to the artifact at `toggle.grid_index` in the backpack grid.

### Approach A — Re-open backpack and navigate to grid positions

```rust
// After scan_grid completes and BackpackScanner is dropped:

if !pending_toggles.is_empty() {
    // Sort by grid_index for sequential scrolling
    pending_toggles.sort_by_key(|t| t.grid_index);

    // Re-open backpack
    let mut nav_scanner = BackpackScanner::new(ctrl);
    nav_scanner.open_backpack(400);
    nav_scanner.select_tab("artifact", 400);
    // drop nav_scanner to release ctrl
    drop(nav_scanner);

    let items_per_page = GRID_COLS * GRID_ROWS;  // 40
    let mut current_page = 0usize;

    for toggle in &pending_toggles {
        if yas::utils::is_rmb_down() {
            results.insert(toggle.instr_id.clone(), InstructionResult {
                id: toggle.instr_id.clone(),
                status: InstructionStatus::Aborted,
                detail: Some("User aborted".to_string()),
            });
            continue;
        }

        let target_page = toggle.grid_index / items_per_page;
        let row_in_page = (toggle.grid_index % items_per_page) / GRID_COLS;
        let col = toggle.grid_index % GRID_COLS;

        // Scroll to correct page
        while current_page < target_page {
            let mut ticks = SCROLL_TICKS_PER_PAGE;
            current_page += 1;
            if current_page % SCROLL_CORRECTION_INTERVAL as usize == 0 {
                ticks -= 1;
            }
            ctrl.mouse_scroll(ticks);
            yas::utils::sleep(300);
        }

        // Click the grid position
        let x = GRID_FIRST_X + col as f64 * GRID_OFFSET_X;
        let y = GRID_FIRST_Y + row_in_page as f64 * GRID_OFFSET_Y;
        ctrl.click_at(x, y);
        ctrl.wait_until_panel_loaded(PANEL_POOL_RECT, 400)?;
        yas::utils::sleep(100); // wait for lock icon animation

        // Toggle lock
        click_lock_button(ctrl, toggle.y_shift)?;

        // Verify
        yas::utils::sleep(200);
        let image = ctrl.capture_game()?;
        let new_lock = pixel_utils::detect_artifact_lock(&image, &scaler, toggle.y_shift);
        let desired = toggle.desired_lock;

        if new_lock == desired {
            results.insert(toggle.instr_id.clone(), InstructionResult {
                id: toggle.instr_id.clone(),
                status: InstructionStatus::Success,
                detail: None,
            });
        } else {
            results.insert(toggle.instr_id.clone(), InstructionResult {
                id: toggle.instr_id.clone(),
                status: InstructionStatus::UiError,
                detail: Some("Lock toggle verification failed".to_string()),
            });
        }
    }
}
```

### Approach B — Simpler: re-scan with early stop

Instead of calculating scroll positions, do a second `scan_grid` pass but only process items at known `grid_index` positions. This avoids scroll math:

```rust
let toggle_indices: HashSet<usize> = pending_toggles.iter()
    .map(|t| t.grid_index).collect();
let toggle_map: HashMap<usize, &PendingToggle> = pending_toggles.iter()
    .map(|t| (t.grid_index, t)).collect();

let mut scanner2 = BackpackScanner::new(ctrl);
scanner2.open_backpack(400);
scanner2.select_tab("artifact", 400);
// ... scan_grid again, but in the callback only act on toggle_indices
```

This is simpler but slower (re-scans the whole backpack). Approach A is recommended.

### Important Notes

- The `PendingToggle` struct fields `grid_index` and `desired_lock` currently have `#[allow(dead_code)]` — remove those annotations when you use them.
- You'll need to add imports: `use super::ui_actions`, `use crate::scanner::common::constants::*`, `use crate::scanner::common::pixel_utils`.
- The `PANEL_POOL_RECT` constant is defined in `backpack_scanner.rs` as `(1400.0, 300.0, 300.0, 200.0)` — you may need to make it `pub` or redefine it in `lock_manager.rs`.

## Testing

### Start the server

```sh
# Build
cargo build --release

# Run in server mode (requires admin, game must be running)
target/release/GOODScanner.exe --server --port 8765
```

### Test lock toggle

```sh
curl -X POST http://127.0.0.1:8765/manage \
  -H "Content-Type: application/json" \
  -d '{
    "instructions": [{
      "id": "test-lock-1",
      "target": {
        "setKey": "GladiatorsFinale",
        "slotKey": "flower",
        "rarity": 5,
        "level": 20,
        "mainStatKey": "hp",
        "substats": [
          {"key": "critRate_", "value": 3.9},
          {"key": "critDMG_", "value": 7.8}
        ]
      },
      "changes": {"lock": true}
    }]
  }'
```

### Test equip

```sh
curl -X POST http://127.0.0.1:8765/manage \
  -H "Content-Type: application/json" \
  -d '{
    "instructions": [{
      "id": "test-equip-1",
      "target": {
        "setKey": "GladiatorsFinale",
        "slotKey": "flower",
        "rarity": 5,
        "level": 20,
        "mainStatKey": "hp",
        "substats": []
      },
      "changes": {"location": "Furina"}
    }]
  }'
```

### Test health check

```sh
curl http://127.0.0.1:8765/health
# {"status":"ok"}
```

### Expected behavior with placeholders

With unimplemented placeholders, lock instructions will return:
- `"status": "ui_error"` with detail mentioning "placeholder"
- Artifacts that already have the correct lock state will return `"status": "already_correct"`

Equip instructions will all return `"status": "ui_error"` with placeholder messages.

### Implementation order (recommended)

1. **`click_lock_button`** + **Lock Manager Pass 2** — gets lock toggling working end-to-end
2. **`click_equipment_slot`** — easy, fixed coordinates
3. **`click_unequip_button`** — easy, one coordinate
4. **`open_character_screen`** — medium, character cycling logic
5. **`find_and_click_artifact_in_selection`** — hardest, new grid discovery
6. **`verify_artifact_equipped`** — optional, can defer

## File Map

| File | Purpose |
|------|---------|
| `genshin/src/manager/mod.rs` | Module root, re-exports all sub-modules |
| `genshin/src/manager/models.rs` | Input/output data models (serde JSON) |
| `genshin/src/manager/matching.rs` | Artifact identity matching with scoring |
| `genshin/src/manager/ui_actions.rs` | **Placeholder functions to implement** |
| `genshin/src/manager/lock_manager.rs` | Phase 1: backpack scan + lock toggle (**Pass 2 needs completion**) |
| `genshin/src/manager/equip_manager.rs` | Phase 2: character navigation + equip/unequip |
| `genshin/src/manager/orchestrator.rs` | Top-level coordinator for both phases |
| `genshin/src/server.rs` | HTTP server (tiny_http, POST /manage, GET /health) |
| `genshin/src/cli.rs` | CLI entry point with `--server` / `--port` flags |
| `genshin/src/scanner/common/game_controller.rs` | Game controller (click, key, capture, OCR) |
| `genshin/src/scanner/common/backpack_scanner.rs` | Grid-based inventory navigation |
| `genshin/src/scanner/common/constants.rs` | All calibrated UI coordinates |
| `genshin/src/scanner/common/pixel_utils.rs` | Lock/rarity/elixir pixel detection |
| `genshin/src/scanner/common/mappings.rs` | Character/weapon/artifact name mappings |
| `genshin/src/scanner/common/ocr_factory.rs` | OCR backend creation (ppocrv4/v5) |
| `genshin/src/scanner/artifact/scanner.rs` | `GoodArtifactScanner::identify_artifact()` |

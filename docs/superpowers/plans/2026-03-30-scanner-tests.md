# Scanner Per-Item Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add unit tests for the per-item scan functions (`scan_single_artifact`, `scan_single_weapon`) and pixel detection utilities, using mock OCR and synthetic images.

**Architecture:** Create a shared `FakeOcr` (queue-based `ImageToText` mock) and synthetic image builder in a `#[cfg(test)]` test utilities module. Use these to test `scan_single_artifact` (already public) and `scan_single_weapon` (needs `pub(crate)` visibility). Also add pixel_utils tests with synthetic images. No changes to scan logic — only visibility modifiers and new test code.

**Tech Stack:** Rust standard test framework, `image` crate for synthetic `RgbImage` construction, existing `yas::ocr::ImageToText` trait.

---

### Task 1: Shared test utilities — FakeOcr + image builder

**Files:**
- Create: `genshin/src/scanner/common/test_utils.rs`
- Modify: `genshin/src/scanner/common/mod.rs`

- [ ] **Step 1: Create the test_utils module**

Create `genshin/src/scanner/common/test_utils.rs`:

```rust
//! Shared test utilities for scanner unit tests.
//!
//! Provides FakeOcr (queue-based ImageToText mock) and synthetic image builders.

use std::collections::HashMap;
use std::sync::Mutex;
use std::collections::VecDeque;

use anyhow::Result;
use image::RgbImage;

use yas::ocr::ImageToText;

use super::coord_scaler::CoordScaler;
use super::mappings::{ConstBonus, MappingManager};

// ============================================================
// FakeOcr — queue-based ImageToText mock
// ============================================================

/// Mock OCR that returns pre-programmed strings in FIFO order.
///
/// Each call to `image_to_text` pops the next response from the queue.
/// Panics if the queue is exhausted (test misconfiguration).
pub struct FakeOcr {
    responses: Mutex<VecDeque<Result<String>>>,
    call_count: Mutex<usize>,
}

impl FakeOcr {
    /// Create a FakeOcr with a list of successful OCR results.
    pub fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(
                responses.into_iter().map(|s| Ok(s.to_string())).collect(),
            ),
            call_count: Mutex::new(0),
        }
    }

    /// Create a FakeOcr where some calls return errors.
    pub fn with_results(responses: Vec<Result<String>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            call_count: Mutex::new(0),
        }
    }

    /// How many times image_to_text has been called.
    pub fn call_count(&self) -> usize {
        *self.call_count.lock().unwrap()
    }
}

impl ImageToText<RgbImage> for FakeOcr {
    fn image_to_text(&self, _image: &RgbImage, _is_preprocessed: bool) -> Result<String> {
        let mut count = self.call_count.lock().unwrap();
        *count += 1;
        let call_num = *count;
        drop(count);

        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("FakeOcr: no response queued for call #{}", call_num))
    }

    fn get_average_inference_time(&self) -> Option<std::time::Duration> {
        None
    }
}

// ============================================================
// Synthetic image builder
// ============================================================

/// Create a blank (black) 1920x1080 test image.
pub fn make_1080p_image() -> RgbImage {
    RgbImage::new(1920, 1080)
}

/// Create a CoordScaler for 1920x1080 (identity scale).
pub fn make_1080p_scaler() -> CoordScaler {
    CoordScaler::new(1920, 1080)
}

/// Set a single pixel on the image.
pub fn set_pixel(image: &mut RgbImage, x: u32, y: u32, rgb: [u8; 3]) {
    if x < image.width() && y < image.height() {
        image.put_pixel(x, y, image::Rgb(rgb));
    }
}

/// Paint star-yellow pixels for a given rarity.
///
/// Sets yellow pixels (R=200, G=180, B=50) across the star row at y=372.
/// For rarity 5: fills x=1350..=1490
/// For rarity 4: fills x=1350..=1455
/// For rarity 3: fills x=1350..=1420
pub fn paint_rarity_stars(image: &mut RgbImage, rarity: i32) {
    let star_y = 372u32;
    let max_x: u32 = match rarity {
        5 => 1490,
        4 => 1455,
        3 => 1420,
        _ => 1380,
    };
    let yellow: [u8; 3] = [200, 180, 50];
    // Paint a small band (±2 pixels in y) for robustness
    for dy in 0..=4 {
        let y = star_y - 2 + dy;
        for x in (1350..=max_x).step_by(2) {
            set_pixel(image, x, y, yellow);
        }
    }
}

/// Paint the lock icon as "present" (dark) at the artifact lock position.
/// Artifact lock pos1: (1683, 428), pos2: (1708, 428)
pub fn paint_artifact_lock(image: &mut RgbImage, locked: bool, y_shift: f64) {
    let dark: [u8; 3] = [60, 60, 60];   // brightness ~60 < 116 (ICON_BRIGHT_PRESENT)
    let light: [u8; 3] = [230, 230, 230]; // brightness ~230 > 208 (ICON_BRIGHT_ABSENT)
    let color = if locked { dark } else { light };
    let y = (428.0 + y_shift) as u32;
    set_pixel(image, 1683, y, color);
    set_pixel(image, 1708, y, color);
}

/// Paint the astral mark as "present" or "absent" at artifact astral position.
/// Artifact astral pos1: (1768, 428), pos2: (1740, 429)
pub fn paint_artifact_astral(image: &mut RgbImage, present: bool, y_shift: f64) {
    let dark: [u8; 3] = [60, 60, 60];
    let light: [u8; 3] = [230, 230, 230];
    let color = if present { dark } else { light };
    let y = (428.0 + y_shift) as u32;
    set_pixel(image, 1768, y, color);
    set_pixel(image, 1740, y + 1, color);
}

/// Paint elixir purple banner pixels at (1510-1530, 423).
pub fn paint_elixir_banner(image: &mut RgbImage, is_elixir: bool) {
    let purple: [u8; 3] = [80, 50, 240]; // blue > 230, blue > green + 40
    let beige: [u8; 3] = [200, 190, 195]; // similar channels, not purple
    let color = if is_elixir { purple } else { beige };
    for x in [1510u32, 1520, 1530] {
        set_pixel(image, x, 423, color);
    }
}

/// Paint weapon lock icon. Pos1: (1768, 428), Pos2: (1740, 429)
pub fn paint_weapon_lock(image: &mut RgbImage, locked: bool) {
    let dark: [u8; 3] = [60, 60, 60];
    let light: [u8; 3] = [230, 230, 230];
    let color = if locked { dark } else { light };
    set_pixel(image, 1768, 428, color);
    set_pixel(image, 1740, 429, color);
}

// ============================================================
// Test MappingManager builder
// ============================================================

/// Create a minimal MappingManager with just enough data for tests.
pub fn make_test_mappings() -> MappingManager {
    let mut char_map = HashMap::new();
    char_map.insert("芙宁娜".to_string(), "Furina".to_string());
    char_map.insert("纳西妲".to_string(), "Nahida".to_string());
    char_map.insert("胡桃".to_string(), "HuTao".to_string());

    let mut weapon_map = HashMap::new();
    weapon_map.insert("天空之翼".to_string(), "SkywardHarp".to_string());
    weapon_map.insert("护摩之杖".to_string(), "StaffOfHoma".to_string());
    weapon_map.insert("风鹰剑".to_string(), "AquilaFavonia".to_string());

    let mut set_map = HashMap::new();
    set_map.insert("角斗士的终幕礼".to_string(), "GladiatorsFinale".to_string());
    set_map.insert("流浪大地的乐团".to_string(), "WanderersTroupe".to_string());
    set_map.insert("绝缘之旗印".to_string(), "EmblemOfSeveredFate".to_string());

    let mut max_rarity = HashMap::new();
    max_rarity.insert("GladiatorsFinale".to_string(), 5);
    max_rarity.insert("WanderersTroupe".to_string(), 5);
    max_rarity.insert("EmblemOfSeveredFate".to_string(), 5);

    MappingManager {
        character_name_map: char_map,
        character_const_bonus: HashMap::new(),
        weapon_name_map: weapon_map,
        artifact_set_map: set_map,
        artifact_set_max_rarity: max_rarity,
    }
}
```

- [ ] **Step 2: Register the module**

In `genshin/src/scanner/common/mod.rs`, add:

```rust
#[cfg(test)]
pub(crate) mod test_utils;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo test --package yas_scanner_genshin --lib scanner::common::test_utils -- --test-threads=1 2>&1 | head -5`
Expected: Compiles with 0 tests (no test functions yet).

- [ ] **Step 4: Commit**

```bash
git add genshin/src/scanner/common/test_utils.rs genshin/src/scanner/common/mod.rs
git commit -m "test: add shared FakeOcr and synthetic image builder for scanner tests"
```

---

### Task 2: Weapon scanner visibility changes

**Files:**
- Modify: `genshin/src/scanner/weapon/scanner.rs`

The weapon scanner's per-item function and types are private. Make them `pub(crate)` so tests in other modules (or inline tests) can access them.

- [ ] **Step 1: Change visibility of WeaponScanResult**

In `genshin/src/scanner/weapon/scanner.rs`, change:

```rust
enum WeaponScanResult {
```

to:

```rust
pub(crate) enum WeaponScanResult {
```

- [ ] **Step 2: Change visibility of WeaponOcrRegions**

Change:

```rust
struct WeaponOcrRegions {
```

to:

```rust
pub(crate) struct WeaponOcrRegions {
```

And change its constructor:

```rust
    fn new() -> Self {
```

to:

```rust
    pub(crate) fn new() -> Self {
```

- [ ] **Step 3: Change visibility of scan_single_weapon**

Change:

```rust
    fn scan_single_weapon(
```

to:

```rust
    pub(crate) fn scan_single_weapon(
```

- [ ] **Step 4: Verify existing code still compiles**

Run: `cargo build --package yas_scanner_genshin 2>&1 | tail -3`
Expected: Compiles successfully with no errors.

- [ ] **Step 5: Commit**

```bash
git add genshin/src/scanner/weapon/scanner.rs
git commit -m "refactor: make weapon per-item scan function pub(crate) for testability"
```

---

### Task 3: Pixel utils tests

**Files:**
- Modify: `genshin/src/scanner/common/pixel_utils.rs`

Test rarity detection, lock detection, astral mark detection, and elixir detection with synthetic images.

- [ ] **Step 1: Add test module to pixel_utils.rs**

Append to the end of `genshin/src/scanner/common/pixel_utils.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::common::test_utils::*;

    #[test]
    fn test_artifact_rarity_5_star() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        let scaler = make_1080p_scaler();
        assert_eq!(detect_artifact_rarity(&image, &scaler), 5);
    }

    #[test]
    fn test_artifact_rarity_4_star() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 4);
        let scaler = make_1080p_scaler();
        assert_eq!(detect_artifact_rarity(&image, &scaler), 4);
    }

    #[test]
    fn test_artifact_rarity_3_star() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 3);
        let scaler = make_1080p_scaler();
        assert_eq!(detect_artifact_rarity(&image, &scaler), 3);
    }

    #[test]
    fn test_artifact_rarity_blank_image() {
        let image = make_1080p_image();
        let scaler = make_1080p_scaler();
        // No star pixels → rarity 2 (fallback)
        assert_eq!(detect_artifact_rarity(&image, &scaler), 2);
    }

    #[test]
    fn test_weapon_rarity_5_star() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        let scaler = make_1080p_scaler();
        assert_eq!(detect_weapon_rarity(&image, &scaler), 5);
    }

    #[test]
    fn test_artifact_lock_detected() {
        let mut image = make_1080p_image();
        paint_artifact_lock(&mut image, true, 0.0);
        let scaler = make_1080p_scaler();
        assert!(detect_artifact_lock(&image, &scaler, 0.0));
    }

    #[test]
    fn test_artifact_unlock_detected() {
        let mut image = make_1080p_image();
        paint_artifact_lock(&mut image, false, 0.0);
        let scaler = make_1080p_scaler();
        assert!(!detect_artifact_lock(&image, &scaler, 0.0));
    }

    #[test]
    fn test_artifact_lock_with_elixir_shift() {
        let mut image = make_1080p_image();
        paint_artifact_lock(&mut image, true, 40.0);
        let scaler = make_1080p_scaler();
        // Without shift → should NOT detect lock (pixels are at shifted position)
        assert!(!detect_artifact_lock(&image, &scaler, 0.0));
        // With correct shift → should detect lock
        assert!(detect_artifact_lock(&image, &scaler, 40.0));
    }

    #[test]
    fn test_artifact_astral_mark_detected() {
        let mut image = make_1080p_image();
        paint_artifact_astral(&mut image, true, 0.0);
        let scaler = make_1080p_scaler();
        assert!(detect_artifact_astral_mark(&image, &scaler, 0.0));
    }

    #[test]
    fn test_artifact_astral_mark_absent() {
        let mut image = make_1080p_image();
        paint_artifact_astral(&mut image, false, 0.0);
        let scaler = make_1080p_scaler();
        assert!(!detect_artifact_astral_mark(&image, &scaler, 0.0));
    }

    #[test]
    fn test_weapon_lock_detected() {
        let mut image = make_1080p_image();
        paint_weapon_lock(&mut image, true);
        let scaler = make_1080p_scaler();
        assert!(detect_weapon_lock(&image, &scaler));
    }

    #[test]
    fn test_weapon_unlock_detected() {
        let mut image = make_1080p_image();
        paint_weapon_lock(&mut image, false);
        let scaler = make_1080p_scaler();
        assert!(!detect_weapon_lock(&image, &scaler));
    }

    #[test]
    fn test_icon_ambiguous_mid_animation() {
        let mut image = make_1080p_image();
        let scaler = make_1080p_scaler();
        // Set lock pixel to mid-range brightness (ambiguous)
        let mid: [u8; 3] = [150, 150, 150]; // brightness 150, between 116 and 208
        set_pixel(&mut image, 1683, 428, mid);
        assert!(is_artifact_icon_ambiguous(&image, &scaler));
    }

    #[test]
    fn test_icon_not_ambiguous_when_clearly_locked() {
        let mut image = make_1080p_image();
        let scaler = make_1080p_scaler();
        // Set both lock and astral to clearly dark (present)
        let dark: [u8; 3] = [60, 60, 60];
        set_pixel(&mut image, 1683, 428, dark); // lock pos1
        set_pixel(&mut image, 1768, 428, dark); // astral pos1
        assert!(!is_artifact_icon_ambiguous(&image, &scaler));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --package yas_scanner_genshin --lib scanner::common::pixel_utils::tests -- --test-threads=1`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```bash
git add genshin/src/scanner/common/pixel_utils.rs
git commit -m "test: pixel detection tests with synthetic images"
```

---

### Task 4: Weapon scanner per-item tests

**Files:**
- Modify: `genshin/src/scanner/weapon/scanner.rs`

Test `scan_single_weapon` with FakeOcr and synthetic images. The weapon scanner makes OCR calls in this order on the primary `ocr` engine:
1. Name
2. Level
3. Refinement
4. Equip

And optionally one call on `equip_fallback_ocr` if the primary equip parse fails.

- [ ] **Step 1: Add test module to weapon scanner**

Append to the end of `genshin/src/scanner/weapon/scanner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::common::test_utils::*;

    fn default_config() -> GoodWeaponScannerConfig {
        GoodWeaponScannerConfig {
            verbose: false,
            dump_images: false,
            continue_on_failure: false,
            ..Default::default()
        }
    }

    /// Build a synthetic 1080p image with 5-star rarity pixels and lock set.
    fn make_weapon_image(rarity: i32, locked: bool) -> RgbImage {
        let mut img = make_1080p_image();
        paint_rarity_stars(&mut img, rarity);
        paint_weapon_lock(&mut img, locked);
        img
    }

    #[test]
    fn test_weapon_happy_path_5star_locked() {
        let image = make_weapon_image(5, true);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        // OCR call order: name, level, refinement, equip
        let ocr = FakeOcr::new(vec![
            "天空之翼",       // name → SkywardHarp
            "90/90",          // level → 90, ascended=false (90==max)
            "精炼1阶",        // refinement → 1
            "",               // equip → empty (not equipped)
        ]);

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, None, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            WeaponScanResult::Weapon(w) => {
                assert_eq!(w.key, "SkywardHarp");
                assert_eq!(w.level, 90);
                assert_eq!(w.refinement, 1);
                assert_eq!(w.rarity, 5);
                assert!(w.lock);
                assert!(w.location.is_empty());
            }
            other => panic!("Expected Weapon, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_weapon_with_equip_location() {
        let image = make_weapon_image(5, false);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        let ocr = FakeOcr::new(vec![
            "护摩之杖",
            "90/90",
            "精炼1阶",
            "胡桃已装备",     // equip → HuTao
        ]);

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, None, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            WeaponScanResult::Weapon(w) => {
                assert_eq!(w.key, "StaffOfHoma");
                assert_eq!(w.location, "HuTao");
                assert!(!w.lock);
            }
            other => panic!("Expected Weapon, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_weapon_low_rarity_stops() {
        // 2-star image with unmatched name → Stop
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 2);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        // Name won't match any weapon, rarity ≤ 2 → Stop
        let ocr = FakeOcr::new(vec!["something"]);

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, None, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        assert!(matches!(result, WeaponScanResult::Stop));
    }

    #[test]
    fn test_weapon_unmatched_name_continue_on_failure_skips() {
        let image = make_weapon_image(5, false);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let mut config = default_config();
        config.continue_on_failure = true;

        let ocr = FakeOcr::new(vec!["完全看不懂的名字"]); // unmatched name

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, None, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        assert!(matches!(result, WeaponScanResult::Skip));
    }

    #[test]
    fn test_weapon_unmatched_name_errors_without_continue() {
        let image = make_weapon_image(5, false);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config(); // continue_on_failure = false

        let ocr = FakeOcr::new(vec!["完全看不懂的名字"]);

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, None, &image, &scaler, &regions, &mappings, &config, 0,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_weapon_equip_fallback_to_v5() {
        let image = make_weapon_image(5, false);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        // Primary OCR: name OK, level OK, refinement OK, equip returns non-empty but non-matching text
        let ocr = FakeOcr::new(vec![
            "风鹰剑",
            "80/90",
            "精炼3阶",
            "纳两妲已装备",   // v4 garbles 纳西妲 → 纳两妲 (won't match)
        ]);
        // Fallback v5 OCR returns correct text
        let fallback = FakeOcr::new(vec!["纳西妲已装备"]);

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, Some(&fallback), &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            WeaponScanResult::Weapon(w) => {
                assert_eq!(w.key, "AquilaFavonia");
                assert_eq!(w.location, "Nahida");
                assert_eq!(w.level, 80);
                assert_eq!(w.refinement, 3);
            }
            other => panic!("Expected Weapon, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_weapon_level_ascended() {
        let image = make_weapon_image(5, false);
        let scaler = make_1080p_scaler();
        let regions = WeaponOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        // "80/80" means lv80 NOT ascended (max=80, level==max → ascended=false)
        // "80/90" means lv80 IS ascended (level < max_level → ascended=true)
        let ocr = FakeOcr::new(vec![
            "天空之翼",
            "80/90",     // ascended: 80 >= 20 && 80 < 90 → true
            "精炼1阶",
            "",
        ]);

        let result = GoodWeaponScanner::scan_single_weapon(
            &ocr, None, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            WeaponScanResult::Weapon(w) => {
                assert_eq!(w.level, 80);
                assert_eq!(w.ascension, 6); // level_to_ascension(80, true) = 6
            }
            other => panic!("Expected Weapon, got {:?}", std::mem::discriminant(&other)),
        }
    }
}
```

- [ ] **Step 2: Run weapon tests**

Run: `cargo test --package yas_scanner_genshin --lib scanner::weapon::scanner::tests -- --test-threads=1`
Expected: All tests pass. If the FakeOcr call count is wrong (OCR call order differs from expected), adjust the response queue based on the panic message which indicates which call number ran out.

- [ ] **Step 3: Commit**

```bash
git add genshin/src/scanner/weapon/scanner.rs
git commit -m "test: weapon scanner per-item tests with FakeOcr"
```

---

### Task 5: Artifact scanner per-item tests

**Files:**
- Modify: `genshin/src/scanner/artifact/scanner.rs`

The artifact scanner makes OCR calls in this order:

**substat_ocr (general/v4) calls:**
1. Part name (slot)
2. Main stat
3. Level (v4 attempt)
4. Per substat line (up to 4 lines, 1-2 calls each):
   - Direct OCR of substat region
   - If direct parse fails: masked OCR of same region
5. Set name (primary position — RGB, then grayscale if RGB fails)
6. Equip (v4 attempt)

**ocr (level/v5) calls:**
1. Level (v5 attempt) — called BEFORE the v4 attempt above

If equip v4 returns non-empty but unmatched text, there's an additional **ocr** call for equip v5 fallback.

For set name, each `try_set_ocr` call makes 1-2 substat_ocr calls (RGB first, then grayscale if no match).

- [ ] **Step 1: Add test module to artifact scanner**

Append to the end of `genshin/src/scanner/artifact/scanner.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::common::test_utils::*;

    fn default_config() -> GoodArtifactScannerConfig {
        GoodArtifactScannerConfig {
            verbose: false,
            dump_images: false,
            continue_on_failure: false,
            ..Default::default()
        }
    }

    #[test]
    fn test_artifact_low_rarity_stops() {
        // 3-star image → ArtifactScanResult::Stop before any OCR
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 3);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        // No OCR calls should be made — empty FakeOcr will panic if called
        let level_ocr = FakeOcr::new(vec![]);
        let general_ocr = FakeOcr::new(vec![]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        assert!(matches!(result, ArtifactScanResult::Stop));
        assert_eq!(level_ocr.call_count(), 0);
        assert_eq!(general_ocr.call_count(), 0);
    }

    #[test]
    fn test_artifact_unrecognizable_slot_4star_skips() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 4);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config();

        let level_ocr = FakeOcr::new(vec![]);
        // First general OCR call is part name — return garbage
        let general_ocr = FakeOcr::new(vec!["乱码无法识别"]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        // 4-star with unrecognizable slot → Skip (not error)
        assert!(matches!(result, ArtifactScanResult::Skip));
    }

    #[test]
    fn test_artifact_unrecognizable_slot_5star_errors() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let config = default_config(); // continue_on_failure = false

        let level_ocr = FakeOcr::new(vec![]);
        let general_ocr = FakeOcr::new(vec!["乱码无法识别"]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_artifact_unrecognizable_slot_5star_skips_with_continue() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let mut config = default_config();
        config.continue_on_failure = true;

        let level_ocr = FakeOcr::new(vec![]);
        let general_ocr = FakeOcr::new(vec!["乱码无法识别"]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        assert!(matches!(result, ArtifactScanResult::Skip));
    }

    #[test]
    fn test_artifact_flower_main_stat_forced_hp() {
        // For flower slot, main stat is always "hp" regardless of OCR
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        paint_artifact_lock(&mut image, true, 0.0);
        paint_artifact_astral(&mut image, false, 0.0);
        paint_elixir_banner(&mut image, false);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let mut config = default_config();
        config.continue_on_failure = true; // so set name failure doesn't error

        // level_ocr (v5): called once for level
        let level_ocr = FakeOcr::new(vec!["+20"]);

        // general_ocr (v4) call sequence:
        // 1. part name → "生之花" (flower)
        // 2. main stat → "生命值" (hp — but for flower it's forced anyway)
        // 3. level v4 → "+20"
        // 4-11. substats: 4 lines × up to 2 calls (direct + masked)
        //       For a clean parse, direct succeeds → 1 call per line
        // 12. set name (RGB) → match
        // 13. equip
        //
        // We provide enough responses; if substat parsing gets complex
        // the FakeOcr will tell us which call needs more data.
        let general_ocr = FakeOcr::new(vec![
            "生之花",                  // 1. part name
            "生命值",                  // 2. main stat
            "+20",                     // 3. level v4
            "暴击率+3.9%",             // 4. sub0 direct
            "暴击伤害+7.8%",           // 5. sub1 direct
            "攻击力+14.0%",            // 6. sub2 direct
            "元素充能效率+6.5%",       // 7. sub3 direct
            "角斗士的终幕礼",          // 8. set name RGB
            "",                        // 9. equip (empty)
        ]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            ArtifactScanResult::Artifact(a) => {
                assert_eq!(a.slot_key, "flower");
                assert_eq!(a.main_stat_key, "hp"); // forced for flower
                assert_eq!(a.level, 20);
                assert_eq!(a.rarity, 5);
                assert!(a.lock);
                assert!(!a.astral_mark);
                assert!(!a.elixir_crafted);
                assert_eq!(a.set_key, "GladiatorsFinale");
                assert!(a.location.is_empty());
                // Verify substats parsed
                assert_eq!(a.substats.len(), 4);
                assert_eq!(a.substats[0].key, "critRate_");
                assert!((a.substats[0].value - 3.9).abs() < 0.1);
                assert!(a.total_rolls.is_some());
            }
            other => panic!("Expected Artifact, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_artifact_elixir_crafted_detected() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        paint_elixir_banner(&mut image, true);
        // For elixir, lock/astral are shifted down by 40px
        paint_artifact_lock(&mut image, false, 40.0);
        paint_artifact_astral(&mut image, false, 40.0);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let mut config = default_config();
        config.continue_on_failure = true;

        let level_ocr = FakeOcr::new(vec!["+20"]);
        let general_ocr = FakeOcr::new(vec![
            "生之花",                  // part name
            "生命值",                  // main stat
            "+20",                     // level v4
            "暴击率+3.9%",             // sub0
            "暴击伤害+7.8%",           // sub1
            "攻击力+14.0%",            // sub2
            "元素充能效率+6.5%",       // sub3
            "角斗士的终幕礼",          // set name
            "",                        // equip
        ]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            ArtifactScanResult::Artifact(a) => {
                assert!(a.elixir_crafted);
                assert!(!a.lock);
            }
            other => panic!("Expected Artifact, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_artifact_astral_forces_lock_true() {
        // Game invariant: astral mark → always locked
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        paint_elixir_banner(&mut image, false);
        // Astral = true, lock = false → scanner should force lock=true
        paint_artifact_lock(&mut image, false, 0.0);
        paint_artifact_astral(&mut image, true, 0.0);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let mut config = default_config();
        config.continue_on_failure = true;

        let level_ocr = FakeOcr::new(vec!["+20"]);
        let general_ocr = FakeOcr::new(vec![
            "生之花",
            "生命值",
            "+20",
            "暴击率+3.9%",
            "暴击伤害+7.8%",
            "攻击力+14.0%",
            "元素充能效率+6.5%",
            "角斗士的终幕礼",
            "",
        ]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            ArtifactScanResult::Artifact(a) => {
                assert!(a.astral_mark);
                assert!(a.lock, "Lock should be forced true when astral is present");
            }
            other => panic!("Expected Artifact, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_artifact_equipped_to_character() {
        let mut image = make_1080p_image();
        paint_rarity_stars(&mut image, 5);
        paint_artifact_lock(&mut image, false, 0.0);
        paint_artifact_astral(&mut image, false, 0.0);
        paint_elixir_banner(&mut image, false);
        let scaler = make_1080p_scaler();
        let regions = ArtifactOcrRegions::new();
        let mappings = make_test_mappings();
        let mut config = default_config();
        config.continue_on_failure = true;

        let level_ocr = FakeOcr::new(vec!["+20"]);
        let general_ocr = FakeOcr::new(vec![
            "空之杯",                  // goblet
            "岩元素伤害加成",           // main stat
            "+20",
            "暴击率+3.9%",
            "暴击伤害+7.8%",
            "攻击力+14.0%",
            "元素充能效率+6.5%",
            "角斗士的终幕礼",
            "芙宁娜已装备",             // equip v4 → Furina
        ]);

        let result = GoodArtifactScanner::scan_single_artifact(
            &level_ocr, &general_ocr, &image, &scaler, &regions, &mappings, &config, 0,
        ).unwrap();

        match result {
            ArtifactScanResult::Artifact(a) => {
                assert_eq!(a.slot_key, "goblet");
                assert_eq!(a.location, "Furina");
            }
            other => panic!("Expected Artifact, got {:?}", std::mem::discriminant(&other)),
        }
    }
}
```

- [ ] **Step 2: Run artifact tests**

Run: `cargo test --package yas_scanner_genshin --lib scanner::artifact::scanner::tests -- --test-threads=1 --nocapture`
Expected: All tests pass. If any FakeOcr panics with "no response queued for call #N", it means the OCR call sequence differs from what was expected. Read the panic message to identify which call number needs an additional response, then add it to the queue.

**Important:** The substat OCR sequence may include masked fallback calls when the direct OCR text doesn't parse as a valid stat. If a test fails because extra OCR calls are made, add the same stat text as additional queue entries (the masked OCR on the same region should return similar text).

- [ ] **Step 3: Fix any call sequence mismatches**

If tests fail due to FakeOcr queue exhaustion, run with `--nocapture` and examine which call number panicked. Common fixes:
- Substats: if direct parse fails, a masked OCR call is added per line. Ensure each substat line has clean parseable text (e.g., "暴击率+3.9%") to avoid fallback calls.
- Set name: `try_set_ocr` tries RGB first, then grayscale. If RGB returns matching text, only 1 call. If not, 2 calls per position.
- Equip: if v4 returns non-empty but unmatched text, a v5 fallback call is made on `level_ocr`.

- [ ] **Step 4: Commit**

```bash
git add genshin/src/scanner/artifact/scanner.rs
git commit -m "test: artifact scanner per-item tests with FakeOcr"
```

---

### Task 6: Final verification

- [ ] **Step 1: Run all scanner tests together**

Run: `cargo test --package yas_scanner_genshin --lib scanner -- --test-threads=1`
Expected: All tests pass (pixel_utils, weapon, artifact, plus existing coord_scaler, fuzzy_match, stat_parser, roll_solver tests).

- [ ] **Step 2: Run full crate tests**

Run: `cargo test --package yas_scanner_genshin -- --test-threads=1`
Expected: All tests pass including server tests.

- [ ] **Step 3: Commit any final fixes**

If any tests needed adjustment, commit the fixes.

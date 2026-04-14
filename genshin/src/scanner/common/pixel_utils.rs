use image::RgbImage;
use yas::log_debug;

use super::coord_scaler::CoordScaler;

/// Check if a pixel at the given position (in base 1920x1080 coords) is "star yellow".
/// Star yellow = R > 150, G > 100, B < 100
///
/// Port of `isStarYellow()` from GOODScanner/lib/ocr_utils.js
pub fn is_star_yellow(image: &RgbImage, scaler: &CoordScaler, base_x: f64, base_y: f64) -> bool {
    let x = scaler.x(base_x) as u32;
    let y = scaler.y(base_y) as u32;

    if x >= image.width() || y >= image.height() {
        return false;
    }

    let pixel = image.get_pixel(x, y);
    let r = pixel[0];
    let g = pixel[1];
    let b = pixel[2];
    r > 150 && g > 100 && b < 100
}

// ================================================================
// Icon animation early-detection thresholds.
//
// Lock/astral icons fade in/out over ~100ms. The button background
// transitions between brightness ~86 (dark = icon present) and ~239
// (light = icon absent). We use two thresholds to detect state early
// during the animation, with a wide margin for PC speed variation:
//
//   brightness < ICON_BRIGHT_PRESENT  → icon IS present   (fast path)
//   brightness > ICON_BRIGHT_ABSENT   → icon NOT present  (fast path)
//   in between                        → ambiguous, need full delay
//
// At ~50ms into the animation, brightness is at its first step:
//   ~115 (disappearing) or ~217 (appearing), both safely outside [130,210].
// ================================================================

/// Brightness at or below which the icon is definitively present (locked / astral marked).
/// True dark value is 86, true bright value is 238 (gap=152).
/// Set at 1/5 of the range so mid-animation frames trigger a retry capture.
pub const ICON_BRIGHT_PRESENT: u32 = 116;
/// Brightness at or above which the icon is definitively absent (unlocked / no astral).
/// Set at 4/5 of the range for the same reason.
pub const ICON_BRIGHT_ABSENT: u32 = 208;

/// Get the average brightness of a pixel at base 1920x1080 coordinates.
/// Returns 0 if out of bounds.
pub fn get_pixel_brightness(image: &RgbImage, scaler: &CoordScaler, base_x: f64, base_y: f64) -> u32 {
    let x = scaler.x(base_x) as u32;
    let y = scaler.y(base_y) as u32;
    if x >= image.width() || y >= image.height() {
        return 0;
    }
    let pixel = image.get_pixel(x, y);
    (pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3
}

/// Check if any icon detection pixel is in the ambiguous brightness zone
/// (mid-animation). Used for early-capture retry: if true, the caller
/// should wait longer and re-capture.
///
/// Checks artifact lock and astral positions (without elixir y_shift,
/// which is unknown at early-capture time; elixir artifacts will simply
/// fall through to the full delay, which is correct).
///
/// Also checks for impossible semantic state: astral present but lock absent.
/// All astraled artifacts are locked in-game, so this means the lock icon
/// animation hasn't settled yet.
pub fn is_artifact_icon_ambiguous(image: &RgbImage, scaler: &CoordScaler) -> bool {
    use super::constants::{ARTIFACT_LOCK_POS1, ARTIFACT_ASTRAL_POS1};
    let lock_b = get_pixel_brightness(image, scaler, ARTIFACT_LOCK_POS1.0, ARTIFACT_LOCK_POS1.1);
    if lock_b >= ICON_BRIGHT_PRESENT && lock_b <= ICON_BRIGHT_ABSENT {
        return true;
    }
    let astral_b = get_pixel_brightness(image, scaler, ARTIFACT_ASTRAL_POS1.0, ARTIFACT_ASTRAL_POS1.1);
    if astral_b >= ICON_BRIGHT_PRESENT && astral_b <= ICON_BRIGHT_ABSENT {
        return true;
    }
    // Impossible state: astral present (dark) but lock not present.
    // All astraled artifacts must be locked — retry to let lock icon settle.
    let astral_present = astral_b <= ICON_BRIGHT_PRESENT;
    let lock_present = lock_b <= ICON_BRIGHT_PRESENT;
    if astral_present && !lock_present {
        return true;
    }
    false
}

/// Check if the weapon lock icon pixel is in the ambiguous brightness zone.
pub fn is_weapon_icon_ambiguous(image: &RgbImage, scaler: &CoordScaler) -> bool {
    use super::constants::WEAPON_LOCK_POS1;
    let b = get_pixel_brightness(image, scaler, WEAPON_LOCK_POS1.0, WEAPON_LOCK_POS1.1);
    b >= ICON_BRIGHT_PRESENT && b <= ICON_BRIGHT_ABSENT
}

/// Check if a pixel at the given position is dark (brightness < 128).
///
/// Port of `isPixelDark()` from GOODScanner/lib/ocr_utils.js
pub fn is_pixel_dark(image: &RgbImage, scaler: &CoordScaler, base_x: f64, base_y: f64) -> bool {
    let x = scaler.x(base_x) as u32;
    let y = scaler.y(base_y) as u32;

    if x >= image.width() || y >= image.height() {
        return false;
    }

    let pixel = image.get_pixel(x, y);
    let brightness = (pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3;
    brightness < 128
}

/// Dual-pixel dark icon verification.
/// Checks two pixels and returns true if the first pixel is dark.
/// Logs a warning if the two pixels disagree.
///
/// Port of `detectDarkIcon()` from GOODScanner/lib/ocr_utils.js
pub fn detect_dark_icon(
    image: &RgbImage,
    scaler: &CoordScaler,
    x1: f64, y1: f64,
    x2: f64, y2: f64,
    label: &str,
) -> bool {
    let d1 = is_pixel_dark(image, scaler, x1, y1);
    let d2 = is_pixel_dark(image, scaler, x2, y2);
    if d1 != d2 {
        log_debug!(
            "[{}] 检测不一致: ({},{})={} ({},{})={}",
            "[{}] detection inconsistent: ({},{})={} ({},{})={}",
            label, x1, y1, d1, x2, y2, d2
        );
    }
    d1
}

/// Check if the "5-star sort by acquired time" filter is active.
/// When active, the pixel at ARTIFACT_FIVE_STAR_FILTER_POS is light (not dark).
/// Uses the same brightness < 128 threshold as `is_pixel_dark`.
pub fn is_five_star_filter_active(image: &RgbImage, scaler: &CoordScaler) -> bool {
    use super::constants::ARTIFACT_FIVE_STAR_FILTER_POS;
    let bright = !is_pixel_dark(image, scaler, ARTIFACT_FIVE_STAR_FILTER_POS.0, ARTIFACT_FIVE_STAR_FILTER_POS.1);
    if bright {
        log_debug!(
            "[backpack] 检测到五星排序筛选已开启，将点击关闭",
            "[backpack] 5-star sort filter detected as active, will click to dismiss"
        );
    }
    bright
}

/// Detect weapon rarity from star pixels.
///
/// Scans the star row at y=STAR_Y. Returns 5, 4, 3, or 2.
///
/// Port from GOODScanner/lib/weapon_scanner.js rarity detection
pub fn detect_weapon_rarity(image: &RgbImage, scaler: &CoordScaler) -> i32 {
    use super::constants::STAR_Y;

    // Scan horizontal band to find rightmost star pixel
    let y_offsets: [f64; 3] = [-2.0, 0.0, 2.0];
    let mut rightmost_star_x: f64 = 0.0;
    let mut star_pixel_count = 0;

    for &dy in &y_offsets {
        let by = STAR_Y + dy;
        for bx_int in (1300..=1500).step_by(2) {
            let bx = bx_int as f64;
            let x = scaler.x(bx) as u32;
            let y = scaler.y(by) as u32;
            if x < image.width() && y < image.height() {
                let px = image.get_pixel(x, y);
                if px[0] > 150 && px[1] > 100 && px[2] < 100 {
                    star_pixel_count += 1;
                    if bx > rightmost_star_x {
                        rightmost_star_x = bx;
                    }
                }
            }
        }
    }

    if rightmost_star_x > 1470.0 {
        5
    } else if rightmost_star_x > 1430.0 {
        4
    } else if rightmost_star_x > 1400.0 {
        3
    } else if star_pixel_count > 0 {
        2
    } else {
        // Fallback to original single-pixel checks
        if is_star_yellow(image, scaler, 1485.0, STAR_Y) { 5 }
        else if is_star_yellow(image, scaler, 1450.0, STAR_Y) { 4 }
        else if is_star_yellow(image, scaler, 1416.0, STAR_Y) { 3 }
        else { 2 }
    }
}

/// Detect artifact rarity from star pixels.
///
/// Scans the star row at y=STAR_Y to count star-yellow pixels in the expected region.
/// Uses boundary x-positions to determine rarity: 5, 4, or 3.
///
/// Port from GOODScanner/lib/artifact_scanner.js rarity detection
pub fn detect_artifact_rarity(image: &RgbImage, scaler: &CoordScaler) -> i32 {
    use super::constants::STAR_Y;

    // Scan a horizontal band around STAR_Y to find yellow star pixels.
    // Stars are in the range x ≈ [1350..1500] at base 1920x1080.
    // We probe multiple y-offsets to be robust against slight vertical shifts.
    let y_offsets: [f64; 3] = [-2.0, 0.0, 2.0];

    // Find the rightmost x (in base coords) that has a star-yellow pixel
    let mut rightmost_star_x: f64 = 0.0;
    let mut star_pixel_count = 0;

    for &dy in &y_offsets {
        let by = STAR_Y + dy;
        // Scan from x=1340 to x=1500 (covers 3-star through 5-star range)
        for bx_int in (1340..=1500).step_by(2) {
            let bx = bx_int as f64;
            let x = scaler.x(bx) as u32;
            let y = scaler.y(by) as u32;
            if x < image.width() && y < image.height() {
                let px = image.get_pixel(x, y);
                if px[0] > 150 && px[1] > 100 && px[2] < 100 {
                    star_pixel_count += 1;
                    if bx > rightmost_star_x {
                        rightmost_star_x = bx;
                    }
                }
            }
        }
    }

    // Determine rarity from rightmost star position
    // 5-star: rightmost star extends past x≈1470
    // 4-star: rightmost star around x≈1440-1470
    // 3-star: any star pixels found
    let rarity = if rightmost_star_x > 1470.0 {
        5
    } else if rightmost_star_x > 1430.0 {
        4
    } else if star_pixel_count > 0 {
        3
    } else {
        // No star pixels found at all — fall back to original single-pixel check
        if is_star_yellow(image, scaler, 1485.0, STAR_Y) {
            5
        } else if is_star_yellow(image, scaler, 1450.0, STAR_Y) {
            4
        } else {
            3
        }
    };

    log_debug!("[rarity] 最右x={}, 数量={}, 结果={}星", "[rarity] rightmost_x={}, count={}, result={}*", rightmost_star_x, star_pixel_count, rarity);
    rarity
}

/// Check if an artifact's detected rarity is below the minimum threshold.
pub fn artifact_below_min_rarity(image: &RgbImage, scaler: &CoordScaler, min_rarity: i32) -> bool {
    let rarity = detect_artifact_rarity(image, scaler);
    if rarity < min_rarity {
        log_debug!(
            "[rarity] {}星 < 最低{}星，应停止",
            "[rarity] {}* < min {}*, should stop",
            rarity, min_rarity
        );
        true
    } else {
        false
    }
}

/// Check if a weapon's detected rarity is below the minimum threshold.
pub fn weapon_below_min_rarity(image: &RgbImage, scaler: &CoordScaler, min_rarity: i32) -> bool {
    let rarity = detect_weapon_rarity(image, scaler);
    if rarity < min_rarity {
        log_debug!(
            "[rarity] {}星 < 最低{}星，应停止",
            "[rarity] {}* < min {}*, should stop",
            rarity, min_rarity
        );
        true
    } else {
        false
    }
}

/// Detect if a substat line region appears dimmed (inactive/unactivated).
///
/// Active substats have bright white text (brightness > 200).
/// Inactive substats have dimmed grey text (brightness ~120-160).
/// We count the fraction of pixels above a "bright text" threshold.
/// Active lines have many bright text pixels; inactive lines have fewer.
///
/// Only samples the right 2/3 of the region to avoid the stat icon.
pub fn is_substat_dimmed(
    image: &RgbImage,
    scaler: &CoordScaler,
    rect: (f64, f64, f64, f64),
    y_shift: f64,
) -> bool {
    let (bx, by, bw, bh) = rect;
    let x = scaler.x(bx) as u32;
    let y = scaler.y(by + y_shift) as u32;
    let w = scaler.x(bw) as u32;
    let h = scaler.y(bh) as u32;

    let x = x.min(image.width().saturating_sub(1));
    let y = y.min(image.height().saturating_sub(1));
    let w = w.min(image.width().saturating_sub(x));
    let h = h.min(image.height().saturating_sub(y));

    if w == 0 || h == 0 {
        return false;
    }

    // Skip left 1/3 (icon area), sample right 2/3
    let start_x = w / 3;
    let mut bright_count: u32 = 0;
    let mut mid_count: u32 = 0;
    let mut total_count: u32 = 0;

    for py in (0..h).step_by(2) {
        for px in (start_x..w).step_by(2) {
            let p = image.get_pixel(x + px, y + py);
            let brightness = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
            total_count += 1;
            if brightness > 200 {
                bright_count += 1;
            } else if brightness > 130 {
                mid_count += 1;
            }
        }
    }

    if total_count == 0 {
        return false;
    }

    let bright_pct = bright_count * 100 / total_count;
    let mid_pct = mid_count * 100 / total_count;
    // Active substats: bright ~85-95%, mid ~2-5%
    // Inactive substats: bright ~72-80%, mid ~19-23%
    // Threshold: mid > 15% indicates dimmed/inactive text.
    // Combined with bright < 82% to avoid false positives on active lines
    // that happen to have slightly more mid-range pixels.
    mid_pct > 20 && bright_pct < 78
}

/// Detect weapon lock state via dual-pixel verification.
///
/// Port of `detectWeaponLock()` from GOODScanner/lib/ocr_utils.js
pub fn detect_weapon_lock(image: &RgbImage, scaler: &CoordScaler) -> bool {
    use super::constants::{WEAPON_LOCK_POS1, WEAPON_LOCK_POS2};
    detect_dark_icon(
        image, scaler,
        WEAPON_LOCK_POS1.0, WEAPON_LOCK_POS1.1,
        WEAPON_LOCK_POS2.0, WEAPON_LOCK_POS2.1,
        "\u{6B66}\u{5668}\u{9501}\u{5B9A}", // 武器锁定
    )
}

/// Detect artifact lock state via dual-pixel verification.
/// Supports y_shift for elixir-crafted artifacts.
///
/// Port of `detectArtifactLock()` from GOODScanner/lib/ocr_utils.js
pub fn detect_artifact_lock(image: &RgbImage, scaler: &CoordScaler, y_shift: f64) -> bool {
    use super::constants::{ARTIFACT_LOCK_POS1, ARTIFACT_LOCK_POS2};
    detect_dark_icon(
        image, scaler,
        ARTIFACT_LOCK_POS1.0, ARTIFACT_LOCK_POS1.1 + y_shift,
        ARTIFACT_LOCK_POS2.0, ARTIFACT_LOCK_POS2.1 + y_shift,
        "\u{5723}\u{9057}\u{7269}\u{9501}\u{5B9A}", // 圣遗物锁定
    )
}

/// Verify that an artifact lock toggle succeeded, using asymmetric brightness
/// thresholds that are permissive to mid-animation frames.
///
/// The lock icon has two end-states: dark (locked, brightness ~86) and bright
/// (unlocked, brightness ~238). During the toggle animation the pixel passes
/// through mid-brightness. A single threshold (<128) works for unlock→lock
/// because the pixel darkens fast, but fails for lock→unlock because the
/// brightening animation may not have crossed 128 by verify time.
///
/// Logic:
/// - `desired_lock = true`  (just locked): accept if brightness is below
///   `ICON_BRIGHT_ABSENT` (208). Any sign the pixel is darkening below the
///   definitively-bright level means the lock animation started.
/// - `desired_lock = false` (just unlocked): accept if brightness is above
///   `ICON_BRIGHT_PRESENT` (116). Any sign the pixel is brightening above
///   the definitively-dark level means the unlock animation started.
///
/// Checks both POS1 and POS2; either passing counts as verified.
pub fn verify_artifact_lock_toggled(
    image: &RgbImage,
    scaler: &CoordScaler,
    y_shift: f64,
    desired_lock: bool,
) -> bool {
    use super::constants::{ARTIFACT_LOCK_POS1, ARTIFACT_LOCK_POS2};
    let b1 = get_pixel_brightness(image, scaler, ARTIFACT_LOCK_POS1.0, ARTIFACT_LOCK_POS1.1 + y_shift);
    let b2 = get_pixel_brightness(image, scaler, ARTIFACT_LOCK_POS2.0, ARTIFACT_LOCK_POS2.1 + y_shift);
    let ok = if desired_lock {
        // Expect darkening: below "definitely bright" threshold
        b1 < ICON_BRIGHT_ABSENT || b2 < ICON_BRIGHT_ABSENT
    } else {
        // Expect brightening: above "definitely dark" threshold
        b1 > ICON_BRIGHT_PRESENT || b2 > ICON_BRIGHT_PRESENT
    };
    log_debug!(
        "[verify_lock] desired={} b1={} b2={} ok={}",
        "[verify_lock] desired={} b1={} b2={} ok={}",
        desired_lock, b1, b2, ok
    );
    ok
}

/// Detect artifact astral mark via dual-pixel verification.
/// Supports y_shift for elixir-crafted artifacts.
///
/// Port of `detectArtifactAstralMark()` from GOODScanner/lib/ocr_utils.js
pub fn detect_artifact_astral_mark(image: &RgbImage, scaler: &CoordScaler, y_shift: f64) -> bool {
    use super::constants::{ARTIFACT_ASTRAL_POS1, ARTIFACT_ASTRAL_POS2};
    detect_dark_icon(
        image, scaler,
        ARTIFACT_ASTRAL_POS1.0, ARTIFACT_ASTRAL_POS1.1 + y_shift,
        ARTIFACT_ASTRAL_POS2.0, ARTIFACT_ASTRAL_POS2.1 + y_shift,
        "\u{5723}\u{9057}\u{7269}\u{6536}\u{85CF}", // 圣遗物收藏
    )
}

/// Sample average brightness in a ring around a constellation icon position.
///
/// `c_index` is 0-based: 0=C1, 1=C2, ..., 5=C6.
/// Samples pixels between r_inner and r_outer from the icon center.
/// The ring avoids the icon center (where active art and locked lock icon have
/// similar brightness) and captures the glow vs dark-circle region.
///
/// Returns the average (R+G+B)/3 brightness of sampled pixels.
fn sample_constellation_brightness(
    image: &RgbImage,
    scaler: &CoordScaler,
    c_index: usize,
) -> f64 {
    use super::constants::{
        CONSTELLATION_NODES, CONSTELLATION_RING_INNER, CONSTELLATION_RING_OUTER,
    };

    let (cx, cy) = CONSTELLATION_NODES[c_index];
    let r_inner = CONSTELLATION_RING_INNER;
    let r_outer = CONSTELLATION_RING_OUTER;
    let r_inner_sq = (r_inner as f64) * (r_inner as f64);
    let r_outer_sq = (r_outer as f64) * (r_outer as f64);

    let mut sum = 0.0_f64;
    let mut count = 0u32;

    // Sample every other pixel (step=2) for speed
    let mut bx = cx as i32 - r_outer;
    let bx_end = cx as i32 + r_outer;
    while bx <= bx_end {
        let mut dy = -r_outer;
        while dy <= r_outer {
            let dx = bx as f64 - cx;
            let dist_sq = dx * dx + (dy as f64) * (dy as f64);
            if dist_sq >= r_inner_sq && dist_sq <= r_outer_sq {
                let px = scaler.x(bx as f64) as u32;
                let py = scaler.y(cy + dy as f64) as u32;
                if px < image.width() && py < image.height() {
                    let pixel = image.get_pixel(px, py);
                    sum += (pixel[0] as f64 + pixel[1] as f64 + pixel[2] as f64) / 3.0;
                    count += 1;
                }
            }
            dy += 2;
        }
        bx += 2;
    }

    if count > 0 { sum / count as f64 } else { 0.0 }
}

/// Detect constellation level from the constellation sidebar screenshot using pixel brightness.
///
/// Checks all 6 icon positions with per-position thresholds, then enforces
/// monotonicity (constellations are always contiguous from C1).
/// Constellation = index of first locked node.
///
/// Accuracy: 100% on 109 test characters (min gap=+55.4, d'=7.14).
pub fn detect_constellation_pixel(image: &RgbImage, scaler: &CoordScaler) -> (i32, bool) {
    use super::constants::CONSTELLATION_THRESHOLDS;

    let mut brightnesses = [0.0_f64; 6];
    for ci in 0..6 {
        brightnesses[ci] = sample_constellation_brightness(image, scaler, ci);
    }

    // Per-position threshold check
    let active: Vec<bool> = (0..6)
        .map(|ci| brightnesses[ci] >= CONSTELLATION_THRESHOLDS[ci])
        .collect();

    // Monotonicity: constellation = first locked position
    let mut constellation = 0;
    for ci in 0..6 {
        if active[ci] {
            constellation = ci as i32 + 1;
        } else {
            break;
        }
    }

    // Check for non-monotonic pattern (A-L-A) which would indicate a detection error
    let mut non_monotonic = false;
    for ci in (constellation as usize)..6 {
        if active[ci] {
            non_monotonic = true;
            break;
        }
    }

    let det_str: String = active.iter().map(|&a| if a { 'A' } else { 'L' }).collect();
    if non_monotonic {
        log_debug!(
            "[constellation-pixel] 非单调: [{}] br=[{:.0},{:.0},{:.0},{:.0},{:.0},{:.0}] → C{}",
            "[constellation-pixel] NON-MONOTONIC: [{}] br=[{:.0},{:.0},{:.0},{:.0},{:.0},{:.0}] → C{}",
            det_str,
            brightnesses[0], brightnesses[1], brightnesses[2],
            brightnesses[3], brightnesses[4], brightnesses[5],
            constellation
        );
    } else {
        log_debug!(
            "[constellation-pixel] [{}] br=[{:.0},{:.0},{:.0},{:.0},{:.0},{:.0}] → C{}",
            "[constellation-pixel] [{}] br=[{:.0},{:.0},{:.0},{:.0},{:.0},{:.0}] → C{}",
            det_str,
            brightnesses[0], brightnesses[1], brightnesses[2],
            brightnesses[3], brightnesses[4], brightnesses[5],
            constellation
        );
    }

    (constellation, !non_monotonic)
}

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
        // Blank image has no star pixels; fallback single-pixel checks also fail → returns 3
        assert_eq!(detect_artifact_rarity(&image, &scaler), 3);
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
        // Paint unlocked at base position so black pixels don't read as "dark"
        paint_artifact_lock(&mut image, false, 0.0);
        // Paint locked at shifted position
        paint_artifact_lock(&mut image, true, 40.0);
        let scaler = make_1080p_scaler();
        assert!(!detect_artifact_lock(&image, &scaler, 0.0));
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
        let mid: [u8; 3] = [150, 150, 150];
        set_pixel(&mut image, 1683, 428, mid);
        assert!(is_artifact_icon_ambiguous(&image, &scaler));
    }

    #[test]
    fn test_icon_not_ambiguous_when_clearly_locked() {
        let mut image = make_1080p_image();
        let scaler = make_1080p_scaler();
        let dark: [u8; 3] = [60, 60, 60];
        set_pixel(&mut image, 1683, 428, dark);
        set_pixel(&mut image, 1768, 428, dark);
        assert!(!is_artifact_icon_ambiguous(&image, &scaler));
    }
}

use image::RgbImage;

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
        log::error!(
            "[{}] detection inconsistent: ({},{})={} ({},{})={}",
            label, x1, y1, d1, x2, y2, d2
        );
    }
    d1
}

/// Detect weapon rarity from star pixels.
///
/// Checks yellow pixels at x=1485, 1450, 1416 at y=372 (base coords).
/// Returns 5, 4, 3, or 2.
///
/// Port from GOODScanner/lib/weapon_scanner.js rarity detection
pub fn detect_weapon_rarity(image: &RgbImage, scaler: &CoordScaler) -> i32 {
    use super::constants::STAR_Y;
    if is_star_yellow(image, scaler, 1485.0, STAR_Y) {
        5
    } else if is_star_yellow(image, scaler, 1450.0, STAR_Y) {
        4
    } else if is_star_yellow(image, scaler, 1416.0, STAR_Y) {
        3
    } else {
        2
    }
}

/// Detect artifact rarity from star pixels.
///
/// Checks yellow pixels at x=1485, 1450 at y=372 (base coords).
/// Returns 5, 4, or 3.
///
/// Port from GOODScanner/lib/artifact_scanner.js rarity detection
pub fn detect_artifact_rarity(image: &RgbImage, scaler: &CoordScaler) -> i32 {
    use super::constants::STAR_Y;
    if is_star_yellow(image, scaler, 1485.0, STAR_Y) {
        5
    } else if is_star_yellow(image, scaler, 1450.0, STAR_Y) {
        4
    } else {
        3
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

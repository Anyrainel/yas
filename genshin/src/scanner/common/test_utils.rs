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

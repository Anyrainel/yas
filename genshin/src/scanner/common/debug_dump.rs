use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use image::{GenericImageView, Rgb, RgbImage};

use super::coord_scaler::CoordScaler;
use super::grid_icon_detector::GridCellAnnotation;
use super::pixel_utils::ConstellationResult;

// ── CJK font rendering (ab_glyph + Windows system font) ─────────────────────

/// Base font size for CJK labels at 1920×1080 resolution.
/// Scaled proportionally for higher resolutions (e.g., 2× at 4K).
const CJK_FONT_SIZE_BASE: f32 = 24.0;

/// Cached system font (loaded once, shared across all dump collectors).
static SYSTEM_FONT: OnceLock<Option<FontVec>> = OnceLock::new();

fn get_system_font() -> Option<&'static FontVec> {
    SYSTEM_FONT.get_or_init(|| {
        // Try common Windows CJK fonts (present on all Windows 7+)
        let paths = [
            "C:/Windows/Fonts/msyh.ttc",    // Microsoft YaHei (微软雅黑)
            "C:/Windows/Fonts/msyhbd.ttc",   // Microsoft YaHei Bold
            "C:/Windows/Fonts/simsun.ttc",   // SimSun (宋体)
            "C:/Windows/Fonts/simhei.ttf",   // SimHei (黑体)
        ];
        for path in &paths {
            if let Ok(data) = std::fs::read(path) {
                // .ttc files: use index 0 (regular weight)
                if let Ok(font) = FontVec::try_from_vec_and_index(data, 0) {
                    return Some(font);
                }
            }
        }
        None
    }).as_ref()
}

/// Draw a text label using the system CJK font (ab_glyph).
/// Renders a background rectangle behind the text, then alpha-blends glyphs on top.
fn draw_label_cjk(
    image: &mut RgbImage,
    font: &FontVec,
    x: i32,
    y: i32,
    text: &str,
    fg: Rgb<u8>,
    bg: Rgb<u8>,
    font_size: f32,
) {
    let scaled = font.as_scaled(PxScale::from(font_size));
    let pad = (font_size * 0.15).ceil() as i32;
    let iw = image.width() as i32;
    let ih = image.height() as i32;

    // Measure text width
    let text_w: f32 = text.chars()
        .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
        .sum();
    let bg_w = text_w.ceil() as i32 + pad * 2;
    let bg_h = font_size.ceil() as i32 + pad * 2;

    // Draw background rectangle
    for py in y.max(0)..(y + bg_h).min(ih) {
        for px in x.max(0)..(x + bg_w).min(iw) {
            image.put_pixel(px as u32, py as u32, bg);
        }
    }

    // Draw text glyphs with alpha blending
    let baseline_y = y as f32 + pad as f32 + scaled.ascent();
    let mut cursor_x = x as f32 + pad as f32;

    for ch in text.chars() {
        let glyph_id = scaled.glyph_id(ch);
        let glyph = glyph_id.with_scale_and_position(
            PxScale::from(font_size),
            point(cursor_x, baseline_y),
        );

        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px = gx as i32 + bounds.min.x as i32;
                let py = gy as i32 + bounds.min.y as i32;
                if px >= 0 && px < iw && py >= 0 && py < ih && coverage > 0.05 {
                    let existing = image.get_pixel(px as u32, py as u32);
                    let blend = |f: u8, b: u8| -> u8 {
                        (f as f32 * coverage + b as f32 * (1.0 - coverage)).round().clamp(0.0, 255.0) as u8
                    };
                    image.put_pixel(px as u32, py as u32, Rgb([
                        blend(fg[0], existing[0]),
                        blend(fg[1], existing[1]),
                        blend(fg[2], existing[2]),
                    ]));
                }
            });
        }

        cursor_x += scaled.h_advance(glyph_id);
    }
}

/// Measure the pixel dimensions of a label rendered with `draw_label_cjk`.
fn label_size_cjk(font: &FontVec, text: &str, font_size: f32) -> (i32, i32) {
    let scaled = font.as_scaled(PxScale::from(font_size));
    let pad = (font_size * 0.15).ceil() as i32;
    let text_w: f32 = text.chars()
        .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
        .sum();
    (text_w.ceil() as i32 + pad * 2, font_size.ceil() as i32 + pad * 2)
}

// ── Bitmap font fallback (6×8 printable ASCII 32..127) ───────────────────────
// Each glyph is 6 columns × 8 rows packed into [u8; 6] (one byte per column,
// LSB = top row).  Total: 96 glyphs × 6 bytes = 576 bytes.
#[rustfmt::skip]
const FONT_6X8: [[u8; 6]; 96] = [
    [0x00,0x00,0x00,0x00,0x00,0x00], // 32  (space)
    [0x00,0x00,0x5F,0x00,0x00,0x00], // 33  !
    [0x00,0x07,0x00,0x07,0x00,0x00], // 34  "
    [0x14,0x7F,0x14,0x7F,0x14,0x00], // 35  #
    [0x24,0x2A,0x7F,0x2A,0x12,0x00], // 36  $
    [0x23,0x13,0x08,0x64,0x62,0x00], // 37  %
    [0x36,0x49,0x55,0x22,0x50,0x00], // 38  &
    [0x00,0x05,0x03,0x00,0x00,0x00], // 39  '
    [0x00,0x1C,0x22,0x41,0x00,0x00], // 40  (
    [0x00,0x41,0x22,0x1C,0x00,0x00], // 41  )
    [0x14,0x08,0x3E,0x08,0x14,0x00], // 42  *
    [0x08,0x08,0x3E,0x08,0x08,0x00], // 43  +
    [0x00,0x50,0x30,0x00,0x00,0x00], // 44  ,
    [0x08,0x08,0x08,0x08,0x08,0x00], // 45  -
    [0x00,0x60,0x60,0x00,0x00,0x00], // 46  .
    [0x20,0x10,0x08,0x04,0x02,0x00], // 47  /
    [0x3E,0x51,0x49,0x45,0x3E,0x00], // 48  0
    [0x00,0x42,0x7F,0x40,0x00,0x00], // 49  1
    [0x42,0x61,0x51,0x49,0x46,0x00], // 50  2
    [0x21,0x41,0x45,0x4B,0x31,0x00], // 51  3
    [0x18,0x14,0x12,0x7F,0x10,0x00], // 52  4
    [0x27,0x45,0x45,0x45,0x39,0x00], // 53  5
    [0x3C,0x4A,0x49,0x49,0x30,0x00], // 54  6
    [0x01,0x71,0x09,0x05,0x03,0x00], // 55  7
    [0x36,0x49,0x49,0x49,0x36,0x00], // 56  8
    [0x06,0x49,0x49,0x29,0x1E,0x00], // 57  9
    [0x00,0x36,0x36,0x00,0x00,0x00], // 58  :
    [0x00,0x56,0x36,0x00,0x00,0x00], // 59  ;
    [0x08,0x14,0x22,0x41,0x00,0x00], // 60  <
    [0x14,0x14,0x14,0x14,0x14,0x00], // 61  =
    [0x00,0x41,0x22,0x14,0x08,0x00], // 62  >
    [0x02,0x01,0x51,0x09,0x06,0x00], // 63  ?
    [0x32,0x49,0x79,0x41,0x3E,0x00], // 64  @
    [0x7E,0x11,0x11,0x11,0x7E,0x00], // 65  A
    [0x7F,0x49,0x49,0x49,0x36,0x00], // 66  B
    [0x3E,0x41,0x41,0x41,0x22,0x00], // 67  C
    [0x7F,0x41,0x41,0x22,0x1C,0x00], // 68  D
    [0x7F,0x49,0x49,0x49,0x41,0x00], // 69  E
    [0x7F,0x09,0x09,0x09,0x01,0x00], // 70  F
    [0x3E,0x41,0x49,0x49,0x7A,0x00], // 71  G
    [0x7F,0x08,0x08,0x08,0x7F,0x00], // 72  H
    [0x00,0x41,0x7F,0x41,0x00,0x00], // 73  I
    [0x20,0x40,0x41,0x3F,0x01,0x00], // 74  J
    [0x7F,0x08,0x14,0x22,0x41,0x00], // 75  K
    [0x7F,0x40,0x40,0x40,0x40,0x00], // 76  L
    [0x7F,0x02,0x0C,0x02,0x7F,0x00], // 77  M
    [0x7F,0x04,0x08,0x10,0x7F,0x00], // 78  N
    [0x3E,0x41,0x41,0x41,0x3E,0x00], // 79  O
    [0x7F,0x09,0x09,0x09,0x06,0x00], // 80  P
    [0x3E,0x41,0x51,0x21,0x5E,0x00], // 81  Q
    [0x7F,0x09,0x19,0x29,0x46,0x00], // 82  R
    [0x46,0x49,0x49,0x49,0x31,0x00], // 83  S
    [0x01,0x01,0x7F,0x01,0x01,0x00], // 84  T
    [0x3F,0x40,0x40,0x40,0x3F,0x00], // 85  U
    [0x1F,0x20,0x40,0x20,0x1F,0x00], // 86  V
    [0x3F,0x40,0x38,0x40,0x3F,0x00], // 87  W
    [0x63,0x14,0x08,0x14,0x63,0x00], // 88  X
    [0x07,0x08,0x70,0x08,0x07,0x00], // 89  Y
    [0x61,0x51,0x49,0x45,0x43,0x00], // 90  Z
    [0x00,0x7F,0x41,0x41,0x00,0x00], // 91  [
    [0x02,0x04,0x08,0x10,0x20,0x00], // 92  backslash
    [0x00,0x41,0x41,0x7F,0x00,0x00], // 93  ]
    [0x04,0x02,0x01,0x02,0x04,0x00], // 94  ^
    [0x40,0x40,0x40,0x40,0x40,0x00], // 95  _
    [0x00,0x01,0x02,0x04,0x00,0x00], // 96  `
    [0x20,0x54,0x54,0x54,0x78,0x00], // 97  a
    [0x7F,0x48,0x44,0x44,0x38,0x00], // 98  b
    [0x38,0x44,0x44,0x44,0x20,0x00], // 99  c
    [0x38,0x44,0x44,0x48,0x7F,0x00], // 100 d
    [0x38,0x54,0x54,0x54,0x18,0x00], // 101 e
    [0x08,0x7E,0x09,0x01,0x02,0x00], // 102 f
    [0x0C,0x52,0x52,0x52,0x3E,0x00], // 103 g
    [0x7F,0x08,0x04,0x04,0x78,0x00], // 104 h
    [0x00,0x44,0x7D,0x40,0x00,0x00], // 105 i
    [0x20,0x40,0x44,0x3D,0x00,0x00], // 106 j
    [0x7F,0x10,0x28,0x44,0x00,0x00], // 107 k
    [0x00,0x41,0x7F,0x40,0x00,0x00], // 108 l
    [0x7C,0x04,0x18,0x04,0x78,0x00], // 109 m
    [0x7C,0x08,0x04,0x04,0x78,0x00], // 110 n
    [0x38,0x44,0x44,0x44,0x38,0x00], // 111 o
    [0x7C,0x14,0x14,0x14,0x08,0x00], // 112 p
    [0x08,0x14,0x14,0x18,0x7C,0x00], // 113 q
    [0x7C,0x08,0x04,0x04,0x08,0x00], // 114 r
    [0x48,0x54,0x54,0x54,0x20,0x00], // 115 s
    [0x04,0x3F,0x44,0x40,0x20,0x00], // 116 t
    [0x3C,0x40,0x40,0x20,0x7C,0x00], // 117 u
    [0x1C,0x20,0x40,0x20,0x1C,0x00], // 118 v
    [0x3C,0x40,0x30,0x40,0x3C,0x00], // 119 w
    [0x44,0x28,0x10,0x28,0x44,0x00], // 120 x
    [0x0C,0x50,0x50,0x50,0x3C,0x00], // 121 y
    [0x44,0x64,0x54,0x4C,0x44,0x00], // 122 z
    [0x00,0x08,0x36,0x41,0x00,0x00], // 123 {
    [0x00,0x00,0x7F,0x00,0x00,0x00], // 124 |
    [0x00,0x41,0x36,0x08,0x00,0x00], // 125 }
    [0x10,0x08,0x08,0x10,0x08,0x00], // 126 ~
    [0x00,0x00,0x00,0x00,0x00,0x00], // 127 DEL (blank)
];

// ── Drawing primitives ──────────────────────────────────────────────────────

const RED: Rgb<u8> = Rgb([255, 50, 50]);
const GREEN: Rgb<u8> = Rgb([50, 255, 50]);
const CYAN: Rgb<u8> = Rgb([50, 220, 220]);
const BLACK: Rgb<u8> = Rgb([0, 0, 0]);

fn draw_rect(image: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, color: Rgb<u8>, thickness: i32) {
    let iw = image.width() as i32;
    let ih = image.height() as i32;
    for t in 0..thickness {
        // Top and bottom edges
        for px in x..x + w {
            let ty = y + t;
            let by = y + h - 1 - t;
            if px >= 0 && px < iw {
                if ty >= 0 && ty < ih { image.put_pixel(px as u32, ty as u32, color); }
                if by >= 0 && by < ih { image.put_pixel(px as u32, by as u32, color); }
            }
        }
        // Left and right edges
        for py in y..y + h {
            let lx = x + t;
            let rx = x + w - 1 - t;
            if py >= 0 && py < ih {
                if lx >= 0 && lx < iw { image.put_pixel(lx as u32, py as u32, color); }
                if rx >= 0 && rx < iw { image.put_pixel(rx as u32, py as u32, color); }
            }
        }
    }
}

fn draw_crosshair(image: &mut RgbImage, cx: i32, cy: i32, size: i32, thickness: i32, color: Rgb<u8>) {
    let iw = image.width() as i32;
    let ih = image.height() as i32;
    let half_t = thickness / 2;
    for d in -size..=size {
        // Horizontal arm
        let hx = cx + d;
        for t in -half_t..=half_t {
            let py = cy + t;
            if hx >= 0 && hx < iw && py >= 0 && py < ih {
                image.put_pixel(hx as u32, py as u32, color);
            }
        }
        // Vertical arm
        let vy = cy + d;
        for t in -half_t..=half_t {
            let px = cx + t;
            if px >= 0 && px < iw && vy >= 0 && vy < ih {
                image.put_pixel(px as u32, vy as u32, color);
            }
        }
    }
}

/// Annotation scale factor for bitmap font fallback.
const ANNOTATION_SCALE: i32 = 3;

/// Draw a text label using the embedded 6×8 bitmap font, scaled by `ANNOTATION_SCALE`.
/// Non-ASCII characters render as `?`. Used as fallback when system font is unavailable.
fn draw_label_bitmap(image: &mut RgbImage, x: i32, y: i32, text: &str, fg: Rgb<u8>, bg: Rgb<u8>) {
    let s = ANNOTATION_SCALE;
    let iw = image.width() as i32;
    let ih = image.height() as i32;
    let char_w = 6 * s;
    let char_h = 8 * s;
    let pad = s; // padding around text
    let text_w = text.chars().count() as i32 * char_w + pad * 2;
    let text_h = char_h + pad * 2;
    // Draw background rectangle
    for py in y.max(0)..(y + text_h).min(ih) {
        for px in x.max(0)..(x + text_w).min(iw) {
            image.put_pixel(px as u32, py as u32, bg);
        }
    }
    // Draw characters (each pixel in the 6×8 glyph becomes an s×s block)
    for (ci, ch) in text.chars().enumerate() {
        let glyph_idx = if ch as u32 >= 32 && ch as u32 <= 127 {
            (ch as u32 - 32) as usize
        } else {
            ('?' as u32 - 32) as usize
        };
        let glyph = &FONT_6X8[glyph_idx];
        let base_x = x + pad + ci as i32 * char_w;
        for col in 0..6i32 {
            let bits = glyph[col as usize];
            for row in 0..8i32 {
                if bits & (1 << row) != 0 {
                    for dy in 0..s {
                        for dx in 0..s {
                            let px = base_x + col * s + dx;
                            let py = y + pad + row * s + dy;
                            if px >= 0 && px < iw && py >= 0 && py < ih {
                                image.put_pixel(px as u32, py as u32, fg);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Measure label size for bitmap font.
fn label_size_bitmap(text: &str) -> (i32, i32) {
    let s = ANNOTATION_SCALE;
    let w = text.chars().count() as i32 * 6 * s + s * 2;
    let h = 8 * s + s * 2;
    (w, h)
}

// ── Unified label drawing ────────────────────────────────────────────────────

/// Compute the scaled CJK font size for the given image width.
/// At 1920px → `CJK_FONT_SIZE_BASE`, scales linearly for larger/smaller images.
fn scaled_font_size(image_width: u32) -> f32 {
    CJK_FONT_SIZE_BASE * image_width as f32 / 1920.0
}

/// Draw a label at (x, y) using the best available font.
/// `font_size` is the pre-computed scaled font size for the current image.
/// Returns (label_width, label_height).
fn draw_smart_label(image: &mut RgbImage, x: i32, y: i32, text: &str, fg: Rgb<u8>, bg: Rgb<u8>, font_size: f32) -> (i32, i32) {
    if let Some(font) = get_system_font() {
        let size = label_size_cjk(font, text, font_size);
        draw_label_cjk(image, font, x, y, text, fg, bg, font_size);
        size
    } else {
        let size = label_size_bitmap(text);
        draw_label_bitmap(image, x, y, text, fg, bg);
        size
    }
}

/// Measure label size using the best available font.
/// `font_size` is the pre-computed scaled font size for the current image.
fn measure_label(text: &str, font_size: f32) -> (i32, i32) {
    if let Some(font) = get_system_font() {
        label_size_cjk(font, text, font_size)
    } else {
        label_size_bitmap(text)
    }
}

// ── Level cap display helper ─────────────────────────────────────────────────

/// Convert (level, ascended) to "level/cap" display string.
///
/// Genshin ascension boundaries: 20, 40, 50, 60, 70, 80 → cap 90.
/// At a boundary level, `ascended=true` means the cap is the next boundary.
pub fn level_cap_display(level: i32, ascended: bool) -> String {
    let caps: [(i32, i32); 8] = [
        (20, 40), (40, 50), (50, 60), (60, 70),
        (70, 80), (80, 90), (90, 95), (95, 100),
    ];
    let cap = 'find: {
        for &(boundary, next_cap) in &caps {
            if level < boundary {
                break 'find boundary;
            }
            if level == boundary && !ascended {
                break 'find boundary;
            }
            if level == boundary && ascended {
                break 'find next_cap;
            }
        }
        100
    };
    format!("{}/{}", level, cap)
}

// ── DumpEntry & DumpCollector ───────────────────────────────────────────────

/// Where to position the label relative to its OCR bounding box.
#[derive(Clone, Copy, PartialEq)]
pub enum LabelPosition {
    /// Right of region if space allows, else left (default).
    Auto,
    /// Below the bounding box.
    Below,
}

pub enum DumpEntry {
    OcrRegion {
        field_name: String,
        /// Base 1920×1080 coords (already shifted by y_shift where applicable)
        rect: (f64, f64, f64, f64),
        raw_text: String,
        /// GOOD key / code-level result (used in result.txt).
        final_result: String,
        /// Human-readable display name for image annotation (e.g., Chinese name).
        /// When set and different from raw_text, annotation shows "raw -> display".
        display_result: String,
        label_pos: LabelPosition,
    },
    PixelCheck {
        field_name: String,
        /// Base coords (already shifted)
        pos: (f64, f64),
        rgb: [u8; 3],
        result_text: String,
        /// Optional color override for crosshair + label (default: GREEN).
        color: Option<Rgb<u8>>,
    },
    /// Constellation detection result — 6 node bounding boxes + final level.
    Constellation {
        level: i32,
        /// Per-node: (center_pos, activated, brightness, threshold)
        nodes: Vec<((f64, f64), bool, f64, f64)>,
    },
    /// Grid overlay — cell bounding boxes + detected lock/astral positions.
    GridOverlay {
        cells: Vec<GridCellAnnotation>,
        /// Per-cell detection results: (index, lock_detected, astral_detected)
        detections: Vec<(usize, bool, bool)>,
    },
}

pub struct DumpCollector {
    dir: PathBuf,
    images: Vec<(String, RgbImage)>,
    entries: Vec<(usize, DumpEntry)>,
    warnings: Vec<String>,
    scaler: CoordScaler,
    finalized: bool,
}

impl DumpCollector {
    /// Create a new dump collector. Creates the output directory.
    pub fn new(base_dir: &str, category: &str, index: usize, scaler: &CoordScaler) -> Self {
        let folder = format!("{:04}", index);
        let dir = Path::new(base_dir).join(category).join(folder);
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            images: Vec::new(),
            entries: Vec::new(),
            warnings: Vec::new(),
            scaler: scaler.clone(),
            finalized: false,
        }
    }

    /// Number of registered images.
    pub fn images_len(&self) -> usize {
        self.images.len()
    }

    /// Register a captured image. Returns the image index for later entry references.
    pub fn add_image(&mut self, label: &str, image: &RgbImage) -> usize {
        let idx = self.images.len();
        self.images.push((label.to_string(), image.clone()));
        idx
    }

    /// Record an OCR region result. `rect` should already include any y_shift.
    pub fn record_ocr(
        &mut self,
        img_idx: usize,
        field_name: &str,
        rect: (f64, f64, f64, f64),
        raw_text: &str,
    ) {
        self.entries.push((img_idx, DumpEntry::OcrRegion {
            field_name: field_name.to_string(),
            rect,
            raw_text: raw_text.to_string(),
            final_result: String::new(),
            display_result: String::new(),
            label_pos: LabelPosition::Auto,
        }));
    }

    /// Update the final (post-processed) result for a previously recorded OCR entry.
    /// This is the GOOD key / code-level value shown in result.txt.
    pub fn set_final_result(&mut self, field_name: &str, final_result: &str) {
        for (_, entry) in self.entries.iter_mut().rev() {
            if let DumpEntry::OcrRegion { field_name: ref name, final_result: ref mut fr, .. } = entry {
                if name == field_name {
                    *fr = final_result.to_string();
                    return;
                }
            }
        }
    }

    /// Set the human-readable display name for image annotations.
    /// When this differs from raw_text, the image label shows "raw -> display".
    /// Use this with the Chinese name (not the GOOD key) for entity names.
    pub fn set_display_result(&mut self, field_name: &str, display: &str) {
        for (_, entry) in self.entries.iter_mut().rev() {
            if let DumpEntry::OcrRegion { field_name: ref name, display_result: ref mut dr, .. } = entry {
                if name == field_name {
                    *dr = display.to_string();
                    return;
                }
            }
        }
    }

    /// Set label position to Below for a field (e.g., weapon name).
    pub fn set_label_below(&mut self, field_name: &str) {
        for (_, entry) in self.entries.iter_mut().rev() {
            if let DumpEntry::OcrRegion { field_name: ref name, label_pos: ref mut lp, .. } = entry {
                if name == field_name {
                    *lp = LabelPosition::Below;
                    return;
                }
            }
        }
    }

    /// Record a pixel check result with default color (green).
    pub fn record_pixel(
        &mut self,
        img_idx: usize,
        field_name: &str,
        pos: (f64, f64),
        rgb: [u8; 3],
        result_text: &str,
    ) {
        self.record_pixel_colored(img_idx, field_name, pos, rgb, result_text, None);
    }

    /// Record a pixel check result with an optional color override.
    pub fn record_pixel_colored(
        &mut self,
        img_idx: usize,
        field_name: &str,
        pos: (f64, f64),
        rgb: [u8; 3],
        result_text: &str,
        color: Option<Rgb<u8>>,
    ) {
        self.entries.push((img_idx, DumpEntry::PixelCheck {
            field_name: field_name.to_string(),
            pos,
            rgb,
            result_text: result_text.to_string(),
            color,
        }));
    }

    /// Record constellation detection results for annotation.
    /// Draws per-node bounding boxes with yes/no labels and a centered "Cx" label.
    pub fn record_constellation(&mut self, img_idx: usize, result: &ConstellationResult) {
        let nodes: Vec<_> = result.nodes.iter().map(|n| {
            (n.pos, n.activated, n.brightness, n.threshold)
        }).collect();
        self.entries.push((img_idx, DumpEntry::Constellation {
            level: result.level,
            nodes,
        }));
    }

    /// Record grid overlay for annotation.
    /// `cells` come from the detection pass. `detections` maps cell index → (lock, astral).
    pub fn record_grid_overlay(
        &mut self,
        img_idx: usize,
        cells: Vec<GridCellAnnotation>,
        detections: Vec<(usize, bool, bool)>,
    ) {
        self.entries.push((img_idx, DumpEntry::GridOverlay { cells, detections }));
    }

    /// Add a warning message to the result file.
    pub fn add_warning(&mut self, text: &str) {
        self.warnings.push(text.to_string());
    }

    /// Write all output files for a successfully scanned item.
    pub fn finalize_success(mut self, result_json: &str) {
        self.finalized = true;
        self.write_images();
        self.write_result_txt(Some(result_json), None);
    }

    /// Write output files for a failed scan.
    pub fn finalize_error(mut self, partial_json: Option<&str>, error: &str) {
        self.finalized = true;
        self.write_images();
        self.write_result_txt(partial_json, Some(error));
    }

    /// Write output files for a skipped item.
    pub fn finalize_skip(mut self, reason: &str) {
        self.finalized = true;
        // Minimal output — just error.txt with the skip reason
        let path = self.dir.join("error.txt");
        let _ = std::fs::write(&path, format!("SKIPPED: {}\n", reason));
    }

    // ── Private helpers ─────────────────────────────────────────────────

    fn write_images(&self) {
        let single = self.images.len() == 1;

        for (img_idx, (label, image)) in self.images.iter().enumerate() {
            // Save the original full image
            let full_name = if single { "full.png".to_string() } else { format!("full_{}.png", label) };
            let _ = image.save(self.dir.join(&full_name));

            // Scale all annotation sizes based on image resolution
            let fs = scaled_font_size(image.width());
            let scale = image.width() as f32 / 1920.0;
            let rect_thickness = (2.0 * scale).round().max(1.0) as i32; // split: floor(t/2) inside, rest outside
            let rect_outset = rect_thickness - rect_thickness / 2; // pixels drawn outside the OCR region
            let cross_size = (18.0 * scale).round() as i32; // half of old 36px at 1080p
            let cross_thickness = (2.0 * scale).round().max(1.0) as i32;
            let gap = (6.0 * scale).round().max(2.0) as i32;

            // Build annotated image
            let mut annotated = image.clone();
            for (entry_img_idx, entry) in &self.entries {
                if *entry_img_idx != img_idx { continue; }
                match entry {
                    DumpEntry::OcrRegion { field_name, rect, raw_text, display_result, label_pos, .. } => {
                        let (bx, by, bw, bh) = *rect;
                        let x = self.scaler.x(bx);
                        let y = self.scaler.y(by);
                        let w = self.scaler.x(bw);
                        let h = self.scaler.y(bh);

                        // Skip zero-area entries (synthetic/metadata entries)
                        if w <= 0 || h <= 0 { continue; }

                        // Expand outward so floor(t/2) pixels sit on/inside the region
                        draw_rect(&mut annotated,
                            x - rect_outset, y - rect_outset,
                            w + rect_outset * 2, h + rect_outset * 2,
                            RED, rect_thickness);

                        // Build label text: no field names, just OCR results.
                        // If display_result is set and differs from raw, show "raw -> display".
                        // Strip colons (: ：) before comparing — set names often differ only by colon.
                        let raw_trimmed = raw_text.trim();
                        let normalize = |s: &str| s.replace([':', '：'], "");
                        let raw_norm = normalize(raw_trimmed);
                        let disp_norm = normalize(display_result);
                        // Treat as same if: colons stripped match, or raw == display + "已装备"
                        let effectively_same = !display_result.is_empty()
                            && (disp_norm == raw_norm
                                || raw_norm == format!("{}已装备", disp_norm));
                        let label_text = if !display_result.is_empty() && !effectively_same && display_result != raw_trimmed {
                            format!("{} -> {}", raw_trimmed, display_result)
                        } else if effectively_same {
                            display_result.to_string()
                        } else if !raw_trimmed.is_empty() {
                            raw_trimmed.to_string()
                        } else {
                            continue; // no text to show
                        };

                        let (label_w, label_h) = measure_label(&label_text, fs);
                        let img_w = annotated.width() as i32;

                        let (label_x, label_y) = match label_pos {
                            LabelPosition::Below => {
                                // Center below the bounding box
                                let lx = x + (w - label_w) / 2;
                                let ly = y + h + gap;
                                (lx.max(0).min(img_w - label_w), ly)
                            }
                            LabelPosition::Auto => {
                                // Right of region if it fits, otherwise left
                                let lx = if x + w + gap + label_w <= img_w {
                                    x + w + gap
                                } else {
                                    x - label_w - gap
                                };
                                let ly = y + (h - label_h) / 2;
                                (lx, ly)
                            }
                        };
                        draw_smart_label(&mut annotated, label_x, label_y, &label_text, RED, BLACK, fs);

                        // Save individual crop
                        self.save_crop(image, field_name, *rect);
                    }
                    DumpEntry::PixelCheck { field_name, pos, rgb: _, result_text, color, .. } => {
                        let draw_color = color.unwrap_or(GREEN);
                        let cx = self.scaler.x(pos.0);
                        let cy = self.scaler.y(pos.1);
                        draw_crosshair(&mut annotated, cx, cy, cross_size, cross_thickness, draw_color);

                        // Label: just the result (no field name, no RGB values)
                        let label_text = result_text.to_string();
                        let (label_w, label_h) = measure_label(&label_text, fs);
                        let img_w = annotated.width() as i32;
                        let label_x = if cx + cross_size + gap + label_w <= img_w {
                            cx + cross_size + gap
                        } else {
                            cx - cross_size - gap - label_w
                        };
                        draw_smart_label(&mut annotated, label_x, cy - label_h / 2, &label_text, draw_color, BLACK, fs);

                        // Save pixel neighbourhood crop
                        self.save_pixel_crop(image, field_name, *pos);
                    }
                    DumpEntry::Constellation { level, nodes } => {
                        // Draw bounding box per node with label on the left
                        let box_r = 45.0; // slightly larger than ring outer (41)
                        for (i, &(pos, activated, _brightness, _threshold)) in nodes.iter().enumerate() {
                            let cx = self.scaler.x(pos.0);
                            let cy = self.scaler.y(pos.1);
                            let r = self.scaler.x(box_r);
                            let color = if activated { GREEN } else { RED };
                            draw_rect(&mut annotated, cx - r, cy - r, r * 2, r * 2, color, 2);
                            let label = format!("C{}: {}", i + 1, if activated { "yes" } else { "no" });
                            let (lw, lh) = measure_label(&label, fs);
                            // Place label to the left of the bounding box
                            draw_smart_label(&mut annotated, cx - r - gap - lw, cy - lh / 2, &label, color, BLACK, fs);
                        }
                        // "Cx" label further left, between nodes 3 and 4
                        let center_label = format!("C{}", level);
                        let (clw, clh) = measure_label(&center_label, fs);
                        let mid_y = if nodes.len() >= 4 {
                            let y3 = self.scaler.y(nodes[2].0 .1);
                            let y4 = self.scaler.y(nodes[3].0 .1);
                            (y3 + y4) / 2
                        } else {
                            self.scaler.y(540.0)
                        };
                        // Find the leftmost node label edge and place C# further left
                        let leftmost_x = nodes.iter().map(|&(pos, _, _, _)| {
                            let cx = self.scaler.x(pos.0);
                            let r = self.scaler.x(box_r);
                            let sample_label = "C6: yes"; // widest possible label
                            let (sw, _) = measure_label(sample_label, fs);
                            cx - r - gap - sw
                        }).min().unwrap_or(0);
                        draw_smart_label(&mut annotated, leftmost_x - gap - clw, mid_y - clh / 2,
                            &center_label, CYAN, BLACK, fs);
                    }
                    DumpEntry::GridOverlay { cells, detections } => {
                        let grid_thickness = (1.0 * scale).round().max(1.0) as i32;
                        // Draw all cell bounding boxes
                        for cell in cells {
                            let x = self.scaler.x(cell.rect.0);
                            let y = self.scaler.y(cell.rect.1);
                            let w = self.scaler.x(cell.rect.2);
                            let h = self.scaler.y(cell.rect.3);
                            draw_rect(&mut annotated, x, y, w, h, RED, grid_thickness);
                        }
                        // Draw crosshairs + labels only for detected lock/astral
                        for &(idx, lock, astral) in detections {
                            if idx >= cells.len() { continue; }
                            let cell = &cells[idx];
                            if lock {
                                let cx = self.scaler.x(cell.lock_pos.0);
                                let cy = self.scaler.y(cell.lock_pos.1);
                                draw_crosshair(&mut annotated, cx, cy, cross_size / 2, cross_thickness, CYAN);
                                let x_off = cross_size / 2 + gap;
                                let (_, lh) = measure_label("lock", fs);
                                draw_smart_label(&mut annotated, cx + x_off, cy - lh / 2, "lock", CYAN, BLACK, fs);
                            }
                            if astral {
                                let cx = self.scaler.x(cell.astral_pos.0);
                                let cy = self.scaler.y(cell.astral_pos.1);
                                draw_crosshair(&mut annotated, cx, cy, cross_size / 2, cross_thickness, GREEN);
                                let x_off = cross_size / 2 + gap;
                                let (_, lh) = measure_label("astral", fs);
                                draw_smart_label(&mut annotated, cx + x_off, cy - lh / 2, "astral", GREEN, BLACK, fs);
                            }
                        }
                    }
                }
            }

            let ann_name = if single { "annotated.png".to_string() } else { format!("annotated_{}.png", label) };
            let _ = annotated.save(self.dir.join(&ann_name));
        }
    }

    fn save_crop(&self, image: &RgbImage, name: &str, rect: (f64, f64, f64, f64)) {
        let (bx, by, bw, bh) = rect;
        let x = (self.scaler.x(bx) as u32).min(image.width().saturating_sub(1));
        let y = (self.scaler.y(by) as u32).min(image.height().saturating_sub(1));
        let w = (self.scaler.x(bw) as u32).min(image.width().saturating_sub(x));
        let h = (self.scaler.y(bh) as u32).min(image.height().saturating_sub(y));
        if w == 0 || h == 0 { return; }
        let sub = image.view(x, y, w, h).to_image();
        let _ = sub.save(self.dir.join(format!("{}.png", name)));
    }

    fn save_pixel_crop(&self, image: &RgbImage, name: &str, pos: (f64, f64)) {
        let padding = 10u32;
        let cx = self.scaler.x(pos.0) as i32;
        let cy = self.scaler.y(pos.1) as i32;
        let x = (cx - padding as i32).max(0) as u32;
        let y = (cy - padding as i32).max(0) as u32;
        let w = (padding * 2 + 1).min(image.width().saturating_sub(x));
        let h = (padding * 2 + 1).min(image.height().saturating_sub(y));
        if w == 0 || h == 0 { return; }
        let sub = image.view(x, y, w, h).to_image();
        let _ = sub.save(self.dir.join(format!("{}.png", name)));
    }

    fn write_result_txt(&self, result_json: Option<&str>, error: Option<&str>) {
        let mut out = String::new();

        if let Some(err) = error {
            out.push_str(&format!("FAILED: {}\n\n", err));
        }

        let multi_image = self.images.len() > 1;

        // Group entries by image, writing OCR and pixel results per section
        for (img_idx, (label, _)) in self.images.iter().enumerate() {
            if multi_image {
                out.push_str(&format!("--- {} Screen ---\n", capitalize(label)));
            } else {
                out.push_str("--- Detections ---\n");
            }

            for (entry_idx, entry) in &self.entries {
                if *entry_idx != img_idx { continue; }
                match entry {
                    DumpEntry::OcrRegion { field_name, raw_text, final_result, display_result, .. } => {
                        let label = format!("{}:", field_name);
                        let raw = raw_text.trim();
                        let fin = final_result.trim();
                        let disp = display_result.trim();
                        if raw.is_empty() && !fin.is_empty() {
                            // Computed/summary field (no OCR source)
                            out.push_str(&format!("{:<16} {}\n", label, fin));
                        } else if fin.is_empty() || raw == fin {
                            // No post-processing, or result identical to raw
                            let shown = if raw.is_empty() { "(empty)" } else { raw };
                            out.push_str(&format!("{:<16} {}\n", label, shown));
                        } else if !disp.is_empty() && disp != raw && disp != fin {
                            // 3-tier: raw OCR -> fuzzy-matched name -> GOOD key
                            out.push_str(&format!("{:<16} {} -> {} -> {}\n",
                                label, raw, disp, fin));
                        } else {
                            out.push_str(&format!("{:<16} {} -> {}\n", label, raw, fin));
                        }
                    }
                    DumpEntry::PixelCheck { field_name, rgb, result_text, .. } => {
                        out.push_str(&format!("{:<16} rgb({},{},{}) -> {}\n",
                            format!("{}:", field_name),
                            rgb[0], rgb[1], rgb[2],
                            result_text,
                        ));
                    }
                    DumpEntry::Constellation { level, nodes } => {
                        out.push_str(&format!("{:<16} C{}\n", "constellation:", level));
                        for (i, &(_pos, activated, brightness, threshold)) in nodes.iter().enumerate() {
                            out.push_str(&format!("  C{}: {:<3} brightness={:.0} threshold={:.0}\n",
                                i + 1,
                                if activated { "yes" } else { "no" },
                                brightness,
                                threshold,
                            ));
                        }
                    }
                    DumpEntry::GridOverlay { cells, detections } => {
                        out.push_str(&format!("{:<16} {} cells\n", "grid:", cells.len()));
                        let locks: Vec<_> = detections.iter().filter(|d| d.1).map(|d| d.0).collect();
                        let astrals: Vec<_> = detections.iter().filter(|d| d.2).map(|d| d.0).collect();
                        if !locks.is_empty() {
                            out.push_str(&format!("{:<16} cells {:?}\n", "  locked:", locks));
                        }
                        if !astrals.is_empty() {
                            out.push_str(&format!("{:<16} cells {:?}\n", "  astral:", astrals));
                        }
                    }
                }
            }
            out.push('\n');
        }

        // Warnings
        out.push_str("--- Warnings ---\n");
        if self.warnings.is_empty() {
            out.push_str("(none)\n");
        } else {
            for w in &self.warnings {
                out.push_str(&format!("- {}\n", w));
            }
        }

        // Final JSON
        if let Some(json) = result_json {
            out.push_str("\n--- Final Object ---\n");
            out.push_str(json);
            out.push('\n');
        }

        let filename = if error.is_some() { "error.txt" } else { "result.txt" };
        let _ = std::fs::write(self.dir.join(filename), &out);
    }
}

impl Drop for DumpCollector {
    fn drop(&mut self) {
        if !self.finalized {
            // Write a minimal error.txt so debug output is never silently lost
            // (e.g., when a ? operator propagates an error before finalize is called)
            let path = self.dir.join("error.txt");
            let _ = std::fs::write(&path, "DROPPED: scan function exited before finalize (likely an error)\n");
        }
    }
}

/// Capitalize first letter of a string (for section headers).
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}

// ── Legacy DumpCtx (kept during migration, will be removed in Step 5) ───────

/// Legacy dump context — simple image saves without annotations or result text.
/// Used by manager modules and backpack_scanner. Will be replaced by DumpCollector.
pub struct DumpCtx {
    dir: PathBuf,
}

impl DumpCtx {
    pub fn new(base_dir: &str, category: &str, index: usize, _entity_name: &str) -> Self {
        let folder = format!("{:04}", index);
        let dir = Path::new(base_dir).join(category).join(folder);
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    pub fn dump_full(&self, image: &RgbImage) {
        let path = self.dir.join("full.png");
        let _ = image.save(&path);
    }

    pub fn dump_region(&self, field_name: &str, image: &RgbImage, rect: (f64, f64, f64, f64), scaler: &CoordScaler) {
        save_region(&self.dir, field_name, image, rect, scaler);
    }

    pub fn dump_region_shifted(&self, field_name: &str, image: &RgbImage, rect: (f64, f64, f64, f64), y_shift: f64, scaler: &CoordScaler) {
        let shifted = (rect.0, rect.1 + y_shift, rect.2, rect.3);
        save_region(&self.dir, field_name, image, shifted, scaler);
    }

    pub fn dump_pixel(&self, field_name: &str, image: &RgbImage, center: (f64, f64), padding: u32, scaler: &CoordScaler) {
        let cx = scaler.x(center.0) as i32;
        let cy = scaler.y(center.1) as i32;
        let x = (cx - padding as i32).max(0) as u32;
        let y = (cy - padding as i32).max(0) as u32;
        let w = (padding * 2 + 1).min(image.width().saturating_sub(x));
        let h = (padding * 2 + 1).min(image.height().saturating_sub(y));
        if w == 0 || h == 0 { return; }
        let sub = image.view(x, y, w, h).to_image();
        let _ = sub.save(self.dir.join(format!("{}.png", field_name)));
    }
}

fn save_region(dir: &Path, name: &str, image: &RgbImage, rect: (f64, f64, f64, f64), scaler: &CoordScaler) {
    let (bx, by, bw, bh) = rect;
    let x = (scaler.x(bx) as u32).min(image.width().saturating_sub(1));
    let y = (scaler.y(by) as u32).min(image.height().saturating_sub(1));
    let w = (scaler.x(bw) as u32).min(image.width().saturating_sub(x));
    let h = (scaler.y(bh) as u32).min(image.height().saturating_sub(y));
    if w == 0 || h == 0 { return; }
    let sub = image.view(x, y, w, h).to_image();
    let _ = sub.save(dir.join(format!("{}.png", name)));
}

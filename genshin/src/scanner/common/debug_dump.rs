use std::path::{Path, PathBuf};

use image::{GenericImageView, Rgb, RgbImage};

use super::coord_scaler::CoordScaler;

// ── Bitmap font (6×8 printable ASCII 32..127) ──────────────────────────────
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

fn draw_crosshair(image: &mut RgbImage, cx: i32, cy: i32, size: i32, color: Rgb<u8>) {
    let iw = image.width() as i32;
    let ih = image.height() as i32;
    for d in -size..=size {
        let hx = cx + d;
        let vy = cy + d;
        if hx >= 0 && hx < iw && cy >= 0 && cy < ih {
            image.put_pixel(hx as u32, cy as u32, color);
        }
        if cx >= 0 && cx < iw && vy >= 0 && vy < ih {
            image.put_pixel(cx as u32, vy as u32, color);
        }
    }
}

/// Annotation scale factor. The base 6×8 font and drawing primitives are
/// designed for tiny images. At game resolution (1920×1080+), we scale
/// everything up so annotations are readable without zooming.
const ANNOTATION_SCALE: i32 = 3;

/// Draw a text label using the embedded 6×8 bitmap font, scaled by `ANNOTATION_SCALE`.
/// Non-ASCII characters render as `?`.
fn draw_label(image: &mut RgbImage, x: i32, y: i32, text: &str, fg: Rgb<u8>, bg: Rgb<u8>) {
    let s = ANNOTATION_SCALE;
    let iw = image.width() as i32;
    let ih = image.height() as i32;
    let char_w = 6 * s;
    let char_h = 8 * s;
    let pad = s; // padding around text
    let text_w = text.len() as i32 * char_w + pad * 2;
    let text_h = char_h + pad * 2;
    // Draw background rectangle
    for py in y..y + text_h {
        for px in x..x + text_w {
            if px >= 0 && px < iw && py >= 0 && py < ih {
                image.put_pixel(px as u32, py as u32, bg);
            }
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
                    // Fill an s×s block for this pixel
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

/// Height of a label drawn by `draw_label`, for positioning labels above regions.
const LABEL_HEIGHT: i32 = 8 * ANNOTATION_SCALE + ANNOTATION_SCALE * 2 + 2;

// ── DumpEntry & DumpCollector ───────────────────────────────────────────────

pub enum DumpEntry {
    OcrRegion {
        field_name: String,
        /// Base 1920×1080 coords (already shifted by y_shift where applicable)
        rect: (f64, f64, f64, f64),
        raw_text: String,
        final_result: String,
    },
    PixelCheck {
        field_name: String,
        /// Base coords (already shifted)
        pos: (f64, f64),
        rgb: [u8; 3],
        result_text: String,
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
        }));
    }

    /// Update the final (post-processed) result for a previously recorded OCR entry.
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

    /// Record a pixel check result.
    pub fn record_pixel(
        &mut self,
        img_idx: usize,
        field_name: &str,
        pos: (f64, f64),
        rgb: [u8; 3],
        result_text: &str,
    ) {
        self.entries.push((img_idx, DumpEntry::PixelCheck {
            field_name: field_name.to_string(),
            pos,
            rgb,
            result_text: result_text.to_string(),
        }));
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

            // Build annotated image
            let mut annotated = image.clone();
            for (entry_img_idx, entry) in &self.entries {
                if *entry_img_idx != img_idx { continue; }
                match entry {
                    DumpEntry::OcrRegion { field_name, rect, raw_text, final_result } => {
                        let (bx, by, bw, bh) = *rect;
                        let x = self.scaler.x(bx);
                        let y = self.scaler.y(by);
                        let w = self.scaler.x(bw);
                        let h = self.scaler.y(bh);

                        // Skip zero-area entries (synthetic/metadata entries)
                        if w <= 0 || h <= 0 { continue; }

                        draw_rect(&mut annotated, x, y, w, h, RED, ANNOTATION_SCALE);
                        // Label to the right if it fits, otherwise to the left.
                        // Most game-panel regions are on the right side of the screen,
                        // so labels usually end up on the left.
                        let label_text = if final_result.is_empty() {
                            format!("{}: {}", field_name, truncate_for_label(raw_text))
                        } else {
                            format!("{} -> {}", field_name, truncate_for_label(final_result))
                        };
                        let label_w = label_text.len() as i32 * 6 * ANNOTATION_SCALE + ANNOTATION_SCALE * 2;
                        let gap = ANNOTATION_SCALE * 2;
                        let img_w = annotated.width() as i32;
                        let label_x = if x + w + gap + label_w <= img_w {
                            x + w + gap // right of region
                        } else {
                            x - label_w - gap // left of region
                        };
                        let label_y = y + (h - LABEL_HEIGHT) / 2;
                        draw_label(&mut annotated, label_x, label_y, &label_text, RED, BLACK);

                        // Save individual crop
                        self.save_crop(image, field_name, *rect);
                    }
                    DumpEntry::PixelCheck { field_name, pos, rgb, result_text } => {
                        let cx = self.scaler.x(pos.0);
                        let cy = self.scaler.y(pos.1);
                        let cross_size = 12 * ANNOTATION_SCALE;
                        draw_crosshair(&mut annotated, cx, cy, cross_size, CYAN);
                        let label_text = format!("{}: rgb({},{},{}) {}", field_name, rgb[0], rgb[1], rgb[2], result_text);
                        let label_w = label_text.len() as i32 * 6 * ANNOTATION_SCALE + ANNOTATION_SCALE * 2;
                        let gap = ANNOTATION_SCALE * 2;
                        let img_w = annotated.width() as i32;
                        let label_x = if cx + cross_size + gap + label_w <= img_w {
                            cx + cross_size + gap // right of crosshair
                        } else {
                            cx - cross_size - gap - label_w // left of crosshair
                        };
                        draw_label(&mut annotated, label_x, cy - LABEL_HEIGHT / 2, &label_text, GREEN, BLACK);

                        // Save pixel neighbourhood crop
                        self.save_pixel_crop(image, field_name, *pos);
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
                    DumpEntry::OcrRegion { field_name, raw_text, final_result, .. } => {
                        let display_raw = if raw_text.is_empty() { "(empty)" } else { raw_text.trim() };
                        if final_result.is_empty() {
                            out.push_str(&format!("{:<16} {}\n", format!("{}:", field_name), display_raw));
                        } else {
                            out.push_str(&format!("{:<16} {} -> {}\n",
                                format!("{}:", field_name),
                                display_raw,
                                final_result,
                            ));
                        }
                    }
                    DumpEntry::PixelCheck { field_name, rgb, result_text, .. } => {
                        out.push_str(&format!("{:<16} rgb({},{},{}) -> {}\n",
                            format!("{}:", field_name),
                            rgb[0], rgb[1], rgb[2],
                            result_text,
                        ));
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

/// Truncate a string for use in an on-image label (ASCII-safe, max 40 chars).
/// Replaces CJK characters with `?` since the bitmap font can't render them.
fn truncate_for_label(s: &str) -> String {
    let cleaned: String = s.trim().chars().map(|c| {
        if c as u32 >= 32 && c as u32 <= 127 { c } else { '?' }
    }).collect();
    if cleaned.len() > 40 {
        format!("{}...", &cleaned[..37])
    } else {
        cleaned
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

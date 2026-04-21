//! Thread-local annotation system for debug image dumps.
//!
//! # Design
//!
//! The annotator is a **global, thread-local singleton** that accumulates
//! detection data (OCR regions, pixel checks, constellation results, grid
//! overlays) as side effects of normal detection logic. Scanner code and
//! detection functions call free functions like `record_ocr()` or
//! `record_constellation()` — these are **no-ops** when annotation is
//! disabled, so they never affect the main code path.
//!
//! # Usage
//!
//! ```ignore
//! // Once at startup:
//! annotator::init(config.dump_images);
//!
//! // Per-item (in worker callback or scan loop):
//! annotator::begin_item("artifacts", index, &scaler);
//! annotator::add_image("panel", &image);
//!
//! // Detection functions record automatically as side effects:
//! let rarity = detect_artifact_rarity(&image, &scaler); // records pixel checks
//! let result = detect_constellation_pixel(&image, &scaler); // records constellation
//!
//! // Scanner records OCR (it knows field names):
//! annotator::record_ocr("name", NAME_RECT, &ocr_text);
//! annotator::set_display("name", &chinese_name);
//!
//! // Finalize:
//! annotator::finalize_success(&json);
//! ```
//!
//! # Threading
//!
//! Each thread has its own context via `thread_local!`. Worker threads
//! processing different items concurrently never interfere with each other.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use image::RgbImage;

use super::coord_scaler::CoordScaler;
use super::debug_dump::DumpCollector;
use super::grid_icon_detector::GridCellAnnotation;
use super::pixel_utils::ConstellationResult;

// ── Global state ─────────────────────────────────────────────────────────────

/// Background write threads. Finalize calls spawn threads for PNG encoding
/// so they don't block the scan loop. Call `flush()` to wait for completion.
static PENDING_WRITES: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static CONTEXT: RefCell<Option<AnnotationContext>> = RefCell::new(None);
}

struct AnnotationContext {
    collector: DumpCollector,
    current_img: usize,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Run `f` on the current thread's annotation context. No-op if disabled or no active item.
fn with_ctx(f: impl FnOnce(&mut AnnotationContext)) {
    if !is_enabled() {
        return;
    }
    CONTEXT.with(|c| {
        if let Some(ref mut ctx) = *c.borrow_mut() {
            f(ctx);
        }
    });
}

// ── Configuration ────────────────────────────────────────────────────────────

/// Enable or disable annotation globally. Call once at startup.
pub fn init(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if annotation is currently enabled.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

// ── Item lifecycle ───────────────────────────────────────────────────────────

/// Begin annotating a new item. Creates the output directory.
///
/// If a previous item was not finalized, its Drop handler writes an error file.
/// Call this at the start of processing each scannable item.
pub fn begin_item(category: &str, index: usize, scaler: &CoordScaler) {
    if !is_enabled() {
        return;
    }
    CONTEXT.with(|c| {
        *c.borrow_mut() = Some(AnnotationContext {
            collector: DumpCollector::new("debug_images", category, index, scaler),
            current_img: 0,
        });
    });
}

/// Register a captured image for annotation. Returns image index.
///
/// Subsequent `record_*` calls are associated with this image until
/// the next `add_image` call.
pub fn add_image(label: &str, image: &RgbImage) {
    with_ctx(|ctx| {
        ctx.current_img = ctx.collector.add_image(label, image);
    });
}

// ── OCR recording ────────────────────────────────────────────────────────────

/// Record an OCR region and its raw text result.
pub fn record_ocr(field: &str, rect: (f64, f64, f64, f64), raw_text: &str) {
    with_ctx(|ctx| {
        ctx.collector.record_ocr(ctx.current_img, field, rect, raw_text);
    });
}

/// Set the final (post-processed) GOOD key / code-level result for a field.
/// Shown in result.txt as "field: raw -> final".
pub fn set_final(field: &str, result: &str) {
    with_ctx(|ctx| {
        ctx.collector.set_final_result(field, result);
    });
}

/// Set the human-readable display name for image annotation (e.g. Chinese name).
/// When different from raw_text, the image label shows "raw -> display".
pub fn set_display(field: &str, display: &str) {
    with_ctx(|ctx| {
        ctx.collector.set_display_result(field, display);
    });
}

/// Set label position to Below for a field (e.g. weapon name).
pub fn set_label_below(field: &str) {
    with_ctx(|ctx| {
        ctx.collector.set_label_below(field);
    });
}

// ── Pixel / detection recording ──────────────────────────────────────────────

/// Record a pixel check result (crosshair annotation).
pub fn record_pixel(field: &str, pos: (f64, f64), rgb: [u8; 3], result_text: &str) {
    with_ctx(|ctx| {
        ctx.collector
            .record_pixel(ctx.current_img, field, pos, rgb, result_text);
    });
}

/// Record a pixel check result with a custom annotation color.
pub fn record_pixel_colored(field: &str, pos: (f64, f64), rgb: [u8; 3], result_text: &str, color: [u8; 3]) {
    with_ctx(|ctx| {
        ctx.collector.record_pixel_colored(
            ctx.current_img, field, pos, rgb, result_text,
            Some(image::Rgb(color)),
        );
    });
}

/// Record constellation detection results.
/// Called automatically by `detect_constellation_pixel()`.
pub fn record_constellation(result: &ConstellationResult) {
    with_ctx(|ctx| {
        ctx.collector
            .record_constellation(ctx.current_img, result);
    });
}

/// Record grid overlay (cell bounding boxes + lock/astral detection).
pub fn record_grid_overlay(
    cells: Vec<GridCellAnnotation>,
    detections: Vec<(usize, bool, bool)>,
) {
    with_ctx(|ctx| {
        ctx.collector
            .record_grid_overlay(ctx.current_img, cells, detections);
    });
}

/// Add a warning message to the result file.
pub fn add_warning(text: &str) {
    with_ctx(|ctx| {
        ctx.collector.add_warning(text);
    });
}

// ── Finalization ─────────────────────────────────────────────────────────────

/// Write all output files for a successfully scanned item.
/// File writing is offloaded to a background thread so the scan loop isn't blocked.
pub fn finalize_success(result_json: &str) {
    if !is_enabled() {
        return;
    }
    let json = result_json.to_string();
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().take() {
            spawn_write(move || ctx.collector.finalize_success(&json));
        }
    });
}

/// Write output files for a failed scan.
pub fn finalize_error(partial_json: Option<&str>, error: &str) {
    if !is_enabled() {
        return;
    }
    let json = partial_json.map(|s| s.to_string());
    let err = error.to_string();
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().take() {
            spawn_write(move || ctx.collector.finalize_error(json.as_deref(), &err));
        }
    });
}

/// Write output files for a skipped item.
/// Skip files are tiny (just a text file), so they're written synchronously.
pub fn finalize_skip(reason: &str) {
    if !is_enabled() {
        return;
    }
    let reason = reason.to_string();
    CONTEXT.with(|c| {
        if let Some(ctx) = c.borrow_mut().take() {
            // Skip is cheap (no PNG encoding), write inline
            ctx.collector.finalize_skip(&reason);
        }
    });
}

/// Wait for all background file writes to complete.
/// Call at the end of scanning to ensure all debug images are flushed.
pub fn flush() {
    if let Ok(mut pending) = PENDING_WRITES.lock() {
        for handle in pending.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Spawn a file-writing task on a background thread.
fn spawn_write(f: impl FnOnce() + Send + 'static) {
    let handle = std::thread::spawn(f);
    if let Ok(mut pending) = PENDING_WRITES.lock() {
        // Clean up finished threads to avoid unbounded growth
        pending.retain(|h| !h.is_finished());
        pending.push(handle);
    }
}

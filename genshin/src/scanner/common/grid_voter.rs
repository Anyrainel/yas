//! Per-page grid-icon voting state machine.
//!
//! Encapsulates the 3-pass voting scheme used by both the artifact/weapon
//! scanners and the lock manager to classify icon status (lock / astral /
//! elixir) across inventory pages.
//!
//! ## Algorithm
//!
//! Three detection passes run at page-relative item indices 0, 13, and 26
//! (evenly spaced within a 40-item page). Each pass reads a full-screen
//! capture and votes on the icon state of every cell on the page; the
//! majority wins.
//!
//! ### Tie-breaking
//!
//! We want every page to end up with exactly 1 or 3 passes — never 2, which
//! produces an ambiguous majority. Items are handled as follows:
//!
//! - **Items 0–12:** have only 1 pass so far → emitted immediately with that
//!   single pass's result.
//! - **Items 13–25:** the 2nd pass has just run — 2 passes give an ambiguous
//!   vote. These items are *deferred* until the 3rd pass (at index 26) runs
//!   and disambiguates.
//! - **Items 26+:** 3 passes done → emitted immediately with the majority
//!   vote, and any deferred items (13–25) are flushed alongside.
//! - **Partial last page** (scan stops before index 26): the caller invokes
//!   [`PagedGridVoter::final_flush`], which tie-breaks by running a 3rd pass
//!   on the last deferred item's own image, then drains everything.
//! - **Early-stop** (e.g. rarity cutoff at item X < 26): the caller invokes
//!   [`PagedGridVoter::early_stop_flush`] with the trigger item's image as
//!   the tie-breaker. The trigger item itself is NOT emitted (the caller
//!   drops it because it failed a filter); only previously-deferred items
//!   are flushed.
//!
//! ## Payload
//!
//! The voter is generic over a caller-supplied payload `T` carried alongside
//! each deferred item. Scanners use `T = ()`; the lock manager uses
//! `T = (row, col)` so it can re-click the same grid cell later to toggle
//! the lock.

use std::sync::Arc;

use image::RgbImage;

use super::coord_scaler::CoordScaler;
use super::grid_icon_detector::{
    GridCellAnnotation, GridIconResult, GridMode, GridPageDetection, ITEMS_PER_PAGE,
};

/// Grid annotation snapshot: cell bounding boxes + per-cell (index, lock, astral).
pub type GridAnnotation = Arc<(Vec<GridCellAnnotation>, Vec<(usize, bool, bool)>)>;

/// An item that is ready to be emitted by the caller (voting has settled).
pub struct ReadyItem<T> {
    pub idx: usize,
    pub image: RgbImage,
    pub metadata: Option<GridIconResult>,
    /// Grid overlay annotation data for the page (shared across items on the same page).
    pub grid_annotation: Option<GridAnnotation>,
    pub payload: T,
}

/// Per-page state held by [`PagedGridVoter`].
struct PageState<T> {
    detection: GridPageDetection,
    passes_done: u32,
    deferred: Vec<(usize, RgbImage, T)>,
}

/// State machine that ingests items page-by-page and returns them in voting
/// order: either immediately (1 or 3 passes done) or after deferral.
pub struct PagedGridVoter<T> {
    mode: GridMode,
    total: usize,
    state: Option<PageState<T>>,
}

impl<T> PagedGridVoter<T> {
    /// Create a voter for `total` items using the given grid `mode`.
    pub fn new(total: usize, mode: GridMode) -> Self {
        Self { mode, total, state: None }
    }

    /// Clear page state. Call when `scan_grid` emits `PageScrolled` (the
    /// next `record` call will lazily initialize fresh state for the new
    /// page).
    pub fn reset_page(&mut self) {
        self.state = None;
    }

    /// Record an item captured on the current page. Returns zero, one, or
    /// many items that are now ready for emission. The returned items may
    /// include the one just recorded and/or previously-deferred items that
    /// became ready on this call.
    pub fn record(
        &mut self,
        idx: usize,
        image: RgbImage,
        payload: T,
        scaler: &CoordScaler,
    ) -> Vec<ReadyItem<T>> {
        let page_start = (idx / ITEMS_PER_PAGE) * ITEMS_PER_PAGE;
        let page_rel = idx - page_start;
        let page_items = (self.total - page_start).min(ITEMS_PER_PAGE);

        // Lazily initialize per-page state.
        if self.state.is_none() {
            self.state = Some(PageState {
                detection: GridPageDetection::with_mode(page_start, page_items, self.mode),
                passes_done: 0,
                deferred: Vec::new(),
            });
        }
        let state = self.state.as_mut().unwrap();

        let mut ready: Vec<ReadyItem<T>> = Vec::new();

        // Run detection pass at scheduled page-relative indices.
        if page_rel == 0 && state.passes_done == 0 {
            state.detection.detect_pass(&image, scaler, idx);
            state.passes_done = 1;
        } else if page_rel == 13 && state.passes_done == 1 {
            state.detection.detect_pass(&image, scaler, idx);
            state.passes_done = 2;
        } else if page_rel == 26 && state.passes_done == 2 {
            state.detection.detect_pass(&image, scaler, idx);
            state.passes_done = 3;
            // Flush deferred items now that we have 3 passes.
            let ann = state.detection.annotation_snapshot().map(Arc::new);
            for (d_idx, d_img, d_payload) in state.deferred.drain(..) {
                let gi = state.detection.get(d_idx);
                ready.push(ReadyItem {
                    idx: d_idx,
                    image: d_img,
                    metadata: gi,
                    grid_annotation: ann.clone(),
                    payload: d_payload,
                });
            }
        }

        // Decide whether to defer or emit the current item immediately.
        if state.passes_done == 2 && page_rel >= 13 {
            // Exactly 2 passes → defer until pass 3.
            state.deferred.push((idx, image, payload));
        } else {
            // Either 1 pass (items 0–12) or 3 passes (items 26+).
            let gi = state.detection.get(idx);
            let ann = state.detection.annotation_snapshot().map(Arc::new);
            ready.push(ReadyItem { idx, image, metadata: gi, grid_annotation: ann, payload });
        }

        ready
    }

    /// Tie-break with `trigger_image` and flush all deferred items. Use
    /// when an early-stop condition is detected on an item that the caller
    /// will NOT emit itself (e.g. a rarity cutoff trigger).
    ///
    /// If fewer than 2 passes have run, no tie-break is needed — deferred
    /// is empty in that case because items 0–12 are emitted immediately.
    pub fn early_stop_flush(
        &mut self,
        trigger_image: &RgbImage,
        trigger_idx: usize,
        scaler: &CoordScaler,
    ) -> Vec<ReadyItem<T>> {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return Vec::new(),
        };
        if state.passes_done == 2 {
            state.detection.detect_pass(trigger_image, scaler, trigger_idx);
            state.passes_done = 3;
        }
        let ann = state.detection.annotation_snapshot().map(Arc::new);
        let mut ready = Vec::with_capacity(state.deferred.len());
        for (d_idx, d_img, d_payload) in state.deferred.drain(..) {
            let gi = state.detection.get(d_idx);
            ready.push(ReadyItem {
                idx: d_idx,
                image: d_img,
                metadata: gi,
                grid_annotation: ann.clone(),
                payload: d_payload,
            });
        }
        ready
    }

    /// Final flush at end-of-scan. If the current page never reached the
    /// 3rd pass, tie-break by running pass 3 on the last deferred item's
    /// own image, then drain everything.
    pub fn final_flush(&mut self, scaler: &CoordScaler) -> Vec<ReadyItem<T>> {
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return Vec::new(),
        };
        if state.passes_done == 2 && !state.deferred.is_empty() {
            // Clone the last deferred image so we can pass it to detect_pass
            // without holding a borrow on state.deferred.
            let (last_idx, last_img) = {
                let last = state.deferred.last().unwrap();
                (last.0, last.1.clone())
            };
            state.detection.detect_pass(&last_img, scaler, last_idx);
            state.passes_done = 3;
        }
        let ann = state.detection.annotation_snapshot().map(Arc::new);
        let mut ready = Vec::with_capacity(state.deferred.len());
        for (d_idx, d_img, d_payload) in state.deferred.drain(..) {
            let gi = state.detection.get(d_idx);
            ready.push(ReadyItem {
                idx: d_idx,
                image: d_img,
                metadata: gi,
                grid_annotation: ann.clone(),
                payload: d_payload,
            });
        }
        ready
    }
}

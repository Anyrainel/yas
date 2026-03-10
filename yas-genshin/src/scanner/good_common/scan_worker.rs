use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use image::RgbImage;
use indicatif::{ProgressBar, ProgressStyle};
use log::{error, warn};

/// A work item sent from the capture thread to the worker pool.
pub struct WorkItem<M: Send> {
    pub index: usize,
    pub image: RgbImage,
    pub metadata: M,
}

/// Handle to a running worker. Call `join()` to wait for results.
pub struct WorkerHandle<R> {
    handle: std::thread::JoinHandle<Vec<R>>,
    /// Set to true by the worker when it detects problems (e.g., consecutive
    /// duplicates). The capture thread should check this periodically and stop.
    pub should_stop: Arc<AtomicBool>,
}

impl<R> WorkerHandle<R> {
    /// Wait for the worker to finish and return ordered results.
    pub fn join(self) -> Vec<R> {
        self.handle.join().expect("worker thread panicked")
    }

    /// Check if the worker has signaled that scanning should stop.
    pub fn stop_requested(&self) -> bool {
        self.should_stop.load(Ordering::Relaxed)
    }
}

/// Start a parallel scan worker.
///
/// Items are received via the returned sender, dispatched to rayon for
/// parallel processing, and results collected in index order.
///
/// The `process_fn` receives a `WorkItem` and returns:
/// - `Ok(Some(result))` — include in output
/// - `Ok(None)` — skip this item (e.g., non-artifact)
/// - `Err(e)` — log error, skip item
///
/// The worker shows an indicatif progress bar and can signal the capture
/// thread to stop via `WorkerHandle::should_stop`.
pub fn start_worker<M, R, F>(
    total: usize,
    process_fn: F,
) -> (crossbeam_channel::Sender<WorkItem<M>>, WorkerHandle<R>)
where
    M: Send + 'static,
    R: Send + 'static,
    F: Fn(WorkItem<M>) -> anyhow::Result<Option<R>> + Send + Sync + 'static,
{
    // Bounded channel prevents memory blowup if OCR falls behind capture.
    // Buffer of 16 items ≈ 16 × ~1MB = ~16MB max in-flight images.
    let (item_tx, item_rx) = crossbeam_channel::bounded::<WorkItem<M>>(16);
    let should_stop = Arc::new(AtomicBool::new(false));
    let should_stop_clone = should_stop.clone();

    let handle = std::thread::spawn(move || {
        let process_fn = Arc::new(process_fn);

        // Result channel: rayon tasks send (index, result) here
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<(usize, anyhow::Result<Option<R>>)>();

        // Dispatch: receive items and spawn rayon tasks
        let dispatch_result_tx = result_tx.clone();
        let dispatch_handle = std::thread::spawn(move || {
            for item in item_rx {
                let process_fn = process_fn.clone();
                let tx = dispatch_result_tx.clone();
                let index = item.index;
                rayon::spawn(move || {
                    let result = process_fn(item);
                    let _ = tx.send((index, result));
                });
            }
            // Drop our sender so result_rx eventually closes
            drop(dispatch_result_tx);
        });
        // Drop the original sender clone
        drop(result_tx);

        // Collection: reorder results via BTreeMap
        let pb = ProgressBar::new(total as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );

        let mut results_map: BTreeMap<usize, anyhow::Result<Option<R>>> = BTreeMap::new();
        let mut next_index: usize = 0;
        let mut output: Vec<R> = Vec::new();
        let mut consecutive_errors: usize = 0;

        for (index, result) in result_rx {
            results_map.insert(index, result);

            // Drain consecutive ready results
            while let Some(result) = results_map.remove(&next_index) {
                pb.inc(1);
                next_index += 1;

                match result {
                    Ok(Some(item)) => {
                        output.push(item);
                        consecutive_errors = 0;
                    }
                    Ok(None) => {
                        // Skipped item
                        consecutive_errors = 0;
                    }
                    Err(e) => {
                        error!("[worker] item {} error: {}", next_index - 1, e);
                        consecutive_errors += 1;
                        if consecutive_errors >= 10 {
                            error!("[worker] {} consecutive errors, signaling stop", consecutive_errors);
                            should_stop_clone.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
        }

        // Drain any remaining buffered results
        while let Some(result) = results_map.remove(&next_index) {
            pb.inc(1);
            next_index += 1;
            if let Ok(Some(item)) = result {
                output.push(item);
            }
        }

        pb.finish_with_message("done");
        let _ = dispatch_handle.join();

        output
    });

    (item_tx, WorkerHandle { handle, should_stop })
}

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use image::RgbImage;

use yas::ocr::ImageToText;
use crate::scanner::common::ocr_factory;

/// A pool of OCR model instances for true parallel OCR.
///
/// Each `PPOCRModel` uses `Mutex<Session>` internally, so a single instance
/// serializes all OCR calls. By creating N instances (~16MB each), N rayon
/// tasks can run OCR simultaneously.
///
/// Uses a crossbeam bounded channel as the pool: checkout blocks until a
/// model is available, and the `OcrGuard` returns it on drop.
pub struct OcrPool {
    checkout: crossbeam_channel::Receiver<Box<dyn ImageToText<RgbImage> + Send>>,
    checkin: crossbeam_channel::Sender<Box<dyn ImageToText<RgbImage> + Send>>,
}

impl OcrPool {
    /// Create a pool with `count` model instances.
    ///
    /// `create_fn` is called `count` times to create independent model instances.
    pub fn new<F>(create_fn: F, count: usize) -> Result<Self>
    where
        F: Fn() -> Result<Box<dyn ImageToText<RgbImage> + Send>>,
    {
        let (checkin, checkout) = crossbeam_channel::bounded(count);
        for _ in 0..count {
            checkin.send(create_fn()?).map_err(|_| anyhow::anyhow!("OCR池通道已关闭 / Pool channel closed"))?;
        }
        Ok(Self { checkout, checkin })
    }

    /// Checkout a model from the pool. Blocks until one is available.
    /// The model is returned to the pool when the guard is dropped.
    pub fn get(&self) -> OcrGuard {
        let model = self.checkout.recv().expect("OCR池通道已关闭 / OCR pool channel closed");
        OcrGuard {
            model: Some(model),
            checkin: self.checkin.clone(),
        }
    }
}

/// RAII guard that returns the OCR model to the pool on drop.
pub struct OcrGuard {
    model: Option<Box<dyn ImageToText<RgbImage> + Send>>,
    checkin: crossbeam_channel::Sender<Box<dyn ImageToText<RgbImage> + Send>>,
}

impl ImageToText<RgbImage> for OcrGuard {
    fn image_to_text(&self, image: &RgbImage, is_preprocessed: bool) -> Result<String> {
        self.model
            .as_ref()
            .expect("OCR模型已被取走 / OcrGuard model already taken")
            .image_to_text(image, is_preprocessed)
    }

    fn get_average_inference_time(&self) -> Option<Duration> {
        self.model.as_ref().and_then(|m| m.get_average_inference_time())
    }
}

// Safety: OcrGuard holds a Box<dyn ImageToText<RgbImage> + Send> which is Send.
// The crossbeam Sender is Send + Sync. OcrGuard is only used from the thread
// that checked it out, but we need Sync for the ImageToText trait bound.
unsafe impl Sync for OcrGuard {}

impl Drop for OcrGuard {
    fn drop(&mut self) {
        if let Some(model) = self.model.take() {
            let _ = self.checkin.send(model);
        }
    }
}

/// OCR pool sizing based on available system memory.
///
/// Two tiers:
/// - Normal (≥8 GB available): 2 v5 + 4 v4
/// - Small  (<8 GB available): 1 v5 + 2 v4
#[derive(Clone, Debug)]
pub struct OcrPoolConfig {
    pub v5_count: usize,
    pub v4_count: usize,
}

impl OcrPoolConfig {
    /// Detect available memory and choose pool sizes.
    pub fn detect() -> Self {
        const EIGHT_GB: u64 = 8 * 1024 * 1024 * 1024;

        let available = yas::utils::available_memory_bytes();
        let (v5_count, v4_count) = match available {
            Some(bytes) if bytes < EIGHT_GB => {
                let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                log::info!(
                    "可用内存 {:.1} GB < 8 GB，使用小型OCR池 (1×v5 + 2×v4) / \
                     Available memory {:.1} GB < 8 GB, using small OCR pool (1×v5 + 2×v4)",
                    gb, gb,
                );
                (1, 2)
            }
            Some(bytes) => {
                let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                log::info!(
                    "可用内存 {:.1} GB，使用标准OCR池 (2×v5 + 4×v4) / \
                     Available memory {:.1} GB, using normal OCR pool (2×v5 + 4×v4)",
                    gb, gb,
                );
                (2, 4)
            }
            None => {
                log::warn!(
                    "无法检测内存，使用标准OCR池 (2×v5 + 4×v4) / \
                     Cannot detect memory, using normal OCR pool (2×v5 + 4×v4)",
                );
                (2, 4)
            }
        };

        Self { v5_count, v4_count }
    }
}

/// Shared OCR model pools for the entire scan session.
///
/// Created once, passed by reference to all scanners and managers.
/// Eliminates per-scanner pool creation/destruction overhead and
/// prevents OOM on low-memory systems.
///
/// The v5 pool is also used for one-off OCR tasks (e.g., reading
/// the backpack item count) — just call `v5().get()`.
pub struct SharedOcrPools {
    v5_pool: Arc<OcrPool>,
    v4_pool: Arc<OcrPool>,
    config: OcrPoolConfig,
}

impl SharedOcrPools {
    /// Create shared pools with the given config.
    ///
    /// `v5_backend` and `v4_backend` are the backend strings
    /// (e.g., "ppocrv5", "ppocrv4").
    pub fn new(config: OcrPoolConfig, v5_backend: &str, v4_backend: &str) -> Result<Self> {
        let v5_be = v5_backend.to_string();
        let v5_pool = Arc::new(OcrPool::new(
            move || ocr_factory::create_ocr_model(&v5_be),
            config.v5_count,
        )?);

        let v4_be = v4_backend.to_string();
        let v4_pool = Arc::new(OcrPool::new(
            move || ocr_factory::create_ocr_model(&v4_be),
            config.v4_count,
        )?);

        log::info!(
            "OCR池已创建: v5={}, v4={} / OCR pools created: v5={}, v4={}",
            config.v5_count, config.v4_count,
            config.v5_count, config.v4_count,
        );

        Ok(Self { v5_pool, v4_pool, config })
    }

    pub fn v5(&self) -> &Arc<OcrPool> {
        &self.v5_pool
    }

    pub fn v4(&self) -> &Arc<OcrPool> {
        &self.v4_pool
    }

    pub fn config(&self) -> &OcrPoolConfig {
        &self.config
    }
}

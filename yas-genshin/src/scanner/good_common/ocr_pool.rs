use std::time::Duration;

use anyhow::Result;
use image::RgbImage;

use yas::ocr::ImageToText;

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
            checkin.send(create_fn()?).map_err(|_| anyhow::anyhow!("pool channel closed"))?;
        }
        Ok(Self { checkout, checkin })
    }

    /// Checkout a model from the pool. Blocks until one is available.
    /// The model is returned to the pool when the guard is dropped.
    pub fn get(&self) -> OcrGuard {
        let model = self.checkout.recv().expect("OCR pool channel closed");
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
            .expect("OcrGuard model already taken")
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

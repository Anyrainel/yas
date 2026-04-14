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
/// - Small  (<8 GB or unknown): 1 v5 + 1 v4
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
                log::debug!(
                    "可用内存 {:.1} GB < 8 GB，使用小型OCR池 (1×v5 + 1×v4) / \
                     Available memory {:.1} GB < 8 GB, using small OCR pool (1×v5 + 1×v4)",
                    gb, gb,
                );
                (1, 1)
            }
            Some(bytes) => {
                let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                log::debug!(
                    "可用内存 {:.1} GB，使用标准OCR池 (2×v5 + 4×v4) / \
                     Available memory {:.1} GB, using normal OCR pool (2×v5 + 4×v4)",
                    gb, gb,
                );
                (2, 4)
            }
            None => {
                log::debug!(
                    "无法检测内存，使用小型OCR池 (1×v5 + 1×v4) / \
                     Cannot detect memory, using small OCR pool (1×v5 + 1×v4)",
                );
                (1, 1)
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
    ///
    /// The first `create_ocr_model` call triggers `ort`'s lazy DLL load.
    /// If the DLL or its dependencies (VC++ runtime) are missing, `ort`
    /// **panics** rather than returning an error.  We catch this with
    /// `catch_unwind` and convert it to a diagnosed error.
    pub fn new(config: OcrPoolConfig, v5_backend: &str, v4_backend: &str) -> Result<Self> {
        let v5_be = v5_backend.to_string();
        let v5_pool = Arc::new(create_pool_caught(
            move || ocr_factory::create_ocr_model(&v5_be),
            config.v5_count,
            "v5",
        )?);

        let v4_be = v4_backend.to_string();
        let v4_pool = Arc::new(create_pool_caught(
            move || ocr_factory::create_ocr_model(&v4_be),
            config.v4_count,
            "v4",
        )?);

        log::debug!(
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

/// Create an OcrPool, catching both `Result::Err` and panics from `ort`.
///
/// `ort`'s `load-dynamic` feature panics (not errors) when the DLL fails
/// to load.  The panic message is:
///   "An error occurred while attempting to load the ONNX Runtime binary
///    at `{path}`: LoadLibraryExW failed"
/// This happens on the first `Session::builder()` call, which triggers
/// the lazy `libloading::Library::new()`.
fn create_pool_caught<F>(create_fn: F, count: usize, label: &str) -> Result<OcrPool>
where
    F: Fn() -> Result<Box<dyn ImageToText<RgbImage> + Send>> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        OcrPool::new(&create_fn, count)
    })) {
        Ok(result) => result.map_err(|e| diagnose_error(format!("{:#}", e))),
        Err(panic_payload) => {
            let panic_msg = match panic_payload.downcast_ref::<String>() {
                Some(s) => s.clone(),
                None => match panic_payload.downcast_ref::<&str>() {
                    Some(s) => s.to_string(),
                    None => "unknown panic".to_string(),
                },
            };
            Err(diagnose_error(format!("[{}] panic: {}", label, panic_msg)))
        }
    }
}

/// Log an OCR initialization failure and append actionable hints based on
/// the error message content.
///
/// Known failure modes (from ort 2.0.0-rc.10 with load-dynamic):
///
/// | Source | Pattern | Cause |
/// |--------|---------|-------|
/// | panic  | "LoadLibraryExW failed" | DLL or dependency missing (VC++ runtime) |
/// | panic  | "is not compatible with the ONNX Runtime binary" | DLL version mismatch |
/// | panic  | "OrtGetApiBase" | Not a valid ONNX Runtime DLL |
/// | error  | "protobuf parsing failed" / "could not parse model" | Corrupt ONNX model |
/// | error  | "bad_alloc" | Out of memory |
/// | error  | "not supported in this build" | ORT format version mismatch |
fn diagnose_error(msg: String) -> anyhow::Error {
    // Always log the raw error — essential for remote debugging
    log::error!("OCR模型加载失败 / OCR model failed to load: {}", msg);

    let lower = msg.to_lowercase();
    const VCPP_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

    // DLL loading failure (panic from ort's load-dynamic).
    // By this point ensure_onnxruntime() confirmed onnxruntime.dll exists,
    // so "LoadLibraryExW failed" means a *dependency* is missing — almost
    // always the VC++ runtime.
    if lower.contains("loadlibraryexw failed") || lower.contains("loadlibrary") {
        log::error!(
            "onnxruntime.dll 加载失败，最常见原因：缺少 Visual C++ 运行库 / \
             onnxruntime.dll failed to load. Most common cause: missing Visual C++ runtime"
        );
        log::error!(
            "请安装 VC++ 2015-2022 Redistributable (x64) / \
             Install VC++ 2015-2022 Redistributable (x64): {}", VCPP_URL
        );
        log::error!(
            "若已安装，请删除 onnxruntime.dll 后重启程序以重新下载 / \
             If already installed, delete onnxruntime.dll and restart to re-download"
        );
        return anyhow::anyhow!("{}", msg);
    }

    // DLL version mismatch
    if lower.contains("is not compatible with the onnx runtime binary")
        || lower.contains("not supported in this build")
    {
        log::error!(
            "onnxruntime.dll 版本不兼容，请删除后重启程序以重新下载正确版本 / \
             onnxruntime.dll version is incompatible. Delete it and restart to re-download the correct version"
        );
        return anyhow::anyhow!("{}", msg);
    }

    // Corrupt or invalid DLL
    if lower.contains("ortgetapibase") {
        log::error!(
            "onnxruntime.dll 文件损坏或无效，请删除后重启程序以重新下载 / \
             onnxruntime.dll is corrupt or invalid. Delete it and restart to re-download"
        );
        return anyhow::anyhow!("{}", msg);
    }

    // Corrupt ONNX model (embedded at compile time — should never happen,
    // but could indicate a bad build)
    if lower.contains("protobuf parsing failed")
        || lower.contains("could not parse model")
        || lower.contains("model verification failed")
    {
        log::error!(
            "OCR模型文件损坏，请重新下载本程序 / \
             OCR model data is corrupt. Please re-download this program"
        );
        return anyhow::anyhow!("{}", msg);
    }

    // Out of memory
    if lower.contains("bad_alloc") || lower.contains("out of memory") {
        log::error!(
            "内存不足，无法加载OCR模型。请关闭其他程序后重试 / \
             Out of memory loading OCR model. Close other programs and try again"
        );
        return anyhow::anyhow!("{}", msg);
    }

    // Unknown error — raw message already logged above
    anyhow::anyhow!("{}", msg)
}

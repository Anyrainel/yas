use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use image::{ImageBuffer, RgbImage};
use windows::Win32::Foundation::HWND;
use windows_capture::{
    capture::{CaptureControl, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{ColorFormat, CursorCaptuerSettings, DrawBorderSettings, Settings},
    window::Window,
};

use crate::capture::Capturer;
use crate::positioning::{Pos, Rect};

/// Raw pixel data from a captured frame (BGRA, no row-padding).
struct FrameData {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
    id: u64,
}

struct SharedInner {
    frame: Option<FrameData>,
    next_id: u64,
}

type SharedState = Arc<(Mutex<SharedInner>, Condvar)>;

/// Callback handler for the WGC capture session.
struct WgcHandler {
    state: SharedState,
}

impl GraphicsCaptureApiHandler for WgcHandler {
    type Flags = SharedState;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(flags: Self::Flags) -> std::result::Result<Self, Self::Error> {
        Ok(Self { state: flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        let width = frame.width();
        let height = frame.height();
        let mut buf = frame.buffer()?;

        let has_padding = buf.has_padding();
        let row_pitch = buf.row_pitch() as usize;
        let row_bytes = (width * 4) as usize;

        let raw = buf.as_raw_buffer();
        let bgra = if !has_padding {
            raw.to_vec()
        } else {
            let mut result = Vec::with_capacity(row_bytes * height as usize);
            for y in 0..height as usize {
                let start = y * row_pitch;
                result.extend_from_slice(&raw[start..start + row_bytes]);
            }
            result
        };

        let (lock, cvar) = &*self.state;
        let mut inner = lock.lock().unwrap();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.frame = Some(FrameData { bgra, width, height, id });
        cvar.notify_all();

        Ok(())
    }
}

/// Windows Graphics Capture API-based capturer.
///
/// Captures the game window directly via DXGI, bypassing desktop composition
/// effects like Night Light, color filters, and HDR tone mapping.
pub struct WgcCapturer {
    state: SharedState,
    _control: CaptureControl<WgcHandler, Box<dyn std::error::Error + Send + Sync>>,
    window_left: i32,
    window_top: i32,
    /// DPI scale factor: frame_size / expected_size. 1.0 when no mismatch.
    dpi_scale: f64,
    /// Frame ID of the last frame returned by capture_rect.
    last_frame_id: AtomicU64,
}

impl WgcCapturer {
    /// Create a new WGC capturer targeting the given window.
    ///
    /// Starts a persistent background capture session and waits for the first
    /// frame (up to 2 seconds). Returns an error if WGC is unavailable or the
    /// window cannot be captured.
    pub fn new(
        hwnd: isize,
        window_left: i32,
        window_top: i32,
        expected_width: u32,
        expected_height: u32,
    ) -> Result<Self> {
        let window = Window::from_raw_hwnd(HWND(hwnd));

        let state: SharedState = Arc::new((
            Mutex::new(SharedInner {
                frame: None,
                next_id: 1,
            }),
            Condvar::new(),
        ));

        let settings = Settings::new(
            window,
            CursorCaptuerSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            ColorFormat::Bgra8,
            state.clone(),
        )
        .map_err(|e| anyhow!("WGC设置失败 / WGC settings error: {}", e))?;

        let control = WgcHandler::start_free_threaded(settings)
            .map_err(|e| anyhow!("WGC启动失败 / WGC start error: {}", e))?;

        // Wait for the first frame (up to 2 seconds).
        let dpi_scale;
        {
            let (lock, cvar) = &*state;
            let guard = lock.lock().unwrap();
            let guard = if guard.frame.is_none() {
                let (g, timeout) = cvar
                    .wait_timeout(guard, Duration::from_secs(2))
                    .unwrap();
                if g.frame.is_none() && timeout.timed_out() {
                    drop(g);
                    control.stop().ok();
                    return Err(anyhow!(
                        "WGC: 2秒内未收到帧 / no frame received within 2 seconds"
                    ));
                }
                g
            } else {
                guard
            };

            // Detect DPI mismatch between frame and expected window size.
            let frame = guard.frame.as_ref().unwrap();
            let scale_x = frame.width as f64 / expected_width as f64;
            let scale_y = frame.height as f64 / expected_height as f64;
            if (scale_x - 1.0).abs() > 0.01 || (scale_y - 1.0).abs() > 0.01 {
                log::warn!(
                    "WGC帧尺寸 {}x{} 与窗口尺寸 {}x{} 不一致 (缩放={:.3}x{:.3})，\
                     可能为DPI缩放导致。将自动补偿坐标。 / \
                     WGC frame {}x{} differs from window {}x{} (scale={:.3}x{:.3}), \
                     likely DPI scaling. Coordinates will be compensated.",
                    frame.width, frame.height, expected_width, expected_height,
                    scale_x, scale_y,
                    frame.width, frame.height, expected_width, expected_height,
                    scale_x, scale_y,
                );
            }
            // Use X scale (should equal Y for 16:9).
            dpi_scale = scale_x;
        }

        Ok(Self {
            state,
            _control: control,
            window_left,
            window_top,
            dpi_scale,
            last_frame_id: AtomicU64::new(0),
        })
    }

    /// Wait for a frame newer than the last one returned by capture_rect,
    /// then crop. Used by capture_rect to ensure panel-load detection gets
    /// genuinely distinct frames.
    fn crop_fresh(
        &self,
        rel_x: u32,
        rel_y: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbImage> {
        let (lock, cvar) = &*self.state;
        let last_id = self.last_frame_id.load(Ordering::Acquire);

        let inner = lock.lock().unwrap();
        // If we haven't read any frame yet, or a newer frame is already available, proceed.
        let inner = if last_id > 0
            && inner.frame.as_ref().map_or(true, |f| f.id <= last_id)
        {
            // Wait up to 50ms for a new frame (~3 frames at 60fps).
            let (g, _) = cvar
                .wait_timeout(inner, Duration::from_millis(50))
                .unwrap();
            g
        } else {
            inner
        };

        let frame = inner
            .frame
            .as_ref()
            .ok_or_else(|| anyhow!("WGC: 无可用帧 / no frame available"))?;

        let s = self.dpi_scale;
        let start_x = ((rel_x as f64 * s) as u32).min(frame.width);
        let start_y = ((rel_y as f64 * s) as u32).min(frame.height);
        let scaled_w = (w as f64 * s) as u32;
        let scaled_h = (h as f64 * s) as u32;
        let end_x = (start_x + scaled_w).min(frame.width);
        let end_y = (start_y + scaled_h).min(frame.height);
        let actual_w = end_x.saturating_sub(start_x);
        let actual_h = end_y.saturating_sub(start_y);

        if actual_w == 0 || actual_h == 0 {
            return Err(anyhow!(
                "WGC: 截取区域超出窗口范围 / capture rect outside window bounds"
            ));
        }

        self.last_frame_id.store(frame.id, Ordering::Release);

        let fw = frame.width;
        let bgra = &frame.bgra;

        if (s - 1.0).abs() <= 0.01 {
            let img = ImageBuffer::from_fn(actual_w, actual_h, |x, y| {
                let px = start_x + x;
                let py = start_y + y;
                let idx = ((py * fw + px) * 4) as usize;
                image::Rgb([bgra[idx + 2], bgra[idx + 1], bgra[idx]])
            });
            Ok(img)
        } else {
            let img = ImageBuffer::from_fn(w, h, |x, y| {
                let px = start_x + (x as f64 * s) as u32;
                let py = start_y + (y as f64 * s) as u32;
                let px = px.min(frame.width - 1);
                let py = py.min(frame.height - 1);
                let idx = ((py * fw + px) * 4) as usize;
                image::Rgb([bgra[idx + 2], bgra[idx + 1], bgra[idx]])
            });
            Ok(img)
        }
    }
}

impl Capturer<RgbImage> for WgcCapturer {
    fn capture_rect(&self, rect: Rect<i32>) -> Result<RgbImage> {
        let rel_x = (rect.left - self.window_left).max(0) as u32;
        let rel_y = (rect.top - self.window_top).max(0) as u32;
        // Always wait for a fresh frame so that panel-load detection
        // (which compares consecutive captures) never sees the same frame twice.
        self.crop_fresh(rel_x, rel_y, rect.width as u32, rect.height as u32)
    }

    fn capture_color(&self, pos: Pos<i32>) -> Result<image::Rgb<u8>> {
        let rx = (pos.x - self.window_left).max(0) as f64;
        let ry = (pos.y - self.window_top).max(0) as f64;

        let (lock, _) = &*self.state;
        let inner = lock.lock().unwrap();
        let frame = inner
            .frame
            .as_ref()
            .ok_or_else(|| anyhow!("WGC: 无可用帧 / no frame available"))?;

        // Apply DPI compensation.
        let x = (rx * self.dpi_scale) as u32;
        let y = (ry * self.dpi_scale) as u32;

        if x >= frame.width || y >= frame.height {
            return Err(anyhow!(
                "WGC: 像素超出窗口范围 / pixel outside window bounds"
            ));
        }

        let idx = ((y * frame.width + x) * 4) as usize;
        Ok(image::Rgb([frame.bgra[idx + 2], frame.bgra[idx + 1], frame.bgra[idx]]))
    }
}

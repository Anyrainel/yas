pub use stream_capturer::StreamingCapturer;
pub use capturer::Capturer;
pub use generic_capturer::GenericCapturer;

mod capturer;
mod generic_capturer;
mod stream_capturer;

// windows

#[cfg(target_os = "windows")]
mod screenshots_capturer;
#[cfg(target_os = "windows")]
mod winapi_capturer;
#[cfg(target_os = "windows")]
mod windows_capturer;

#[cfg(target_os = "windows")]
pub use screenshots_capturer::ScreenshotsCapturer;
#[cfg(target_os = "windows")]
pub use winapi_capturer::WinapiCapturer;
#[cfg(target_os = "windows")]
pub use windows_capturer::WindowsCapturer;

// linux
#[cfg(all(target_os = "linux", feature = "capturer_libwayshot"))]
mod libwayshot_capturer;

#[cfg(all(target_os = "linux", feature = "capturer_libwayshot"))]
pub use libwayshot_capturer::LibwayshotCapturer;

#[cfg(all(target_os = "linux", feature = "capturer_screenshots"))]
mod screenshots_capturer;

#[cfg(all(target_os = "linux", feature = "capturer_screenshots"))]
pub use screenshots_capturer::ScreenshotsCapturer;

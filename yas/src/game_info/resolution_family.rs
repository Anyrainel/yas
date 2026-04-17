use crate::positioning::Size;

/// Check whether a window size has a 16:9 aspect ratio.
///
/// This is the only aspect ratio supported by the scanner — all OCR
/// coordinates and UI positions are calibrated against 1920×1080.
pub fn is_16x9(size: Size<usize>) -> bool {
    let h = size.height as u32;
    let w = size.width as u32;
    h * 16 == w * 9
}

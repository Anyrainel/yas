//! Global language setting for bilingual log messages.
//!
//! Log messages use the convention `"中文 / English"`. The logger calls
//! [`localize`] to pick the correct half based on the global setting.

use std::sync::atomic::{AtomicU8, Ordering};

static LANG: AtomicU8 = AtomicU8::new(0); // 0 = zh, 1 = en

/// Set the global language. Call once at startup.
/// Accepts `"en"` for English; anything else defaults to Chinese.
pub fn set_lang(lang: &str) {
    LANG.store(if lang == "en" { 1 } else { 0 }, Ordering::Relaxed);
}

/// Returns `"zh"` or `"en"`.
pub fn get_lang() -> &'static str {
    if LANG.load(Ordering::Relaxed) == 1 { "en" } else { "zh" }
}

/// Returns true if the current language is English.
pub fn is_en() -> bool {
    LANG.load(Ordering::Relaxed) == 1
}

/// Pick the correct language half from a bilingual `"中文 / English"` string.
///
/// Splits on the first `" / "` occurrence. If no separator is found,
/// returns the original string unchanged.
pub fn localize(msg: &str) -> String {
    if let Some(idx) = msg.find(" / ") {
        if is_en() {
            msg[idx + 3..].to_string()
        } else {
            msg[..idx].to_string()
        }
    } else {
        msg.to_string()
    }
}

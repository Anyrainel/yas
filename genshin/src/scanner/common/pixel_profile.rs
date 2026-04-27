use std::sync::atomic::{AtomicBool, Ordering};

static HDR_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_hdr_mode(enabled: bool) {
    HDR_MODE.store(enabled, Ordering::Relaxed);
}

pub fn is_hdr_mode() -> bool {
    HDR_MODE.load(Ordering::Relaxed)
}

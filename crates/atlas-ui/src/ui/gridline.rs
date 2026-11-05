//! Gridline geometry shared by retained controls and immediate paint surfaces.
use std::sync::atomic::{AtomicBool, Ordering};
static ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) fn is_theme(name: &str) -> bool {
    matches!(name, "Gridline Light" | "Gridline Dark")
}
pub(crate) fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}
pub(crate) fn radius(value: f32) -> gpui::Pixels {
    gpui::px(if active() { 0.0 } else { value })
}

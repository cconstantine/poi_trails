//! Thin wrappers over the browser Fullscreen API, wasm/web only.
//!
//! These must be triggered from a user gesture (a button click). egui runs its
//! logic a frame after the pointer event, but that's still inside the browser's
//! transient-activation window, so requesting fullscreen from a `clicked()`
//! handler works.

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// The eframe render canvas — the element we take fullscreen so only the video fills the screen.
fn canvas() -> Option<web_sys::Element> {
    document()?.get_element_by_id("the_canvas_id")
}

/// Is the page currently in browser fullscreen?
pub fn is_active() -> bool {
    document().and_then(|d| d.fullscreen_element()).is_some()
}

pub fn request() {
    if let Some(canvas) = canvas() {
        // Ignore the error: some browsers/contexts reject fullscreen (e.g. no
        // transient activation, or a permissions policy); the caller still hides
        // the controls, so the user gets a full-window view either way.
        let _ = canvas.request_fullscreen();
    }
}

pub fn exit() {
    if is_active() {
        if let Some(doc) = document() {
            doc.exit_fullscreen();
        }
    }
}

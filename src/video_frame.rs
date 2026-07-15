// `VideoFrame` feeds the native CPU path; the web build uploads to the GPU and
// doesn't use it (CameraDevice, below, is still used on web).
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[derive(Clone)]
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    /// Tightly packed RGBA8 pixels, row-major, length == width * height * 4.
    pub rgba: Vec<u8>,
}

impl VideoFrame {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; width * height * 4],
        }
    }
}

/// A selectable camera (web only).
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraDevice {
    pub device_id: String,
    pub label: String,
}

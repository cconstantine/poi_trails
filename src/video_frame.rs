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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraDevice {
    pub device_id: String,
    pub label: String,
}

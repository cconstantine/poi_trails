use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::delay::DelayBuffer;
use crate::trails::{
    TrailsProcessor, DEFAULT_BACKGROUND_SECONDS, DEFAULT_DIM_FACTOR, DEFAULT_FADE_SECONDS,
    DEFAULT_INTENSITY_GAIN, DEFAULT_MOTION_GATE, DEFAULT_MOTION_SENSITIVITY, DEFAULT_THRESHOLD,
};
use crate::video_frame::VideoFrame;

#[cfg(target_arch = "wasm32")]
use crate::camera::{CameraState, CameraStatus};
#[cfg(target_arch = "wasm32")]
use crate::video_frame::CameraDevice;

const DEFAULT_WIDTH: usize = 640;
const DEFAULT_HEIGHT: usize = 480;

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    /// Plain live feed (optionally flipped), no trail accumulation.
    Live,
    Trails,
}

/// Persisted to `localStorage` on web (a file on native) via eframe's
/// built-in `Storage`. Deliberately excludes `mirror_enabled`, which the app
/// always starts with on.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    camera_enabled: bool,
    trails_enabled: bool,
    threshold: f32,
    intensity_gain: f32,
    fade_seconds: f32,
    dim_factor: f32,
    motion_gate: bool,
    motion_sensitivity: f32,
    background_seconds: f32,
    delay_seconds: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            camera_enabled: false,
            trails_enabled: true,
            threshold: DEFAULT_THRESHOLD,
            intensity_gain: DEFAULT_INTENSITY_GAIN,
            fade_seconds: DEFAULT_FADE_SECONDS,
            dim_factor: DEFAULT_DIM_FACTOR,
            motion_gate: DEFAULT_MOTION_GATE,
            motion_sensitivity: DEFAULT_MOTION_SENSITIVITY,
            background_seconds: DEFAULT_BACKGROUND_SECONDS,
            delay_seconds: 0.0,
        }
    }
}

pub struct PoiTrailsApp {
    pub(crate) mode: Mode,
    pub(crate) mirror_enabled: bool,
    pub(crate) trails: TrailsProcessor,
    /// Playback delay in seconds (0 = live). See [`DelayBuffer`].
    pub(crate) delay_seconds: f32,
    delay: DelayBuffer,
    /// When false the side panel is hidden and only a small floating "show
    /// controls" button remains, for a clean full-window/fullscreen view.
    pub(crate) show_controls: bool,
    texture: Option<egui::TextureHandle>,
    composite_buf: Vec<u8>,

    #[cfg(target_arch = "wasm32")]
    camera: CameraState,
    #[cfg(target_arch = "wasm32")]
    pub(crate) selected_device: Option<String>,
    /// True while we've hidden controls *and* entered browser fullscreen, so we
    /// can restore the controls when the browser leaves fullscreen (e.g. Esc).
    #[cfg(target_arch = "wasm32")]
    pub(crate) immersive: bool,

    #[cfg(not(target_arch = "wasm32"))]
    sim_time: f64,
}

impl PoiTrailsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();

        #[cfg(target_arch = "wasm32")]
        let camera = CameraState::new().expect("failed to bind camera DOM elements");

        let mut trails = TrailsProcessor::new(DEFAULT_WIDTH, DEFAULT_HEIGHT);
        trails.threshold = settings.threshold;
        trails.intensity_gain = settings.intensity_gain;
        trails.fade_seconds = settings.fade_seconds;
        trails.dim_factor = settings.dim_factor;
        trails.motion_gate = settings.motion_gate;
        trails.motion_sensitivity = settings.motion_sensitivity;
        trails.background_seconds = settings.background_seconds;

        let mode = if settings.trails_enabled {
            Mode::Trails
        } else {
            Mode::Live
        };

        #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))]
        let mut app = Self {
            mode,
            mirror_enabled: true,
            trails,
            delay_seconds: settings.delay_seconds,
            delay: DelayBuffer::new(),
            show_controls: true,
            texture: None,
            composite_buf: vec![0; DEFAULT_WIDTH * DEFAULT_HEIGHT * 4],
            #[cfg(target_arch = "wasm32")]
            camera,
            #[cfg(target_arch = "wasm32")]
            selected_device: None,
            #[cfg(target_arch = "wasm32")]
            immersive: false,
            #[cfg(not(target_arch = "wasm32"))]
            sim_time: 0.0,
        };

        #[cfg(target_arch = "wasm32")]
        if settings.camera_enabled {
            app.request_camera(None);
        }

        app
    }

    #[cfg(target_arch = "wasm32")]
    fn camera_enabled(&self) -> bool {
        !matches!(self.camera_status(), CameraStatus::NotStarted)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn camera_enabled(&self) -> bool {
        false
    }

    pub(crate) fn texture(&self) -> Option<&egui::TextureHandle> {
        self.texture.as_ref()
    }

    /// True when the controls are hidden *and* we're in real fullscreen — the
    /// view is then just the video, with no restore button (exit via Esc).
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn is_immersive(&self) -> bool {
        self.immersive
    }

    /// Native has no browser-fullscreen tracking; keep the restore button so
    /// the dev preview is never stuck.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn is_immersive(&self) -> bool {
        false
    }

    fn upload(&mut self, ctx: &egui::Context, width: usize, height: usize) {
        let color_image =
            egui::ColorImage::from_rgba_unmultiplied([width, height], &self.composite_buf);
        match &mut self.texture {
            Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("video", color_image, egui::TextureOptions::LINEAR));
            }
        }
    }

    /// The frame to display: delayed if the delay slider is up, otherwise the
    /// live frame (and the buffer is freed while delay is off).
    fn frame_to_show(&mut self, live: VideoFrame, dt: f32) -> VideoFrame {
        if self.delay_seconds > 0.05 {
            self.delay.tick(&live, dt, self.delay_seconds)
        } else {
            self.delay.clear();
            live
        }
    }

    fn process_and_upload(&mut self, ctx: &egui::Context, frame: &VideoFrame, fps: f32) {
        let (w, h) = (frame.width, frame.height);
        if self.composite_buf.len() != w * h * 4 {
            self.composite_buf = vec![0; w * h * 4];
        }

        match self.mode {
            Mode::Live => self.composite_buf.copy_from_slice(&frame.rgba),
            Mode::Trails => {
                self.trails.resize(w, h);
                self.trails
                    .process_frame(frame, fps, &mut self.composite_buf);
            }
        }

        self.upload(ctx, w, h);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn camera_status(&self) -> CameraStatus {
        self.camera.status()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn camera_devices(&self) -> Vec<CameraDevice> {
        self.camera.devices()
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn request_camera(&mut self, device_id: Option<String>) {
        self.selected_device = device_id.clone();
        self.camera.request_camera(device_id);
    }

    #[cfg(target_arch = "wasm32")]
    fn update_wasm(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // If the browser left fullscreen (e.g. the user pressed Esc) while we
        // were immersive, bring the controls back so they aren't stuck hidden.
        if self.immersive && !crate::fullscreen::is_active() {
            self.immersive = false;
            self.show_controls = true;
        }

        if matches!(self.camera_status(), CameraStatus::Ready) {
            let dt = ctx.input(|i| i.stable_dt).max(1.0 / 240.0);
            let fps = 1.0 / dt;
            if let Some(frame) = self.camera.poll_frame() {
                let frame = frame.clone();
                let shown = self.frame_to_show(frame, dt);
                self.process_and_upload(&ctx, &shown, fps);
            }
        }

        crate::ui::draw(self, ui);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn update_native(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let dt = ctx.input(|i| i.stable_dt).max(1.0 / 240.0);
        self.sim_time += dt as f64;
        let fps = 1.0 / dt;

        let frame = synthetic_frame(DEFAULT_WIDTH, DEFAULT_HEIGHT, self.sim_time);
        let shown = self.frame_to_show(frame, dt);
        self.process_and_upload(&ctx, &shown, fps);

        crate::ui::draw(self, ui);
    }
}

impl eframe::App for PoiTrailsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint();

        #[cfg(target_arch = "wasm32")]
        self.update_wasm(ui);

        #[cfg(not(target_arch = "wasm32"))]
        self.update_native(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let settings = Settings {
            camera_enabled: self.camera_enabled(),
            trails_enabled: self.mode == Mode::Trails,
            threshold: self.trails.threshold,
            intensity_gain: self.trails.intensity_gain,
            fade_seconds: self.trails.fade_seconds,
            dim_factor: self.trails.dim_factor,
            motion_gate: self.trails.motion_gate,
            motion_sensitivity: self.trails.motion_sensitivity,
            background_seconds: self.trails.background_seconds,
            delay_seconds: self.delay_seconds,
        };
        eframe::set_value(storage, eframe::APP_KEY, &settings);
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(2)
    }

    /// Pure black behind everything, so letterbox bars around the video are
    /// clean black rather than the default dark gray.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Native-only stand-in for a webcam frame: a bright dot orbiting on a dim
/// background, so trails/mirror behavior can be iterated on quickly with
/// `cargo run` without a browser or camera.
#[cfg(not(target_arch = "wasm32"))]
fn synthetic_frame(width: usize, height: usize, t: f64) -> VideoFrame {
    let mut frame = VideoFrame::new(width, height);
    let cx = width as f64 / 2.0 + t.cos() * width as f64 * 0.3;
    let cy = height as f64 / 2.0 + t.sin() * height as f64 * 0.3;

    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let bright = (1.0 - (dist / 20.0).clamp(0.0, 1.0)) as f32;
            let base_gray: u8 = 30;
            frame.rgba[i] = base_gray.saturating_add((bright * 225.0) as u8);
            frame.rgba[i + 1] = base_gray.saturating_add((bright * 200.0) as u8);
            frame.rgba[i + 2] = base_gray;
            frame.rgba[i + 3] = 255;
        }
    }
    frame
}

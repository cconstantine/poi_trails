use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::delay::DelayBuffer;
use crate::trails::{
    TrailsProcessor, DEFAULT_BACKGROUND_SECONDS, DEFAULT_DIM_FACTOR, DEFAULT_FADE_SECONDS,
    DEFAULT_INTENSITY_GAIN, DEFAULT_MOTION_GATE, DEFAULT_MOTION_SENSITIVITY, DEFAULT_THRESHOLD,
};
#[cfg(not(target_arch = "wasm32"))]
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

/// Standard capture resolutions offered when the camera can reach them. The
/// browser only exposes a supported width/height *range* (via
/// `getCapabilities`), not a discrete list, so we present these presets up to
/// the camera's reported maximum, plus the camera's native max itself.
#[cfg(target_arch = "wasm32")]
pub const RESOLUTION_PRESETS: [(u32, u32); 4] =
    [(640, 480), (1280, 720), (1920, 1080), (2560, 1440)];

/// Human label for a requested capture resolution (`None` = Auto).
#[cfg(target_arch = "wasm32")]
pub fn resolution_label(res: Option<(u32, u32)>) -> String {
    match res {
        None => "Auto (max)".to_string(),
        Some((w, h)) => match h {
            480 => "480p".to_string(),
            720 => "720p".to_string(),
            1080 => "1080p".to_string(),
            1440 => "1440p".to_string(),
            2160 => "4K".to_string(),
            _ => format!("{w}×{h}"),
        },
    }
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
    motion_sensitivity: f32,
    background_seconds: f32,
    delay_seconds: f32,
    /// Requested capture resolution (None = Auto / browser default).
    capture_resolution: Option<(u32, u32)>,
    /// Last selected camera device id, so multi-camera users keep their choice
    /// across visits (device ids are stable per-origin once permission is granted).
    selected_device: Option<String>,
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
            motion_sensitivity: DEFAULT_MOTION_SENSITIVITY,
            background_seconds: DEFAULT_BACKGROUND_SECONDS,
            delay_seconds: 0.0,
            capture_resolution: None,
            selected_device: None,
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
    /// Requested camera capture resolution (drives delay-buffer memory). None = Auto.
    capture_resolution: Option<(u32, u32)>,
    /// Actual resolution of the most recent frame, for display readouts.
    current_resolution: Option<(usize, usize)>,
    /// When false the side panel is hidden and only a small floating "show
    /// controls" button remains, for a clean full-window/fullscreen view.
    pub(crate) show_controls: bool,
    // Native-only CPU compositing target; web composites on the GPU.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    texture: Option<egui::TextureHandle>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
        // The static-background gate is always on (no longer user-toggleable).
        trails.motion_gate = DEFAULT_MOTION_GATE;
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
            capture_resolution: settings.capture_resolution,
            current_resolution: None,
            show_controls: true,
            texture: None,
            composite_buf: vec![0; DEFAULT_WIDTH * DEFAULT_HEIGHT * 4],
            #[cfg(target_arch = "wasm32")]
            camera,
            #[cfg(target_arch = "wasm32")]
            selected_device: settings.selected_device.clone(),
            #[cfg(target_arch = "wasm32")]
            immersive: false,
            #[cfg(not(target_arch = "wasm32"))]
            sim_time: 0.0,
        };

        #[cfg(target_arch = "wasm32")]
        {
            // Share eframe's WebGL2 context for GPU frame processing. WebGL2 is
            // guaranteed under the glow backend, so this should always succeed;
            // log and continue if not.
            if let Err(err) = crate::gpu::init() {
                log::error!("GPU pipeline init failed: {err}");
            }
            if settings.camera_enabled {
                app.request_camera(app.selected_device.clone());
            }
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

    #[cfg(target_arch = "wasm32")]
    fn saved_device(&self) -> Option<String> {
        self.selected_device.clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn saved_device(&self) -> Option<String> {
        None
    }

    // Native-only: web composites on the GPU and draws via a paint callback.
    #[cfg(not(target_arch = "wasm32"))]
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

    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    fn frame_to_show(&mut self, live: VideoFrame, dt: f32) -> VideoFrame {
        self.current_resolution = Some((live.width, live.height));
        if self.delay_seconds > 0.05 {
            self.delay.tick(&live, dt, self.delay_seconds)
        } else {
            self.delay.clear();
            live
        }
    }

    /// Estimated RAM the delay buffer needs at the current delay + resolution.
    /// This is the steady-state target (it doesn't ramp as the buffer fills),
    /// so the readout snaps to the size for the chosen delay. 0 when delay is off.
    pub(crate) fn projected_delay_bytes(&self) -> usize {
        if self.delay_seconds <= 0.05 {
            return 0;
        }
        let (w, h) = self
            .current_resolution
            .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));
        let frames = (self.delay_seconds as f64 * crate::delay::STORE_FPS).ceil() as usize;
        frames * w * h * 4
    }

    /// Actual resolution of the most recent frame, if any.
    pub(crate) fn current_resolution(&self) -> Option<(usize, usize)> {
        self.current_resolution
    }

    /// Restore all effect/view settings to their defaults — Mode, mirror flip,
    /// Delay, and every trails / background-suppression control — and clear the
    /// accumulated trail + re-learn the background for a clean slate.
    /// Intentionally leaves the camera and quality selection alone (resetting
    /// quality would restart the stream, and it's a hardware/performance choice).
    pub(crate) fn reset_to_defaults(&mut self) {
        self.mode = Mode::Trails;
        self.mirror_enabled = true;
        self.delay_seconds = 0.0;
        self.trails.threshold = DEFAULT_THRESHOLD;
        self.trails.intensity_gain = DEFAULT_INTENSITY_GAIN;
        self.trails.fade_seconds = DEFAULT_FADE_SECONDS;
        self.trails.dim_factor = DEFAULT_DIM_FACTOR;
        self.trails.motion_gate = DEFAULT_MOTION_GATE;
        self.trails.motion_sensitivity = DEFAULT_MOTION_SENSITIVITY;
        self.trails.background_seconds = DEFAULT_BACKGROUND_SECONDS;

        self.trails.clear();
        self.trails.reset_background();
        #[cfg(target_arch = "wasm32")]
        {
            crate::gpu::clear_trails();
            crate::gpu::reset_background();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
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
        self.camera
            .request_camera(device_id, self.capture_resolution);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn capture_resolution(&self) -> Option<(u32, u32)> {
        self.capture_resolution
    }

    /// The camera's maximum supported resolution, once discovered (via
    /// `getCapabilities`); `None` if unknown/unsupported.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn camera_max_resolution(&self) -> Option<(u32, u32)> {
        self.camera.max_resolution()
    }

    /// Change the requested capture resolution and restart the stream to apply it.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn set_capture_resolution(&mut self, resolution: Option<(u32, u32)>) {
        if self.capture_resolution != resolution {
            self.capture_resolution = resolution;
            // A resolution change makes the trail/delay buffers stale.
            self.delay.clear();
            self.request_camera(self.selected_device.clone());
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn update_wasm(&mut self, ui: &mut egui::Ui) {
        // Repaint continuously only while frames are flowing. Otherwise poll at
        // a low rate — enough to pick up async camera-status changes without
        // burning battery on an idle page.
        if matches!(self.camera_status(), CameraStatus::Ready) {
            ui.ctx().request_repaint();
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }

        // If the browser left fullscreen (e.g. the user pressed Esc) while we
        // were immersive, bring the controls back so they aren't stuck hidden.
        if self.immersive && !crate::fullscreen::is_active() {
            self.immersive = false;
            self.show_controls = true;
        }

        if matches!(self.camera_status(), CameraStatus::Ready) {
            // Upload the current frame straight to the GPU (no CPU readback),
            // then composite entirely on the GPU: delay ring -> trails -> display.
            if let Some((w, h)) = crate::gpu::upload_video(self.camera.video()) {
                self.current_resolution = Some((w as usize, h as usize));
                let dt = ui.ctx().input(|i| i.stable_dt).max(1.0 / 240.0);
                // Delay applies to both modes: pick the (possibly delayed) frame.
                crate::gpu::set_delay(self.delay_seconds, dt);
                match self.mode {
                    Mode::Trails => {
                        crate::gpu::process_trails(crate::gpu::TrailsParams {
                            threshold: self.trails.threshold,
                            intensity_gain: self.trails.intensity_gain,
                            dim_factor: self.trails.dim_factor,
                            fade_seconds: self.trails.fade_seconds,
                            motion_gate: self.trails.motion_gate,
                            motion_sensitivity: self.trails.motion_sensitivity,
                            background_seconds: self.trails.background_seconds,
                            dt,
                        });
                    }
                    Mode::Live => crate::gpu::use_mirror(),
                }
            }
        }

        crate::ui::draw(self, ui);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn update_native(&mut self, ui: &mut egui::Ui) {
        // The synthetic test pattern animates continuously.
        ui.ctx().request_repaint();
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
            motion_sensitivity: self.trails.motion_sensitivity,
            background_seconds: self.trails.background_seconds,
            delay_seconds: self.delay_seconds,
            capture_resolution: self.capture_resolution,
            selected_device: self.saved_device(),
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

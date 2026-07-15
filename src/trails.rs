use crate::video_frame::VideoFrame;

pub const DEFAULT_THRESHOLD: f32 = 0.6;
pub const DEFAULT_FADE_SECONDS: f32 = 1.0;
pub const DEFAULT_DIM_FACTOR: f32 = 0.35;
pub const DEFAULT_INTENSITY_GAIN: f32 = 1.0;
pub const DEFAULT_MOTION_GATE: bool = true;
pub const DEFAULT_MOTION_SENSITIVITY: f32 = 0.5;
pub const DEFAULT_BACKGROUND_SECONDS: f32 = 4.0;

/// Smooth Hermite interpolation: 0 below `edge0`, 1 above `edge1`, an
/// S-curve between. Requires `edge1 > edge0`.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Tracks a fading "displayed brightness" per pixel: a peak-hold-with-slow-release
/// filter, not a simple accumulator.
///
/// Each frame, every pixel gets a *target* brightness: the live, boosted
/// source color if it's above the brightness threshold, or the dimmed live
/// background otherwise. The displayed value then becomes
/// `max(target, previous_displayed * decay)` — it snaps instantly to the
/// target whenever the target is at least as bright as what's already
/// showing (so brightening is always immediate, never delayed or boosted
/// beyond what the camera actually captured), and only decays slowly toward
/// a dimmer target (that's the trail). Because there's a single piece of
/// state per pixel that *is* exactly what was last displayed, a trail can
/// never linger on top of unrelated content that later passes through the
/// same screen position at a brightness the trail itself never exceeded.
///
/// The buffer is `f32` rather than `u8` because repeated multiplicative
/// decay on an integer buffer truncates to zero within a handful of frames
/// at typical decay rates, killing the trail almost instantly.
pub struct TrailsProcessor {
    width: usize,
    height: usize,
    /// RGB only, row-major, length == width * height * 3, values in 0.0..=255.0.
    /// Always equal to what was written to `out` on the last `process_frame` call.
    displayed: Vec<f32>,
    /// Slowly-learned per-pixel background estimate, RGB, length == width * height * 3.
    /// Only maintained while `motion_gate` is on.
    background: Vec<f32>,
    /// False until `background` has been seeded from a frame; re-set to false by
    /// `resize`/`reset_background` and whenever the gate is off, so the model re-learns.
    background_ready: bool,
    /// Luminance (0.0..=1.0) above which a pixel is treated as a bright
    /// trail-worthy source rather than dimmed background.
    pub threshold: f32,
    /// How long a trail takes to fade to half intensity, in seconds.
    pub fade_seconds: f32,
    /// How much the live (non-trail) background is dimmed, 0.0 (black)..=1.0 (full brightness).
    pub dim_factor: f32,
    /// Multiplier applied to bright source pixels before they become the target brightness.
    pub intensity_gain: f32,
    /// When true, scale each pixel's trail eligibility by how much it differs
    /// from the learned background, so static bright clutter stops trailing.
    pub motion_gate: bool,
    /// 0..=1: higher means a smaller change from the background counts as motion.
    pub motion_sensitivity: f32,
    /// Time constant (seconds) for a newly-static object to be absorbed into the background.
    pub background_seconds: f32,
}

impl TrailsProcessor {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            displayed: vec![0.0; width * height * 3],
            background: vec![0.0; width * height * 3],
            background_ready: false,
            threshold: DEFAULT_THRESHOLD,
            fade_seconds: DEFAULT_FADE_SECONDS,
            dim_factor: DEFAULT_DIM_FACTOR,
            intensity_gain: DEFAULT_INTENSITY_GAIN,
            motion_gate: DEFAULT_MOTION_GATE,
            motion_sensitivity: DEFAULT_MOTION_SENSITIVITY,
            background_seconds: DEFAULT_BACKGROUND_SECONDS,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        if width != self.width || height != self.height {
            self.width = width;
            self.height = height;
            self.displayed = vec![0.0; width * height * 3];
            self.background = vec![0.0; width * height * 3];
            self.background_ready = false;
        }
    }

    pub fn clear(&mut self) {
        self.displayed.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Discard the learned background so the scene is re-learned from scratch.
    pub fn reset_background(&mut self) {
        self.background_ready = false;
    }

    fn decay_factor(&self, fps: f32) -> f32 {
        if self.fade_seconds <= 0.0 || fps <= 0.0 {
            return 0.0;
        }
        0.5_f32.powf(1.0 / (self.fade_seconds * fps))
    }

    /// Per-frame rate at which a fully-static pixel is blended into the
    /// background (before scaling by `1 - motion_factor`).
    fn background_alpha(&self, fps: f32) -> f32 {
        if self.background_seconds <= 0.0 || fps <= 0.0 {
            return 1.0;
        }
        1.0 - 0.5_f32.powf(1.0 / (self.background_seconds * fps))
    }

    /// Processes one incoming frame and writes the composited RGBA result into `out`.
    ///
    /// `frame` must match this processor's width/height, and `out` must be
    /// `width * height * 4` bytes long.
    pub fn process_frame(&mut self, frame: &VideoFrame, fps: f32, out: &mut [u8]) {
        assert_eq!(frame.width, self.width);
        assert_eq!(frame.height, self.height);
        assert_eq!(out.len(), self.width * self.height * 4);

        let decay = self.decay_factor(fps);
        let pixel_count = self.width * self.height;

        // Motion-gate locals, hoisted so the hot loop only touches the two
        // per-pixel buffers (disjoint field borrows) and plain Copy values.
        let motion_gate = self.motion_gate;
        let seed = !self.background_ready;
        let base_alpha = if motion_gate { self.background_alpha(fps) } else { 0.0 };
        // Change threshold for "motion": higher sensitivity lowers the bar.
        let edge0 = (1.0 - self.motion_sensitivity) * 0.5;
        let edge1 = edge0 + 0.1;
        let threshold = self.threshold;
        let dim_factor = self.dim_factor;
        let intensity_gain = self.intensity_gain;

        for i in 0..pixel_count {
            let src = i * 4;
            let dst = i * 3;

            let r = frame.rgba[src] as f32;
            let g = frame.rgba[src + 1] as f32;
            let b = frame.rgba[src + 2] as f32;

            // Rec.709 luma weights, normalized to 0.0..=1.0.
            let luminance = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;

            // How much this pixel differs from the learned static scene, in
            // 0..=1. With the gate off it's always 1 (everything is "motion"),
            // which reproduces the legacy boosted/dimmed behavior exactly.
            let motion_factor = if motion_gate {
                let bg = &mut self.background[dst..dst + 3];
                if seed {
                    bg[0] = r;
                    bg[1] = g;
                    bg[2] = b;
                }
                let diff = (r - bg[0]).abs().max((g - bg[1]).abs()).max((b - bg[2]).abs()) / 255.0;
                let mf = smoothstep(edge0, edge1, diff);
                // Blind running-average update: the background always drifts
                // toward the current frame at `base_alpha`. A briefly-passing
                // poi barely shifts a pixel's long-run average (it's the rare
                // value there), so it keeps trailing; a bright object that
                // stops moving is absorbed over ~`background_seconds` and fades.
                bg[0] += base_alpha * (r - bg[0]);
                bg[1] += base_alpha * (g - bg[1]);
                bg[2] += base_alpha * (b - bg[2]);
                mf
            } else {
                1.0
            };

            // Only bright pixels are source candidates, and then only to the
            // degree they are moving. `source_weight` blends the per-pixel
            // target between the dimmed background and the boosted source.
            let source_weight = if luminance > threshold { motion_factor } else { 0.0 };
            let gain = dim_factor + source_weight * (intensity_gain - dim_factor);
            let (tr, tg, tb) = (r * gain, g * gain, b * gain);

            let disp = &mut self.displayed[dst..dst + 3];
            // Peak-hold with slow release: snap up to the target instantly
            // whenever it's at least as bright as what's already shown, and
            // only decay slowly toward a *dimmer* target. This is what makes
            // fading poi trails linger without ever showing brighter than the
            // camera actually captured.
            disp[0] = tr.max(disp[0] * decay);
            disp[1] = tg.max(disp[1] * decay);
            disp[2] = tb.max(disp[2] * decay);

            let out_base = i * 4;
            out[out_base] = disp[0].clamp(0.0, 255.0) as u8;
            out[out_base + 1] = disp[1].clamp(0.0, 255.0) as u8;
            out[out_base + 2] = disp[2].clamp(0.0, 255.0) as u8;
            out[out_base + 3] = 255;
        }

        // The background is only valid while the gate runs; drop readiness when
        // it's off so re-enabling re-seeds from the current scene.
        self.background_ready = motion_gate;
    }

    #[cfg(test)]
    fn displayed_pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * 3;
        [
            self.displayed[i],
            self.displayed[i + 1],
            self.displayed[i + 2],
        ]
    }
}

#[cfg(test)]
fn max_channel(px: [u8; 4]) -> u8 {
    px[0].max(px[1]).max(px[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(w: usize, h: usize, rgb: [u8; 3]) -> VideoFrame {
        let mut frame = VideoFrame::new(w, h);
        for px in frame.rgba.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
        frame
    }

    #[test]
    fn bright_pixel_decays_geometrically() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.motion_gate = false;
        proc.threshold = 0.5;
        proc.fade_seconds = 1.0;
        proc.dim_factor = 0.0;
        let fps = 30.0;

        let bright = solid_frame(1, 1, [255, 255, 255]);
        let dark = solid_frame(1, 1, [0, 0, 0]);
        let mut out = [0u8; 4];

        proc.process_frame(&bright, fps, &mut out);
        let after_hit = proc.displayed_pixel(0, 0);
        assert!((after_hit[0] - 255.0).abs() < 1e-3);

        let decay = proc.decay_factor(fps);
        let mut expected = after_hit[0];
        for _ in 0..10 {
            proc.process_frame(&dark, fps, &mut out);
            expected *= decay;
            let acc = proc.displayed_pixel(0, 0);
            assert!(
                (acc[0] - expected).abs() < 1e-2,
                "acc={} expected={}",
                acc[0],
                expected
            );
        }
    }

    #[test]
    fn sub_threshold_pixels_stay_at_dimmed_background() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.motion_gate = false;
        proc.threshold = 0.9;
        proc.dim_factor = 0.0;
        let dim = solid_frame(1, 1, [100, 100, 100]); // luminance ~0.39, below threshold
        let mut out = [0u8; 4];

        for _ in 0..5 {
            proc.process_frame(&dim, 30.0, &mut out);
        }

        assert_eq!(proc.displayed_pixel(0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(out, [0, 0, 0, 255]);
    }

    #[test]
    fn clear_zeroes_displayed() {
        let mut proc = TrailsProcessor::new(2, 2);
        proc.motion_gate = false;
        proc.threshold = 0.1;
        let bright = solid_frame(2, 2, [255, 255, 255]);
        let mut out = [0u8; 16];

        proc.process_frame(&bright, 30.0, &mut out);
        assert_ne!(proc.displayed_pixel(0, 0), [0.0, 0.0, 0.0]);

        proc.clear();
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(proc.displayed_pixel(x, y), [0.0, 0.0, 0.0]);
            }
        }
    }

    #[test]
    fn sustained_bright_pixel_does_not_blow_out() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.motion_gate = false;
        proc.threshold = 0.1;
        proc.dim_factor = 0.0;
        proc.intensity_gain = 1.0;
        let bright = solid_frame(1, 1, [200, 200, 200]);
        let mut out = [0u8; 4];

        for _ in 0..20 {
            proc.process_frame(&bright, 30.0, &mut out);
        }

        // Should clamp to the source brightness, never exceed it via accumulation.
        assert_eq!(out, [200, 200, 200, 255]);
    }

    #[test]
    fn brightening_is_instant_dimming_is_gradual() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.motion_gate = false;
        proc.threshold = 0.5;
        proc.dim_factor = 0.0;
        proc.fade_seconds = 1.0;
        let fps = 30.0;

        let bright = solid_frame(1, 1, [255, 255, 255]);
        let dark = solid_frame(1, 1, [0, 0, 0]);
        let mut out = [0u8; 4];

        proc.process_frame(&bright, fps, &mut out);
        assert_eq!(out, [255, 255, 255, 255], "brightening should be immediate");

        // Let the trail decay partway — should still be lit, but dimmer.
        for _ in 0..5 {
            proc.process_frame(&dark, fps, &mut out);
        }
        assert!(
            out[0] > 0 && out[0] < 255,
            "expected a partially-decayed trail, got {out:?}"
        );

        // A fresh bright hit must snap straight back to full brightness in a
        // single frame, not ramp up gradually from the decayed value, and
        // must never exceed what the live camera actually captured.
        proc.process_frame(&bright, fps, &mut out);
        assert_eq!(
            out,
            [255, 255, 255, 255],
            "a pixel getting brighter than before should track instantly"
        );
    }

    /// With the gate off, output must match an independent reference
    /// implementation of the pre-gate formula, byte for byte, across a mixed
    /// sequence — the motion path must be a pure superset that changes nothing
    /// when disabled.
    #[test]
    fn gate_disabled_matches_legacy() {
        let (threshold, dim, gain, fade, fps) = (0.5_f32, 0.3_f32, 1.2_f32, 0.8_f32, 30.0_f32);

        let mut proc = TrailsProcessor::new(1, 1);
        proc.threshold = threshold;
        proc.dim_factor = dim;
        proc.intensity_gain = gain;
        proc.fade_seconds = fade;
        proc.motion_gate = false;

        let decay = 0.5_f32.powf(1.0 / (fade * fps));
        let mut ref_disp = [0.0_f32; 3];

        let frames = [
            [255u8, 255, 255],
            [10, 10, 10],
            [200, 50, 50],
            [0, 0, 0],
            [255, 255, 255],
        ];
        let mut out = [0u8; 4];
        for rgb in &frames {
            proc.process_frame(&solid_frame(1, 1, *rgb), fps, &mut out);

            let c = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
            let lum = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]) / 255.0;
            let g = if lum > threshold { gain } else { dim };
            let mut expected = [0u8; 4];
            expected[3] = 255;
            for ch in 0..3 {
                ref_disp[ch] = (c[ch] * g).max(ref_disp[ch] * decay);
                expected[ch] = ref_disp[ch].clamp(0.0, 255.0) as u8;
            }
            assert_eq!(out, expected, "gate-off output diverged from legacy formula");
        }
    }

    #[test]
    fn static_bright_pixel_stops_trailing() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.threshold = 0.5;
        proc.dim_factor = 0.2;
        proc.intensity_gain = 1.0;
        proc.fade_seconds = 0.5;
        proc.motion_gate = true;
        proc.motion_sensitivity = 0.5;
        proc.background_seconds = 0.5;
        let fps = 30.0;

        let bright = solid_frame(1, 1, [255, 255, 255]);
        let mut out = [0u8; 4];

        // Hold a static bright pixel long enough for the background to converge.
        for _ in 0..120 {
            proc.process_frame(&bright, fps, &mut out);
        }

        // It should have collapsed toward the dimmed background (~0.2*255≈51),
        // nowhere near the boosted 255 it would trail at without the gate.
        assert!(
            max_channel(out) < 100,
            "static bright pixel should stop trailing, got {out:?}"
        );
    }

    #[test]
    fn moving_bright_pixel_still_trails() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.threshold = 0.5;
        proc.dim_factor = 0.2;
        proc.intensity_gain = 1.0;
        proc.fade_seconds = 0.5;
        proc.motion_gate = true;
        proc.motion_sensitivity = 0.5;
        proc.background_seconds = 0.5;
        let fps = 30.0;

        let bright = solid_frame(1, 1, [255, 255, 255]);
        let dark = solid_frame(1, 1, [0, 0, 0]);
        let mut out = [0u8; 4];

        // Alternate bright/dark so the pixel keeps differing from any learned
        // background: it must still be treated as a full-strength source.
        for _ in 0..120 {
            proc.process_frame(&bright, fps, &mut out);
            proc.process_frame(&dark, fps, &mut out);
        }
        proc.process_frame(&bright, fps, &mut out);

        assert!(
            max_channel(out) > 230,
            "a moving bright pixel should still trail at full strength, got {out:?}"
        );
    }

    #[test]
    fn no_startup_flash() {
        let mut proc = TrailsProcessor::new(1, 1);
        proc.threshold = 0.5;
        proc.dim_factor = 0.2;
        proc.intensity_gain = 1.0;
        proc.motion_gate = true;
        proc.motion_sensitivity = 0.5;

        // First frame of a uniformly bright static scene: the background seeds
        // to this frame, so diff≈0, motion≈0, and it renders dimmed — no flash.
        let bright = solid_frame(1, 1, [255, 255, 255]);
        let mut out = [0u8; 4];
        proc.process_frame(&bright, 30.0, &mut out);

        assert!(
            max_channel(out) < 100,
            "first frame should not flash a bright trail, got {out:?}"
        );
    }
}

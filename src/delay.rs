//! A time-delay ring buffer for the "delay mirror" feature: show what the
//! camera saw up to a few seconds ago, so you can perform a move and then look
//! up to review it.
//!
//! Frames are stored at full resolution (no downscaling), throttled to
//! [`STORE_FPS`] so that a display repainting faster than the camera delivers
//! doesn't store duplicate frames. Only frames within the last
//! [`MAX_DELAY_SECONDS`] are kept, and evicted frame buffers are recycled to
//! avoid per-frame allocation churn.

use std::collections::VecDeque;

use crate::video_frame::VideoFrame;

/// Longest delay the user can dial in.
pub const MAX_DELAY_SECONDS: f32 = 5.0;
/// Cap on how often frames are stored. Bounds the buffer to
/// `MAX_DELAY_SECONDS * STORE_FPS` frames regardless of the display refresh rate.
pub const STORE_FPS: f64 = 30.0;

struct Stamped {
    /// Seconds since this buffer started, monotonically increasing.
    t: f64,
    frame: VideoFrame,
}

pub struct DelayBuffer {
    now: f64,
    last_store: f64,
    /// Oldest at the front, newest at the back.
    frames: VecDeque<Stamped>,
    /// Recycled frame buffers, reused when storing to avoid reallocating.
    pool: Vec<VideoFrame>,
}

impl Default for DelayBuffer {
    fn default() -> Self {
        Self {
            now: 0.0,
            last_store: f64::NEG_INFINITY,
            frames: VecDeque::new(),
            pool: Vec::new(),
        }
    }
}

impl DelayBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all buffered frames and free their memory. Called when the delay is
    /// turned off so a stale buffer doesn't sit in memory.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.pool.clear();
        self.pool.shrink_to_fit();
        self.last_store = f64::NEG_INFINITY;
    }

    /// Advance time by `dt` seconds, store `live` (throttled to [`STORE_FPS`]),
    /// and return the frame that should be shown for `delay_seconds` of delay.
    ///
    /// Before enough history exists, this "ramps in": it returns the oldest
    /// frame available, so the delay grows smoothly from 0 to the target
    /// instead of showing a blank screen.
    pub fn tick(&mut self, live: &VideoFrame, dt: f32, delay_seconds: f32) -> VideoFrame {
        self.now += dt.max(0.0) as f64;

        if self.frames.is_empty() || (self.now - self.last_store) >= 1.0 / STORE_FPS {
            self.store(live);
            self.last_store = self.now;
        }

        // Evict frames older than the max window (keep at least one).
        while self.frames.len() > 1
            && (self.now - self.frames.front().unwrap().t) > MAX_DELAY_SECONDS as f64
        {
            let old = self.frames.pop_front().unwrap();
            self.pool.push(old.frame);
        }

        let target = self.now - delay_seconds.max(0.0) as f64;
        // Newest frame at or before the target time; else the oldest we have
        // (the ramp-in case, when the requested delay exceeds our history).
        let chosen = self
            .frames
            .iter()
            .rev()
            .find(|s| s.t <= target)
            .unwrap_or_else(|| self.frames.front().unwrap());
        chosen.frame.clone()
    }

    fn store(&mut self, live: &VideoFrame) {
        let mut frame = self.pool.pop().unwrap_or_else(|| VideoFrame::new(0, 0));
        // Reuse the recycled buffer, resizing only if the camera resolution changed.
        frame.width = live.width;
        frame.height = live.height;
        frame.rgba.clear();
        frame.rgba.extend_from_slice(&live.rgba);
        self.frames.push_back(Stamped { t: self.now, frame });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 frame whose single pixel encodes `tag`, so we can identify which
    /// stored frame comes back out.
    fn tagged(tag: u8) -> VideoFrame {
        let mut f = VideoFrame::new(1, 1);
        f.rgba = vec![tag, tag, tag, 255];
        f
    }

    #[test]
    fn returns_frame_from_the_past() {
        let mut buf = DelayBuffer::new();
        let dt = 1.0 / STORE_FPS as f32; // one stored frame per tick

        // Feed 60 frames tagged 0..=59 (2 seconds at 30 fps), no delay yet.
        for tag in 0..60u8 {
            buf.tick(&tagged(tag), dt, 0.0);
        }
        // Store tag 60 and ask for a 1s delay. Frame `i` is stored at
        // t = (i+1)/30; now = 61/30, so target = now - 1s = 31/30, whose
        // newest frame at-or-before is tag 30.
        let shown = buf.tick(&tagged(60), dt, 1.0);
        assert_eq!(shown.rgba[0], 30, "expected a ~1s-old frame");
    }

    #[test]
    fn ramps_in_returning_oldest_before_history_exists() {
        let mut buf = DelayBuffer::new();
        let dt = 1.0 / STORE_FPS as f32;

        // Only 3 frames stored, but ask for a 5s delay.
        buf.tick(&tagged(10), dt, 5.0);
        buf.tick(&tagged(11), dt, 5.0);
        let shown = buf.tick(&tagged(12), dt, 5.0);
        // Not enough history -> oldest available frame (tag 10).
        assert_eq!(shown.rgba[0], 10);
    }

    #[test]
    fn delay_zero_shows_the_live_frame() {
        let mut buf = DelayBuffer::new();
        let dt = 1.0 / STORE_FPS as f32;
        buf.tick(&tagged(1), dt, 0.0);
        buf.tick(&tagged(2), dt, 0.0);
        let shown = buf.tick(&tagged(3), dt, 0.0);
        assert_eq!(shown.rgba[0], 3, "zero delay should show the current frame");
    }

    #[test]
    fn buffer_is_bounded_to_the_max_window() {
        let mut buf = DelayBuffer::new();
        let dt = 1.0 / STORE_FPS as f32;
        // Feed 20 seconds worth; only ~5s (150 frames) should be retained.
        for tag in 0..600u32 {
            buf.tick(&tagged((tag % 256) as u8), dt, 0.0);
        }
        let max_frames = (MAX_DELAY_SECONDS as f64 * STORE_FPS).ceil() as usize + 2;
        assert!(
            buf.frames.len() <= max_frames,
            "buffer grew to {} frames, expected <= {}",
            buf.frames.len(),
            max_frames
        );
    }

    #[test]
    fn does_not_store_duplicates_when_ticked_faster_than_store_fps() {
        let mut buf = DelayBuffer::new();
        // 120 fps display (dt smaller than the store interval).
        let dt = 1.0 / 120.0;
        for _ in 0..120 {
            buf.tick(&tagged(7), dt, 0.0);
        }
        // ~1 second elapsed -> ~30 stored frames, not 120.
        assert!(
            buf.frames.len() <= 32,
            "stored {} frames; throttle should cap near 30",
            buf.frames.len()
        );
    }

    #[test]
    fn clear_frees_everything() {
        let mut buf = DelayBuffer::new();
        let dt = 1.0 / STORE_FPS as f32;
        for tag in 0..10u8 {
            buf.tick(&tagged(tag), dt, 0.0);
        }
        buf.clear();
        assert!(buf.frames.is_empty());
        assert!(buf.pool.is_empty());
    }
}

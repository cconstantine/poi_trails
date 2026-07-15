//! Webcam capture via `getUserMedia`, wasm/browser only.
//!
//! Frames are pulled synchronously each `eframe::App::update()` tick by
//! drawing the live `<video>` element into a hidden scratch `<canvas>` and
//! reading its pixels back with `getImageData`. The browser is single
//! threaded in wasm, so the async permission flow shares state into the
//! synchronous update loop via `Rc<RefCell<_>>` rather than channels.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, HtmlVideoElement, MediaDeviceInfo,
    MediaDeviceKind, MediaStream, MediaStreamConstraints, MediaStreamTrack, MediaTrackConstraints,
};

use crate::video_frame::{CameraDevice, VideoFrame};

#[derive(Clone)]
pub enum CameraStatus {
    NotStarted,
    Requesting,
    Ready,
    Error(String),
}

pub struct CameraState {
    status: Rc<RefCell<CameraStatus>>,
    video: HtmlVideoElement,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    stream: Rc<RefCell<Option<MediaStream>>>,
    devices: Rc<RefCell<Vec<CameraDevice>>>,
    frame: VideoFrame,
}

impl CameraState {
    pub fn new() -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no global window"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("no document"))?;

        let video = document
            .get_element_by_id("webcam_video")
            .ok_or_else(|| JsValue::from_str("missing #webcam_video element"))?
            .dyn_into::<HtmlVideoElement>()?;

        let canvas = document
            .get_element_by_id("scratch_canvas")
            .ok_or_else(|| JsValue::from_str("missing #scratch_canvas element"))?
            .dyn_into::<HtmlCanvasElement>()?;

        let ctx = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("no 2d context"))?
            .dyn_into::<CanvasRenderingContext2d>()?;

        Ok(Self {
            status: Rc::new(RefCell::new(CameraStatus::NotStarted)),
            video,
            canvas,
            ctx,
            stream: Rc::new(RefCell::new(None)),
            devices: Rc::new(RefCell::new(Vec::new())),
            frame: VideoFrame::new(640, 480),
        })
    }

    pub fn status(&self) -> CameraStatus {
        self.status.borrow().clone()
    }

    pub fn devices(&self) -> Vec<CameraDevice> {
        self.devices.borrow().clone()
    }

    /// Kicks off (or restarts, e.g. to switch device) the async permission +
    /// stream-acquisition flow. Safe to call again while already `Ready` to
    /// switch cameras; the previous stream's tracks are stopped once the new
    /// one is confirmed live.
    pub fn request_camera(&self, device_id: Option<String>) {
        *self.status.borrow_mut() = CameraStatus::Requesting;

        let status = Rc::clone(&self.status);
        let stream_slot = Rc::clone(&self.stream);
        let devices_slot = Rc::clone(&self.devices);
        let video = self.video.clone();

        wasm_bindgen_futures::spawn_local(async move {
            match start_stream(&video, device_id).await {
                Ok(stream) => {
                    if let Some(old) = stream_slot.borrow_mut().take() {
                        stop_all_tracks(&old);
                    }
                    *stream_slot.borrow_mut() = Some(stream);
                    *status.borrow_mut() = CameraStatus::Ready;

                    if let Ok(list) = enumerate_video_devices().await {
                        *devices_slot.borrow_mut() = list;
                    }
                }
                Err(err) => {
                    *status.borrow_mut() = CameraStatus::Error(js_error_to_string(&err));
                }
            }
        });
    }

    /// Pulls the current video frame into an owned RGBA buffer, resizing the
    /// scratch canvas/internal buffer to match the camera's negotiated
    /// resolution the first time it becomes known. Returns `None` until a
    /// stream is ready and has produced at least one decoded frame.
    pub fn poll_frame(&mut self) -> Option<&VideoFrame> {
        if !matches!(*self.status.borrow(), CameraStatus::Ready) {
            return None;
        }

        let vw = self.video.video_width();
        let vh = self.video.video_height();
        if vw == 0 || vh == 0 {
            return None;
        }
        let (vw, vh) = (vw as usize, vh as usize);

        if self.canvas.width() as usize != vw || self.canvas.height() as usize != vh {
            self.canvas.set_width(vw as u32);
            self.canvas.set_height(vh as u32);
        }
        if self.frame.width != vw || self.frame.height != vh {
            self.frame = VideoFrame::new(vw, vh);
        }

        self.ctx
            .draw_image_with_html_video_element(&self.video, 0.0, 0.0)
            .ok()?;
        let image_data = self
            .ctx
            .get_image_data(0.0, 0.0, vw as f64, vh as f64)
            .ok()?;
        let clamped = image_data.data();
        let bytes: &[u8] = &clamped;
        if bytes.len() != self.frame.rgba.len() {
            return None;
        }
        self.frame.rgba.copy_from_slice(bytes);

        Some(&self.frame)
    }
}

impl Drop for CameraState {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.borrow_mut().take() {
            stop_all_tracks(&stream);
        }
    }
}

async fn start_stream(
    video: &HtmlVideoElement,
    device_id: Option<String>,
) -> Result<MediaStream, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no global window"))?;
    let media_devices = window.navigator().media_devices()?;

    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&JsValue::FALSE);
    match device_id {
        Some(id) => {
            let track_constraints = MediaTrackConstraints::new();
            track_constraints.set_device_id(&JsValue::from_str(&id));
            constraints.set_video(&track_constraints);
        }
        None => constraints.set_video(&JsValue::TRUE),
    }

    let promise = media_devices.get_user_media_with_constraints(&constraints)?;
    let stream: MediaStream = JsFuture::from(promise).await?.dyn_into()?;

    video.set_src_object(Some(&stream));
    JsFuture::from(video.play()?).await?;

    Ok(stream)
}

async fn enumerate_video_devices() -> Result<Vec<CameraDevice>, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no global window"))?;
    let media_devices = window.navigator().media_devices()?;
    let promise = media_devices.enumerate_devices()?;
    let array: js_sys::Array = JsFuture::from(promise).await?.dyn_into()?;

    let mut out = Vec::new();
    for item in array.iter() {
        if let Ok(info) = item.dyn_into::<MediaDeviceInfo>() {
            if info.kind() == MediaDeviceKind::Videoinput {
                out.push(CameraDevice {
                    device_id: info.device_id(),
                    label: info.label(),
                });
            }
        }
    }
    Ok(out)
}

fn stop_all_tracks(stream: &MediaStream) {
    let tracks = stream.get_tracks();
    for i in 0..tracks.length() {
        if let Ok(track) = tracks.get(i).dyn_into::<MediaStreamTrack>() {
            track.stop();
        }
    }
}

fn js_error_to_string(err: &JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    if let Some(dom_ex) = err.dyn_ref::<web_sys::DomException>() {
        return format!("{}: {}", dom_ex.name(), dom_ex.message());
    }
    format!("{err:?}")
}

//! Webcam capture via `getUserMedia`, wasm/browser only.
//!
//! Manages the permission flow and keeps the `MediaStream` attached to a hidden
//! `<video>` element. The GPU pipeline (`gpu.rs`) uploads that video element's
//! frames straight to a texture, so no pixel readback happens here. The browser
//! is single threaded in wasm, so async state is shared into the synchronous
//! update loop via `Rc<RefCell<_>>` rather than channels.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    HtmlVideoElement, MediaDeviceInfo, MediaDeviceKind, MediaStream, MediaStreamConstraints,
    MediaStreamTrack, MediaTrackConstraints,
};

use crate::video_frame::CameraDevice;

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
    stream: Rc<RefCell<Option<MediaStream>>>,
    devices: Rc<RefCell<Vec<CameraDevice>>>,
    /// Camera's max supported (width, height) from `getCapabilities`, once known.
    max_resolution: Rc<RefCell<Option<(u32, u32)>>>,
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

        Ok(Self {
            status: Rc::new(RefCell::new(CameraStatus::NotStarted)),
            video,
            stream: Rc::new(RefCell::new(None)),
            devices: Rc::new(RefCell::new(Vec::new())),
            max_resolution: Rc::new(RefCell::new(None)),
        })
    }

    pub fn status(&self) -> CameraStatus {
        self.status.borrow().clone()
    }

    pub fn devices(&self) -> Vec<CameraDevice> {
        self.devices.borrow().clone()
    }

    pub fn max_resolution(&self) -> Option<(u32, u32)> {
        *self.max_resolution.borrow()
    }

    /// The hidden `<video>` element the stream renders into — used by the GPU
    /// pipeline to upload frames directly (no `getImageData` readback).
    pub fn video(&self) -> &HtmlVideoElement {
        &self.video
    }

    /// Kicks off (or restarts, e.g. to switch device) the async permission +
    /// stream-acquisition flow. Safe to call again while already `Ready` to
    /// switch cameras; the previous stream's tracks are stopped once the new
    /// one is confirmed live.
    pub fn request_camera(&self, device_id: Option<String>, resolution: Option<(u32, u32)>) {
        *self.status.borrow_mut() = CameraStatus::Requesting;

        let status = Rc::clone(&self.status);
        let stream_slot = Rc::clone(&self.stream);
        let devices_slot = Rc::clone(&self.devices);
        let max_res_slot = Rc::clone(&self.max_resolution);
        let video = self.video.clone();

        wasm_bindgen_futures::spawn_local(async move {
            // Release any existing stream *before* requesting the new one. A
            // camera can only run one configuration at a time, so requesting a
            // new resolution while the old track is still live gets clamped to
            // the resolution already open — the camera must be free to reopen.
            if let Some(old) = stream_slot.borrow_mut().take() {
                stop_all_tracks(&old);
            }
            match start_stream(&video, device_id, resolution).await {
                Ok(stream) => {
                    *max_res_slot.borrow_mut() = read_max_resolution(&stream);
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
    resolution: Option<(u32, u32)>,
) -> Result<MediaStream, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no global window"))?;
    let media_devices = window.navigator().media_devices()?;

    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&JsValue::FALSE);

    let track_constraints = MediaTrackConstraints::new();
    if let Some(id) = device_id {
        track_constraints.set_device_id(&JsValue::from_str(&id));
    }
    // A plain integer width/height is treated as `ideal` by the constraints
    // spec. For a specific quality we request it directly; for Auto we request
    // an ideal larger than any camera so the browser negotiates down to the
    // camera's native maximum (rather than the ~640x480 getUserMedia default).
    let (w, h) = resolution.unwrap_or((7680, 4320));
    track_constraints.set_width_i32(w as i32);
    track_constraints.set_height_i32(h as i32);
    constraints.set_video(&track_constraints);

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

/// Reads the camera's maximum supported (width, height) from the video track's
/// `getCapabilities()`. Called via `js_sys::Reflect` rather than the typed
/// web-sys binding, which is gated behind the unstable-apis build flag. Returns
/// `None` if the browser doesn't support `getCapabilities` (e.g. older Safari)
/// or doesn't report a width/height range — callers fall back to plain presets.
fn read_max_resolution(stream: &MediaStream) -> Option<(u32, u32)> {
    let track = stream.get_video_tracks().get(0);
    if track.is_undefined() || track.is_null() {
        return None;
    }
    let get_caps = js_sys::Reflect::get(&track, &JsValue::from_str("getCapabilities")).ok()?;
    let get_caps = get_caps.dyn_ref::<js_sys::Function>()?;
    let caps = get_caps.call0(&track).ok()?;

    let max_of = |key: &str| -> Option<u32> {
        let range = js_sys::Reflect::get(&caps, &JsValue::from_str(key)).ok()?;
        let max = js_sys::Reflect::get(&range, &JsValue::from_str("max")).ok()?;
        max.as_f64().map(|v| v as u32)
    };
    Some((max_of("width")?, max_of("height")?))
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

//! Clip recording via `canvas.captureStream()` + `MediaRecorder`, wasm only.
//!
//! Records the visible render canvas, so a clip is exactly what the user sees
//! (mirror flip, delay, and letterboxing included). The app hides the egui
//! controls while recording — egui draws onto the same canvas, so any widget
//! shown would end up in the clip. The Stop control is therefore a DOM overlay
//! button (`#stop_recording` in index.html), which is never part of the canvas
//! pixels. The browser does the encoding; on stop the chunks are assembled
//! into a Blob and downloaded through a temporary object URL.
//!
//! The `MediaRecorder` event closures only write into shared plain-data cells
//! (never into the struct that owns them): a closure that dropped itself
//! mid-invocation would abort in wasm-bindgen. The app polls
//! [`Recorder::poll_finished`] and drops the closures from the update loop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    Blob, BlobEvent, BlobPropertyBag, HtmlCanvasElement, HtmlElement, MediaRecorder,
    MediaRecorderOptions,
};

/// Longest allowed clip; recording auto-stops here. Keeps "short clips" honest
/// and bounds the encoded chunks held in memory.
pub const MAX_CLIP_SECONDS: f64 = 60.0;

const CANVAS_ID: &str = "the_canvas_id";
const STOP_BUTTON_ID: &str = "stop_recording";

/// Preference order: mp4 is the most shareable where supported (Safari, newer
/// Chrome); VP9 beats VP8 at these bitrates; bare webm as a last resort.
const MIME_PREFERENCES: &[(&str, &str)] = &[
    ("video/mp4", "mp4"),
    ("video/webm;codecs=vp9", "webm"),
    ("video/webm;codecs=vp8", "webm"),
    ("video/webm", "webm"),
];

/// Bright trails on mostly-dark frames compress well; this keeps 1080p crisp.
const VIDEO_BITS_PER_SECOND: u32 = 8_000_000;

thread_local! {
    /// Set by the stop button's click listener; polled + cleared by the app.
    static STOP_REQUESTED: Cell<bool> = const { Cell::new(false) };
    /// The click listener is installed once and leaked (page-lifetime).
    static STOP_LISTENER_INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Outcome of the most recently finished recording, for the control panel.
#[derive(Clone)]
pub enum LastClip {
    Saved {
        filename: String,
        seconds: f64,
        bytes: f64,
    },
    Error(String),
}

/// Live recording state: the MediaRecorder plus the event closures kept alive
/// for its duration. Dropped (safely, from the update loop) once `done` fills.
struct Active {
    recorder: MediaRecorder,
    started_ms: f64,
    /// True once stop() was issued, so repeated stop conditions don't re-fire.
    stopping: bool,
    /// Filled by the onstop closure with the final outcome.
    done: Rc<RefCell<Option<LastClip>>>,
    _ondata: Closure<dyn FnMut(BlobEvent)>,
    _onstop: Closure<dyn FnMut(web_sys::Event)>,
    _onerror: Closure<dyn FnMut(web_sys::Event)>,
}

pub struct Recorder {
    active: Option<Active>,
    last: Option<LastClip>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            active: None,
            last: None,
        }
    }

    /// Begin recording the render canvas. On failure the error lands in
    /// [`Self::last_clip`] and `is_recording()` stays false.
    pub fn start(&mut self) {
        if self.active.is_some() {
            return;
        }
        STOP_REQUESTED.with(|c| c.set(false));
        match start_recording() {
            Ok(active) => {
                set_stop_button_hidden(false);
                update_stop_label(0.0);
                self.active = Some(active);
            }
            Err(err) => self.last = Some(LastClip::Error(err)),
        }
    }

    /// Ask the recorder to finish; the outcome arrives via [`Self::poll_finished`].
    pub fn stop(&mut self) {
        if let Some(active) = &mut self.active {
            if !active.stopping {
                active.stopping = true;
                if let Err(err) = active.recorder.stop() {
                    // stop() itself failed — force completion so the app
                    // isn't stuck in the recording state.
                    *active.done.borrow_mut() = Some(LastClip::Error(js_err(&err)));
                }
            }
        }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    /// Seconds since recording started, while recording.
    pub fn elapsed_seconds(&self) -> Option<f64> {
        self.active
            .as_ref()
            .map(|a| (js_sys::Date::now() - a.started_ms) / 1000.0)
    }

    /// True exactly once, the frame a recording finished (its outcome is then
    /// in [`Self::last_clip`]). Also releases the recorder and its closures —
    /// safe here because no MediaRecorder event is executing during the app's
    /// update loop.
    pub fn poll_finished(&mut self) -> bool {
        let done = self
            .active
            .as_ref()
            .and_then(|a| a.done.borrow_mut().take());
        match done {
            Some(clip) => {
                self.last = Some(clip);
                self.active = None;
                set_stop_button_hidden(true);
                true
            }
            None => false,
        }
    }

    pub fn last_clip(&self) -> Option<&LastClip> {
        self.last.as_ref()
    }
}

/// True once (per press) after the DOM stop button was clicked.
pub fn take_stop_request() -> bool {
    STOP_REQUESTED.with(|c| c.replace(false))
}

/// Update the stop button's elapsed-time readout.
pub fn update_stop_label(elapsed: f64) {
    if let Some(button) = stop_button() {
        let m = (elapsed / 60.0) as u32;
        let s = (elapsed % 60.0) as u32;
        button.set_text_content(Some(&format!("● {m}:{s:02} — Stop (Esc)")));
    }
}

fn start_recording() -> Result<Active, String> {
    install_stop_listener()?;

    let canvas = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?
        .get_element_by_id(CANVAS_ID)
        .ok_or("canvas element not found")?
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| "element is not a canvas".to_string())?;

    let stream = canvas.capture_stream().map_err(|e| js_err(&e))?;

    let (mime, ext) = MIME_PREFERENCES
        .iter()
        .copied()
        .find(|(mime, _)| MediaRecorder::is_type_supported(mime))
        .ok_or("this browser supports no video recording format")?;

    let options = MediaRecorderOptions::new();
    options.set_mime_type(mime);
    options.set_video_bits_per_second(VIDEO_BITS_PER_SECOND);
    let recorder = MediaRecorder::new_with_media_stream_and_media_recorder_options(
        &stream, &options,
    )
    .map_err(|e| js_err(&e))?;

    let chunks: Rc<RefCell<Vec<Blob>>> = Rc::new(RefCell::new(Vec::new()));
    let done: Rc<RefCell<Option<LastClip>>> = Rc::new(RefCell::new(None));
    let started_ms = js_sys::Date::now();

    let ondata = {
        let chunks = Rc::clone(&chunks);
        Closure::<dyn FnMut(BlobEvent)>::new(move |event: BlobEvent| {
            if let Some(blob) = event.data() {
                if blob.size() > 0.0 {
                    chunks.borrow_mut().push(blob);
                }
            }
        })
    };
    recorder.set_ondataavailable(Some(ondata.as_ref().unchecked_ref()));

    let onstop = {
        let chunks = Rc::clone(&chunks);
        let done = Rc::clone(&done);
        let mime = mime.to_string();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let seconds = (js_sys::Date::now() - started_ms) / 1000.0;
            let result = save_clip(&chunks.borrow(), &mime, ext, seconds);
            *done.borrow_mut() = Some(result);
        })
    };
    recorder.set_onstop(Some(onstop.as_ref().unchecked_ref()));

    // An encoder error also fires onstop, which saves whatever chunks exist
    // (or reports "no data"); here we just log the underlying cause.
    let onerror = Closure::<dyn FnMut(web_sys::Event)>::new(|_| {
        log::error!("MediaRecorder error while recording");
    });
    recorder.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    recorder.start().map_err(|e| js_err(&e))?;

    Ok(Active {
        recorder,
        started_ms,
        stopping: false,
        done,
        _ondata: ondata,
        _onstop: onstop,
        _onerror: onerror,
    })
}

fn save_clip(chunks: &[Blob], mime: &str, ext: &str, seconds: f64) -> LastClip {
    if chunks.is_empty() {
        return LastClip::Error("recording produced no data".to_string());
    }
    let parts = js_sys::Array::new();
    for chunk in chunks {
        parts.push(chunk);
    }
    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let blob = match Blob::new_with_blob_sequence_and_options(&parts, &options) {
        Ok(blob) => blob,
        Err(err) => return LastClip::Error(js_err(&err)),
    };
    let bytes = blob.size();
    let filename = format!("poi-trails-{}.{ext}", timestamp());
    match download(&blob, &filename) {
        Ok(()) => LastClip::Saved {
            filename,
            seconds,
            bytes,
        },
        Err(err) => LastClip::Error(err),
    }
}

/// Trigger a download of `blob` via a temporary anchor + object URL.
fn download(blob: &Blob, filename: &str) -> Result<(), String> {
    let url = web_sys::Url::create_object_url_with_blob(blob).map_err(|e| js_err(&e))?;
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|e| js_err(&e))?
        .dyn_into()
        .map_err(|_| "not an anchor".to_string())?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();

    // Revoke on a delay — the download must grab the blob first (immediate
    // revocation is racy in some browsers). Until then the blob stays in RAM.
    let revoke = Closure::once_into_js(move || {
        web_sys::Url::revoke_object_url(&url).ok();
    });
    if let Some(window) = web_sys::window() {
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000);
    }
    Ok(())
}

fn timestamp() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    )
}

fn stop_button() -> Option<HtmlElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(STOP_BUTTON_ID)?
        .dyn_into()
        .ok()
}

fn set_stop_button_hidden(hidden: bool) {
    if let Some(button) = stop_button() {
        button.set_hidden(hidden);
    }
}

/// Install the stop triggers once, leaked for the page's lifetime (so no
/// closure can be dropped while an event might still fire): the stop button's
/// click, and Escape — which lets the user run rounds of record → spin → stop
/// without moving the mouse off the Record button. A stray request set while
/// idle is harmless: `start` clears the flag.
fn install_stop_listener() -> Result<(), String> {
    if STOP_LISTENER_INSTALLED.with(|c| c.get()) {
        return Ok(());
    }
    let button = stop_button().ok_or("missing #stop_recording button")?;
    let on_click = Closure::<dyn FnMut(web_sys::Event)>::new(|_| {
        STOP_REQUESTED.with(|c| c.set(true));
    });
    button
        .add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())
        .map_err(|e| js_err(&e))?;
    on_click.forget();

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let on_key = Closure::<dyn FnMut(web_sys::Event)>::new(|event: web_sys::Event| {
        if let Some(key_event) = event.dyn_ref::<web_sys::KeyboardEvent>() {
            if key_event.key() == "Escape" {
                STOP_REQUESTED.with(|c| c.set(true));
            }
        }
    });
    // Capture phase: runs regardless of focus, before egui's canvas handlers
    // could consume the event.
    document
        .add_event_listener_with_callback_and_bool(
            "keydown",
            on_key.as_ref().unchecked_ref(),
            true,
        )
        .map_err(|e| js_err(&e))?;
    on_key.forget();

    STOP_LISTENER_INSTALLED.with(|c| c.set(true));
    Ok(())
}

fn js_err(err: &JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    if let Some(dom_ex) = err.dyn_ref::<web_sys::DomException>() {
        return format!("{}: {}", dom_ex.name(), dom_ex.message());
    }
    format!("{err:?}")
}

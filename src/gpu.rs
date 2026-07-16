//! WebGL2 GPU pipeline (web only).
//!
//! Uploads each camera frame straight to a GPU texture via `texImage2D(video)`
//! — no `getImageData` CPU readback — and composites on the GPU. It shares the
//! same WebGL2 context eframe's `glow` backend already created on the main
//! canvas, and displays its result through an egui paint callback.
//!
//! The trails effect runs as a fragment shader with two render targets, writing
//! the next `displayed` (peak-hold) and `background` (adaptive model) state in
//! one pass, ping-ponged across frames. It mirrors `trails.rs::process_frame`.
//!
//! The pipeline lives in a `thread_local` because WebGL objects are not
//! `Send`/`Sync`, yet the egui paint callback closure must be `Send + Sync`.
//! The callback captures only plain `Copy` data and reaches into the
//! thread-local to draw (sound because wasm is single-threaded).

use std::cell::{Cell, RefCell};

use eframe::egui;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    HtmlCanvasElement, HtmlVideoElement, WebGl2RenderingContext as Gl, WebGlFramebuffer,
    WebGlProgram, WebGlShader, WebGlTexture, WebGlUniformLocation, WebGlVertexArrayObject,
};

thread_local! {
    static PIPELINE: RefCell<Option<GpuPipeline>> = const { RefCell::new(None) };
    /// Set while the WebGL context is lost; all GL work is skipped until restore.
    static CONTEXT_LOST: Cell<bool> = const { Cell::new(false) };
    /// Keeps the context-loss event closures alive for the page's lifetime.
    static LISTENERS: RefCell<Vec<Closure<dyn FnMut(web_sys::Event)>>> =
        const { RefCell::new(Vec::new()) };
}

const CANVAS_ID: &str = "the_canvas_id";

pub fn context_lost() -> bool {
    CONTEXT_LOST.with(|c| c.get())
}

/// Parameters for one trails pass — the user controls plus this frame's `dt`.
/// Mirrors the fields of `trails::TrailsProcessor`.
#[derive(Clone, Copy)]
pub struct TrailsParams {
    pub threshold: f32,
    pub intensity_gain: f32,
    pub dim_factor: f32,
    pub fade_seconds: f32,
    pub motion_gate: bool,
    pub motion_sensitivity: f32,
    pub background_seconds: f32,
    pub dt: f32,
}

/// Full-screen triangle from `gl_VertexID` (no vertex buffers). Straight UVs.
const QUAD_VS: &str = r#"#version 300 es
precision highp float;
out vec2 v_uv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    v_uv = p;
}
"#;

/// Display pass: same triangle, but flips V (video is top-down) and optionally
/// mirrors U, and clamps (trail values can exceed 1 when the gain is > 1).
const DISPLAY_VS: &str = r#"#version 300 es
precision highp float;
out vec2 v_uv;
uniform bool u_flip_x;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    vec2 uv = vec2(p.x, 1.0 - p.y);
    if (u_flip_x) uv.x = 1.0 - uv.x;
    v_uv = uv;
}
"#;

const DISPLAY_FS: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 frag;
void main() {
    frag = vec4(clamp(texture(u_tex, v_uv).rgb, 0.0, 1.0), 1.0);
}
"#;

/// The trails step. Everything is in 0..1 (with headroom above 1 for gain > 1).
/// Writes new displayed (peak-hold) and background (adaptive model).
const TRAILS_FS: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_source;
uniform sampler2D u_displayed_prev;
uniform sampler2D u_background_prev;
uniform float u_threshold;
uniform float u_gain;
uniform float u_dim;
uniform float u_decay;
uniform bool u_motion_gate;
uniform float u_edge0;
uniform float u_edge1;
uniform float u_bg_alpha;
uniform bool u_seed_displayed;
uniform bool u_seed_background;
layout(location = 0) out vec4 out_displayed;
layout(location = 1) out vec4 out_background;
void main() {
    vec3 src = texture(u_source, v_uv).rgb;
    float lum = dot(src, vec3(0.2126, 0.7152, 0.0722));

    float motion = 1.0;
    vec3 bg = src;
    if (u_motion_gate) {
        vec3 bgp = u_seed_background ? src : texture(u_background_prev, v_uv).rgb;
        vec3 d = abs(src - bgp);
        float diff = max(d.r, max(d.g, d.b));
        motion = smoothstep(u_edge0, u_edge1, diff);
        // Blind running-average background update.
        bg = bgp + u_bg_alpha * (src - bgp);
    }

    float source_weight = (lum > u_threshold) ? motion : 0.0;
    float gain = u_dim + source_weight * (u_gain - u_dim);
    vec3 target = src * gain;

    vec3 disp_prev = u_seed_displayed ? vec3(0.0) : texture(u_displayed_prev, v_uv).rgb;
    vec3 disp = max(target, disp_prev * u_decay);

    out_displayed = vec4(disp, 1.0);
    out_background = vec4(bg, 1.0);
}
"#;

/// Passthrough copy (source frame -> a delay-ring slot), straight UVs.
const COPY_FS: &str = r#"#version 300 es
precision highp float;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 frag;
void main() {
    frag = texture(u_tex, v_uv);
}
"#;

/// Store recent frames at most this often, matching the CPU delay buffer.
const DELAY_STORE_FPS: f64 = 30.0;
/// Extra history kept beyond the requested delay so the exact frame is available.
const DELAY_EVICT_MARGIN: f64 = 0.25;

struct DelayEntry {
    t: f64,
    tex: WebGlTexture,
}

/// A ring of GPU textures holding the last few seconds of frames (in VRAM),
/// mirroring `delay::DelayBuffer`. Storage is throttled to [`DELAY_STORE_FPS`]
/// and bounded to the current delay, and evicted textures are recycled.
struct DelayRing {
    entries: std::collections::VecDeque<DelayEntry>,
    pool: Vec<WebGlTexture>,
    now: f64,
    last_store: f64,
    dims: (i32, i32),
}

impl DelayRing {
    fn new() -> Self {
        Self {
            entries: std::collections::VecDeque::new(),
            pool: Vec::new(),
            now: 0.0,
            last_store: f64::NEG_INFINITY,
            dims: (0, 0),
        }
    }

    fn clear(&mut self, gl: &Gl) {
        for e in self.entries.drain(..) {
            gl.delete_texture(Some(&e.tex));
        }
        for t in self.pool.drain(..) {
            gl.delete_texture(Some(&t));
        }
        self.last_store = f64::NEG_INFINITY;
    }

    /// Advance time, store `source` (throttled), evict beyond `delay`, and
    /// return the texture to use as the (possibly delayed) source. The returned
    /// handle is a clone that refers to a live ring texture.
    #[allow(clippy::too_many_arguments)]
    fn tick(
        &mut self,
        gl: &Gl,
        copy_program: &WebGlProgram,
        u_copy: Option<&WebGlUniformLocation>,
        copy_fbo: &WebGlFramebuffer,
        quad_vao: &WebGlVertexArrayObject,
        source: &WebGlTexture,
        dims: (i32, i32),
        dt: f32,
        delay: f32,
    ) -> WebGlTexture {
        if dims != self.dims {
            self.clear(gl);
            self.dims = dims;
        }
        self.now += dt.max(0.0) as f64;

        if self.entries.is_empty() || (self.now - self.last_store) >= 1.0 / DELAY_STORE_FPS {
            // If we can't recycle a slot and a new one won't allocate (out of
            // GPU memory), skip storing: the ring stops growing and the delay is
            // effectively capped at what already fits.
            if let Some(tex) = self.pool.pop().or_else(|| create_rgba8(gl, dims.0, dims.1)) {
                copy_into(gl, copy_program, u_copy, copy_fbo, quad_vao, source, &tex, dims);
                self.entries.push_back(DelayEntry { t: self.now, tex });
                self.last_store = self.now;
            }
        }

        // Nothing stored yet (e.g. the first allocation failed) — show live.
        if self.entries.is_empty() {
            return source.clone();
        }

        let keep = delay.max(0.0) as f64 + DELAY_EVICT_MARGIN;
        while self.entries.len() > 1 && (self.now - self.entries.front().unwrap().t) > keep {
            let e = self.entries.pop_front().unwrap();
            self.pool.push(e.tex);
        }

        let target = self.now - delay.max(0.0) as f64;
        let chosen = self
            .entries
            .iter()
            .rev()
            .find(|e| e.t <= target)
            .unwrap_or_else(|| self.entries.front().unwrap());
        chosen.tex.clone()
    }
}

struct TrailsUniforms {
    source: Option<WebGlUniformLocation>,
    displayed_prev: Option<WebGlUniformLocation>,
    background_prev: Option<WebGlUniformLocation>,
    threshold: Option<WebGlUniformLocation>,
    gain: Option<WebGlUniformLocation>,
    dim: Option<WebGlUniformLocation>,
    decay: Option<WebGlUniformLocation>,
    motion_gate: Option<WebGlUniformLocation>,
    edge0: Option<WebGlUniformLocation>,
    edge1: Option<WebGlUniformLocation>,
    bg_alpha: Option<WebGlUniformLocation>,
    seed_displayed: Option<WebGlUniformLocation>,
    seed_background: Option<WebGlUniformLocation>,
}

struct GpuPipeline {
    gl: Gl,
    empty_vao: WebGlVertexArrayObject,

    display_program: WebGlProgram,
    u_display_tex: Option<WebGlUniformLocation>,
    u_display_flip: Option<WebGlUniformLocation>,

    trails_program: WebGlProgram,
    trails_u: TrailsUniforms,
    fbo: WebGlFramebuffer,
    displayed: [WebGlTexture; 2],
    background: [WebGlTexture; 2],
    cur: usize,
    state_dims: Option<(i32, i32)>,
    /// False if the last state-texture allocation failed (out of GPU memory);
    /// trails then fall back to a plain mirror instead of rendering garbage.
    state_ok: bool,
    seed_displayed: bool,
    seed_background: bool,

    source_tex: WebGlTexture,
    source_dims: Option<(i32, i32)>,
    /// Whether the result to display is the trails output (else the raw source).
    trails_active: bool,

    // Delay ring (VRAM). `current_source` is the (possibly delayed) frame both
    // the mirror display and the trails pass use as their input this frame.
    copy_program: WebGlProgram,
    u_copy_tex: Option<WebGlUniformLocation>,
    copy_fbo: WebGlFramebuffer,
    delay_ring: DelayRing,
    current_source: WebGlTexture,
}

impl GpuPipeline {
    fn new() -> Result<Self, String> {
        let gl = context()?;
        // Needed to render into RGBA16F state textures.
        let _ = gl.get_extension("EXT_color_buffer_float");

        let empty_vao = gl
            .create_vertex_array()
            .ok_or("failed to create vertex array")?;

        let display_program = link_program(&gl, DISPLAY_VS, DISPLAY_FS)?;
        let u_display_tex = gl.get_uniform_location(&display_program, "u_tex");
        let u_display_flip = gl.get_uniform_location(&display_program, "u_flip_x");

        let trails_program = link_program(&gl, QUAD_VS, TRAILS_FS)?;
        let trails_u = TrailsUniforms {
            source: gl.get_uniform_location(&trails_program, "u_source"),
            displayed_prev: gl.get_uniform_location(&trails_program, "u_displayed_prev"),
            background_prev: gl.get_uniform_location(&trails_program, "u_background_prev"),
            threshold: gl.get_uniform_location(&trails_program, "u_threshold"),
            gain: gl.get_uniform_location(&trails_program, "u_gain"),
            dim: gl.get_uniform_location(&trails_program, "u_dim"),
            decay: gl.get_uniform_location(&trails_program, "u_decay"),
            motion_gate: gl.get_uniform_location(&trails_program, "u_motion_gate"),
            edge0: gl.get_uniform_location(&trails_program, "u_edge0"),
            edge1: gl.get_uniform_location(&trails_program, "u_edge1"),
            bg_alpha: gl.get_uniform_location(&trails_program, "u_bg_alpha"),
            seed_displayed: gl.get_uniform_location(&trails_program, "u_seed_displayed"),
            seed_background: gl.get_uniform_location(&trails_program, "u_seed_background"),
        };

        let fbo = gl.create_framebuffer().ok_or("failed to create framebuffer")?;
        let displayed = [new_float_texture(&gl)?, new_float_texture(&gl)?];
        let background = [new_float_texture(&gl)?, new_float_texture(&gl)?];
        let source_tex = new_texture(&gl)?;

        let copy_program = link_program(&gl, QUAD_VS, COPY_FS)?;
        let u_copy_tex = gl.get_uniform_location(&copy_program, "u_tex");
        let copy_fbo = gl
            .create_framebuffer()
            .ok_or("failed to create copy framebuffer")?;
        let current_source = source_tex.clone();

        Ok(Self {
            gl,
            empty_vao,
            display_program,
            u_display_tex,
            u_display_flip,
            trails_program,
            trails_u,
            fbo,
            displayed,
            background,
            cur: 0,
            state_dims: None,
            state_ok: true,
            seed_displayed: true,
            seed_background: true,
            source_tex,
            source_dims: None,
            trails_active: false,
            copy_program,
            u_copy_tex,
            copy_fbo,
            delay_ring: DelayRing::new(),
            current_source,
        })
    }

    /// Choose the (possibly delayed) source frame for this tick. `delay` in
    /// seconds; near-zero bypasses the ring and frees its VRAM.
    fn set_delay(&mut self, delay: f32, dt: f32) {
        match self.source_dims {
            Some(dims) if delay > 0.05 => {
                self.current_source = self.delay_ring.tick(
                    &self.gl,
                    &self.copy_program,
                    self.u_copy_tex.as_ref(),
                    &self.copy_fbo,
                    &self.empty_vao,
                    &self.source_tex,
                    dims,
                    dt,
                    delay,
                );
            }
            _ => {
                self.delay_ring.clear(&self.gl);
                self.current_source = self.source_tex.clone();
            }
        }
    }

    fn upload_video(&mut self, video: &HtmlVideoElement) -> Option<(i32, i32)> {
        let (w, h) = (video.video_width() as i32, video.video_height() as i32);
        if w == 0 || h == 0 {
            return None;
        }
        self.gl.bind_texture(Gl::TEXTURE_2D, Some(&self.source_tex));
        self.gl
            .tex_image_2d_with_u32_and_u32_and_html_video_element(
                Gl::TEXTURE_2D,
                0,
                Gl::RGBA as i32,
                Gl::RGBA,
                Gl::UNSIGNED_BYTE,
                video,
            )
            .ok()?;
        self.source_dims = Some((w, h));
        Some((w, h))
    }

    /// Resize the float state textures to the given dims and mark them for
    /// re-seeding (background <- source, displayed <- 0).
    fn ensure_state(&mut self, w: i32, h: i32) {
        if self.state_dims == Some((w, h)) {
            return;
        }
        for t in self.displayed.iter().chain(self.background.iter()) {
            alloc_float(&self.gl, t, w, h);
        }
        self.state_ok = self.gl.get_error() == Gl::NO_ERROR;
        if !self.state_ok {
            log::warn!("trails state textures failed to allocate (out of GPU memory?); mirroring");
        }
        self.state_dims = Some((w, h));
        self.seed_displayed = true;
        self.seed_background = true;
    }

    fn process_trails(&mut self, p: TrailsParams) {
        let (w, h) = match self.source_dims {
            Some(d) => d,
            None => return,
        };
        self.ensure_state(w, h);
        if !self.state_ok {
            // Not enough GPU memory for the trail state — show the live frame.
            self.trails_active = false;
            return;
        }

        let prev = self.cur;
        let next = 1 - self.cur;
        let gl = &self.gl;

        gl.bind_framebuffer(Gl::FRAMEBUFFER, Some(&self.fbo));
        gl.framebuffer_texture_2d(
            Gl::FRAMEBUFFER,
            Gl::COLOR_ATTACHMENT0,
            Gl::TEXTURE_2D,
            Some(&self.displayed[next]),
            0,
        );
        gl.framebuffer_texture_2d(
            Gl::FRAMEBUFFER,
            Gl::COLOR_ATTACHMENT1,
            Gl::TEXTURE_2D,
            Some(&self.background[next]),
            0,
        );
        let draw_bufs = js_sys::Array::new();
        draw_bufs.push(&JsValue::from_f64(Gl::COLOR_ATTACHMENT0 as f64));
        draw_bufs.push(&JsValue::from_f64(Gl::COLOR_ATTACHMENT1 as f64));
        gl.draw_buffers(&draw_bufs);

        gl.viewport(0, 0, w, h);
        gl.use_program(Some(&self.trails_program));
        gl.bind_vertex_array(Some(&self.empty_vao));

        gl.active_texture(Gl::TEXTURE0);
        gl.bind_texture(Gl::TEXTURE_2D, Some(&self.current_source));
        gl.uniform1i(self.trails_u.source.as_ref(), 0);
        gl.active_texture(Gl::TEXTURE1);
        gl.bind_texture(Gl::TEXTURE_2D, Some(&self.displayed[prev]));
        gl.uniform1i(self.trails_u.displayed_prev.as_ref(), 1);
        gl.active_texture(Gl::TEXTURE2);
        gl.bind_texture(Gl::TEXTURE_2D, Some(&self.background[prev]));
        gl.uniform1i(self.trails_u.background_prev.as_ref(), 2);

        let fps = 1.0 / p.dt.max(1.0 / 240.0);
        let decay = if p.fade_seconds <= 0.0 {
            0.0
        } else {
            0.5_f32.powf(1.0 / (p.fade_seconds * fps))
        };
        let bg_alpha = if p.background_seconds <= 0.0 {
            1.0
        } else {
            1.0 - 0.5_f32.powf(1.0 / (p.background_seconds * fps))
        };
        let edge0 = (1.0 - p.motion_sensitivity) * 0.5;
        let edge1 = edge0 + 0.1;

        gl.uniform1f(self.trails_u.threshold.as_ref(), p.threshold);
        gl.uniform1f(self.trails_u.gain.as_ref(), p.intensity_gain);
        gl.uniform1f(self.trails_u.dim.as_ref(), p.dim_factor);
        gl.uniform1f(self.trails_u.decay.as_ref(), decay);
        gl.uniform1i(self.trails_u.motion_gate.as_ref(), p.motion_gate as i32);
        gl.uniform1f(self.trails_u.edge0.as_ref(), edge0);
        gl.uniform1f(self.trails_u.edge1.as_ref(), edge1);
        gl.uniform1f(self.trails_u.bg_alpha.as_ref(), bg_alpha);
        gl.uniform1i(self.trails_u.seed_displayed.as_ref(), self.seed_displayed as i32);
        gl.uniform1i(
            self.trails_u.seed_background.as_ref(),
            self.seed_background as i32,
        );

        gl.draw_arrays(Gl::TRIANGLES, 0, 3);

        gl.bind_framebuffer(Gl::FRAMEBUFFER, None);
        gl.bind_vertex_array(None);

        self.cur = next;
        self.seed_displayed = false;
        self.seed_background = false;
        self.trails_active = true;
    }

    fn result_tex(&self) -> &WebGlTexture {
        if self.trails_active {
            &self.displayed[self.cur]
        } else {
            &self.current_source
        }
    }

    fn draw_display(&self, rect_px: [i32; 4], flip_x: bool) {
        let gl = &self.gl;
        let [x, top, w, h] = rect_px;
        let gl_y = gl.drawing_buffer_height() - (top + h);
        gl.viewport(x, gl_y, w, h);
        gl.use_program(Some(&self.display_program));
        gl.bind_vertex_array(Some(&self.empty_vao));
        gl.active_texture(Gl::TEXTURE0);
        gl.bind_texture(Gl::TEXTURE_2D, Some(self.result_tex()));
        if let Some(u) = &self.u_display_tex {
            gl.uniform1i(Some(u), 0);
        }
        if let Some(u) = &self.u_display_flip {
            gl.uniform1i(Some(u), flip_x as i32);
        }
        gl.draw_arrays(Gl::TRIANGLES, 0, 3);
        gl.bind_vertex_array(None);
    }
}

// --- public API ------------------------------------------------------------

pub fn init() -> Result<(), String> {
    let pipeline = GpuPipeline::new()?;
    PIPELINE.with(|p| *p.borrow_mut() = Some(pipeline));
    CONTEXT_LOST.with(|c| c.set(false));
    install_context_listeners()?;
    Ok(())
}

/// Upload the current video frame; returns its `(width, height)`.
pub fn upload_video(video: &HtmlVideoElement) -> Option<(i32, i32)> {
    if context_lost() {
        return None;
    }
    PIPELINE.with(|p| p.borrow_mut().as_mut().and_then(|g| g.upload_video(video)))
}

/// Select the (possibly delayed) source frame for this tick. Call once per
/// frame after `upload_video`, before `process_trails`/`use_mirror`.
pub fn set_delay(delay_seconds: f32, dt: f32) {
    if context_lost() {
        return;
    }
    PIPELINE.with(|p| {
        if let Some(g) = p.borrow_mut().as_mut() {
            g.set_delay(delay_seconds, dt);
        }
    });
}

/// Run one trails pass over the current source frame.
pub fn process_trails(params: TrailsParams) {
    if context_lost() {
        return;
    }
    PIPELINE.with(|p| {
        if let Some(g) = p.borrow_mut().as_mut() {
            g.process_trails(params);
        }
    });
}

/// Display the raw source this frame (Mirror mode) instead of the trails output.
pub fn use_mirror() {
    PIPELINE.with(|p| {
        if let Some(g) = p.borrow_mut().as_mut() {
            g.trails_active = false;
        }
    });
}

/// Clear the accumulated trails (keep the learned background).
pub fn clear_trails() {
    PIPELINE.with(|p| {
        if let Some(g) = p.borrow_mut().as_mut() {
            g.seed_displayed = true;
        }
    });
}

/// Re-learn the static background from the current frame.
pub fn reset_background() {
    PIPELINE.with(|p| {
        if let Some(g) = p.borrow_mut().as_mut() {
            g.seed_background = true;
        }
    });
}

pub fn source_dims() -> Option<(i32, i32)> {
    PIPELINE.with(|p| p.borrow().as_ref().and_then(|g| g.source_dims))
}

pub fn display_callback(rect: egui::Rect, rect_px: [i32; 4], flip_x: bool) -> egui::PaintCallback {
    egui::PaintCallback {
        rect,
        callback: std::sync::Arc::new(eframe::egui_glow::CallbackFn::new(
            move |_info, _painter| {
                if context_lost() {
                    return;
                }
                PIPELINE.with(|p| {
                    if let Some(g) = p.borrow().as_ref() {
                        g.draw_display(rect_px, flip_x);
                    }
                });
            },
        )),
    }
}

// --- helpers ---------------------------------------------------------------

fn canvas_element() -> Result<HtmlCanvasElement, String> {
    web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?
        .get_element_by_id(CANVAS_ID)
        .ok_or("canvas element not found")?
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| "element is not a canvas".to_string())
}

fn context() -> Result<Gl, String> {
    // eframe already created the webgl2 context on this canvas; requesting it
    // again returns the same context object.
    canvas_element()?
        .get_context("webgl2")
        .map_err(|_| "get_context(webgl2) failed")?
        .ok_or("webgl2 not available")?
        .dyn_into::<Gl>()
        .map_err(|_| "context is not WebGl2".to_string())
}

/// Listen for context loss/restore so the app degrades gracefully instead of
/// going permanently blank: on loss we skip all GL work; on restore we rebuild
/// the pipeline (all GL objects are invalidated by a loss).
fn install_context_listeners() -> Result<(), String> {
    // Install exactly once, even if `init` is somehow called again.
    if LISTENERS.with(|l| !l.borrow().is_empty()) {
        return Ok(());
    }
    let canvas = canvas_element()?;

    let on_lost = Closure::<dyn FnMut(web_sys::Event)>::new(|e: web_sys::Event| {
        // Prevent the default so the browser will attempt to restore the context.
        e.prevent_default();
        CONTEXT_LOST.with(|c| c.set(true));
        log::warn!("WebGL context lost");
    });
    canvas
        .add_event_listener_with_callback("webglcontextlost", on_lost.as_ref().unchecked_ref())
        .map_err(|_| "failed to add webglcontextlost listener".to_string())?;

    let on_restored = Closure::<dyn FnMut(web_sys::Event)>::new(|_e: web_sys::Event| {
        log::warn!("WebGL context restored; rebuilding GPU pipeline");
        match GpuPipeline::new() {
            Ok(pipeline) => {
                PIPELINE.with(|slot| *slot.borrow_mut() = Some(pipeline));
                CONTEXT_LOST.with(|c| c.set(false));
            }
            Err(err) => log::error!("GPU pipeline rebuild failed after restore: {err}"),
        }
    });
    canvas
        .add_event_listener_with_callback(
            "webglcontextrestored",
            on_restored.as_ref().unchecked_ref(),
        )
        .map_err(|_| "failed to add webglcontextrestored listener".to_string())?;

    LISTENERS.with(|l| {
        let mut v = l.borrow_mut();
        v.push(on_lost);
        v.push(on_restored);
    });
    Ok(())
}

fn set_texture_params(gl: &Gl) {
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MIN_FILTER, Gl::LINEAR as i32);
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MAG_FILTER, Gl::LINEAR as i32);
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE as i32);
    gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE as i32);
}

fn new_texture(gl: &Gl) -> Result<WebGlTexture, String> {
    let tex = gl.create_texture().ok_or("failed to create texture")?;
    gl.bind_texture(Gl::TEXTURE_2D, Some(&tex));
    set_texture_params(gl);
    Ok(tex)
}

fn new_float_texture(gl: &Gl) -> Result<WebGlTexture, String> {
    let tex = gl.create_texture().ok_or("failed to create texture")?;
    gl.bind_texture(Gl::TEXTURE_2D, Some(&tex));
    set_texture_params(gl);
    Ok(tex)
}

/// An RGBA8 texture of the given size for a delay-ring slot. Returns `None` if
/// allocation fails (e.g. out of GPU memory), so the ring can stop growing
/// rather than render garbage — this naturally caps the delay at what fits.
fn create_rgba8(gl: &Gl, w: i32, h: i32) -> Option<WebGlTexture> {
    let tex = gl.create_texture()?;
    gl.bind_texture(Gl::TEXTURE_2D, Some(&tex));
    set_texture_params(gl);
    let _ = gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
        Gl::TEXTURE_2D,
        0,
        Gl::RGBA as i32,
        w,
        h,
        0,
        Gl::RGBA,
        Gl::UNSIGNED_BYTE,
        None,
    );
    if gl.get_error() != Gl::NO_ERROR {
        gl.delete_texture(Some(&tex));
        log::warn!("delay-ring texture failed to allocate; capping delay length");
        return None;
    }
    Some(tex)
}

/// Copy `source` into `dest` (both RGBA8, same `dims`) via a passthrough draw.
#[allow(clippy::too_many_arguments)]
fn copy_into(
    gl: &Gl,
    program: &WebGlProgram,
    u_tex: Option<&WebGlUniformLocation>,
    fbo: &WebGlFramebuffer,
    quad_vao: &WebGlVertexArrayObject,
    source: &WebGlTexture,
    dest: &WebGlTexture,
    dims: (i32, i32),
) {
    gl.bind_framebuffer(Gl::FRAMEBUFFER, Some(fbo));
    gl.framebuffer_texture_2d(
        Gl::FRAMEBUFFER,
        Gl::COLOR_ATTACHMENT0,
        Gl::TEXTURE_2D,
        Some(dest),
        0,
    );
    let bufs = js_sys::Array::new();
    bufs.push(&JsValue::from_f64(Gl::COLOR_ATTACHMENT0 as f64));
    gl.draw_buffers(&bufs);
    gl.viewport(0, 0, dims.0, dims.1);
    gl.use_program(Some(program));
    gl.bind_vertex_array(Some(quad_vao));
    gl.active_texture(Gl::TEXTURE0);
    gl.bind_texture(Gl::TEXTURE_2D, Some(source));
    gl.uniform1i(u_tex, 0);
    gl.draw_arrays(Gl::TRIANGLES, 0, 3);
    gl.bind_framebuffer(Gl::FRAMEBUFFER, None);
    gl.bind_vertex_array(None);
}

/// (Re)allocate `tex` as an RGBA16F texture of the given size (contents undefined
/// until first written — seeding handles that).
fn alloc_float(gl: &Gl, tex: &WebGlTexture, w: i32, h: i32) {
    gl.bind_texture(Gl::TEXTURE_2D, Some(tex));
    let _ = gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
        Gl::TEXTURE_2D,
        0,
        Gl::RGBA16F as i32,
        w,
        h,
        0,
        Gl::RGBA,
        Gl::HALF_FLOAT,
        None,
    );
}

fn compile_shader(gl: &Gl, kind: u32, src: &str) -> Result<WebGlShader, String> {
    let shader = gl.create_shader(kind).ok_or("failed to create shader")?;
    gl.shader_source(&shader, src);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, Gl::COMPILE_STATUS)
        .as_bool()
        == Some(true)
    {
        Ok(shader)
    } else {
        Err(gl
            .get_shader_info_log(&shader)
            .unwrap_or_else(|| "unknown shader compile error".into()))
    }
}

fn link_program(gl: &Gl, vs: &str, fs: &str) -> Result<WebGlProgram, String> {
    let vert = compile_shader(gl, Gl::VERTEX_SHADER, vs)?;
    let frag = compile_shader(gl, Gl::FRAGMENT_SHADER, fs)?;
    let program = gl.create_program().ok_or("failed to create program")?;
    gl.attach_shader(&program, &vert);
    gl.attach_shader(&program, &frag);
    gl.link_program(&program);
    if gl
        .get_program_parameter(&program, Gl::LINK_STATUS)
        .as_bool()
        == Some(true)
    {
        Ok(program)
    } else {
        Err(gl
            .get_program_info_log(&program)
            .unwrap_or_else(|| "unknown program link error".into()))
    }
}

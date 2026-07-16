use eframe::egui;

use crate::app::{Mode, PoiTrailsApp};

#[cfg(target_arch = "wasm32")]
use crate::camera::CameraStatus;

/// A small, muted, wrapping line of explanatory text shown beneath a control.
fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().weak());
}

/// Hide the controls and enter browser (web) / window (native) fullscreen.
fn go_immersive(app: &mut PoiTrailsApp, _ctx: &egui::Context) {
    app.show_controls = false;
    #[cfg(target_arch = "wasm32")]
    {
        app.immersive = true;
        crate::fullscreen::request();
    }
    #[cfg(not(target_arch = "wasm32"))]
    _ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
}

/// Restore the controls and leave fullscreen.
fn leave_immersive(app: &mut PoiTrailsApp, _ctx: &egui::Context) {
    app.show_controls = true;
    #[cfg(target_arch = "wasm32")]
    {
        app.immersive = false;
        crate::fullscreen::exit();
    }
    #[cfg(not(target_arch = "wasm32"))]
    _ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
}

pub fn draw(app: &mut PoiTrailsApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();

    // In real fullscreen the view is just the video — no restore button; the
    // user exits with Esc (which brings the controls back). Only offer the
    // floating button when controls were hidden *without* going fullscreen.
    // While recording, no egui chrome at all — it would be captured in the
    // clip; the DOM stop button (outside the canvas pixels) is the way back.
    if !app.show_controls && !app.is_immersive() && !app.is_recording() {
        // A single unobtrusive button, floating top-right, to bring everything
        // back. Always reachable even after the browser drops fullscreen.
        egui::Area::new(egui::Id::new("restore_controls"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
            .show(&ctx, |ui| {
                if ui
                    .button("Show controls")
                    .on_hover_text("Show the control panel and exit fullscreen.")
                    .clicked()
                {
                    leave_immersive(app, &ctx);
                }
            });
    }

    if app.show_controls {
        egui::Panel::right("controls")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::Panel::bottom("github_link")
                    .show_separator_line(true)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.vertical_centered(|ui| {
                            ui.add(
                                egui::Hyperlink::from_label_and_url(
                                    "Source on GitHub",
                                    "https://github.com/cconstantine/poi_trails",
                                )
                                .open_in_new_tab(true),
                            );
                        });
                        #[cfg(target_arch = "wasm32")]
                        caption(
                            ui,
                            "Anonymous visit counting only (GoatCounter, no cookies). \
                             Your video never leaves this device.",
                        );
                        ui.add_space(4.0);
                    });

                ui.heading("Poi Trails");
                caption(
                    ui,
                    "Watch yourself spin poi, with glowing light trails. Hover any \
                 control for a tip.",
                );

                ui.horizontal(|ui| {
                    if ui
                        .button("Fullscreen")
                        .on_hover_text("Fill the screen and hide these controls.")
                        .clicked()
                    {
                        go_immersive(app, &ctx);
                    }
                    if ui
                        .button("Hide controls")
                        .on_hover_text("Hide this panel without going fullscreen.")
                        .clicked()
                    {
                        app.show_controls = false;
                    }
                });

                #[cfg(target_arch = "wasm32")]
                draw_record_controls(app, ui);

                ui.separator();

                #[cfg(target_arch = "wasm32")]
                draw_camera_controls(app, ui);
                #[cfg(not(target_arch = "wasm32"))]
                ui.label("Native preview: synthetic test pattern (no camera).");

                ui.separator();
                ui.label("Mode");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut app.mode, Mode::Live, "Mirror")
                        .on_hover_text("Plain flipped webcam view, like looking in a mirror.");
                    ui.selectable_value(&mut app.mode, Mode::Trails, "Trails")
                        .on_hover_text(
                            "Bright moving objects — like glowing poi — leave fading light trails.",
                        );
                });
                caption(
                    ui,
                    match app.mode {
                        Mode::Live => "Just a mirror. Switch to Trails to paint with light.",
                        Mode::Trails => "Bright moving lights leave glowing streaks that fade out.",
                    },
                );

                ui.checkbox(&mut app.mirror_enabled, "Mirror flip")
                    .on_hover_text(
                        "Flip the picture left-to-right so it moves like a real mirror. \
                 Applies in both modes.",
                    );

                ui.add(
                    egui::Slider::new(
                        &mut app.delay_seconds,
                        0.0..=crate::delay::MAX_DELAY_SECONDS,
                    )
                    .text("Delay (s)"),
                )
                .on_hover_text(
                    "Show the video from a few seconds ago, so you can do a move and \
                     then look up to review it. 0 = live.",
                );
                if app.delay_seconds > 0.05 {
                    caption(
                        ui,
                        "Showing the past — perform your move, then watch it here a \
                         moment later.",
                    );
                    let mb = app.projected_delay_bytes() as f64 / (1024.0 * 1024.0);
                    let res = app
                        .current_resolution()
                        .map(|(w, h)| format!(" ({w}×{h})"))
                        .unwrap_or_default();
                    caption(ui, &format!("Delay buffer: ~{mb:.0} MB{res}."));
                }

                if app.mode == Mode::Trails {
                    ui.separator();
                    ui.label("Trails settings");
                    ui.add(
                        egui::Slider::new(&mut app.trails.fade_seconds, 0.2..=3.0)
                            .text("Trail fade (s)"),
                    )
                    .on_hover_text(
                        "How many seconds a trail takes to fade away. \
                     Higher = longer-lasting streaks.",
                    );

                    // Finer tuning knobs, hidden by default to avoid confusion.
                    ui.collapsing("Advanced", |ui| {
                        ui.add(
                            egui::Slider::new(&mut app.trails.threshold, 0.0..=1.0)
                                .text("Brightness threshold"),
                        )
                        .on_hover_text(
                            "How bright something must be before it leaves a trail. \
                         Higher = only the brightest lights trail.",
                        );
                        ui.add(
                            egui::Slider::new(&mut app.trails.intensity_gain, 0.1..=2.0)
                                .text("Brightness boost"),
                        )
                        .on_hover_text(
                            "Makes trails glow brighter than the original light. \
                         Turn up if trails look too dim.",
                        );
                        ui.add(
                            egui::Slider::new(&mut app.trails.dim_factor, 0.0..=1.0)
                                .text("Background dim"),
                        )
                        .on_hover_text(
                            "How dark the live picture behind the trails is. \
                         Lower = trails stand out more.",
                        );

                        caption(
                            ui,
                            "Only moving lights trail — still-but-bright things (lamps, \
                         windows) are kept out.",
                        );
                        ui.add(
                            egui::Slider::new(&mut app.trails.motion_sensitivity, 0.0..=1.0)
                                .text("Motion sensitivity"),
                        )
                        .on_hover_text(
                            "How much something must move to count. Higher picks up smaller \
                         or slower movement — but also more clutter.",
                        );
                        ui.add(
                            egui::Slider::new(&mut app.trails.background_seconds, 0.5..=15.0)
                                .text("Background adapt (s)"),
                        )
                        .on_hover_text(
                            "How quickly a light that stops moving is treated as background \
                         and stops trailing.",
                        );
                    });
                }

                ui.separator();
                if ui
                    .button("Reset to defaults")
                    .on_hover_text(
                        "Restore Mode, Mirror flip, Delay, and all trail / \
                         background settings to their defaults. Your camera and \
                         quality choice are left as-is.",
                    )
                    .clicked()
                {
                    app.reset_to_defaults();
                }
            });
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            draw_video(app, ui);
        });
}

/// The "Record clip" button plus the outcome of the last recording. The clip
/// captures the clean, controls-hidden view; stopping is via the DOM overlay
/// button so it never appears in the recording.
#[cfg(target_arch = "wasm32")]
fn draw_record_controls(app: &mut PoiTrailsApp, ui: &mut egui::Ui) {
    use crate::record::{LastClip, MAX_CLIP_SECONDS};

    let camera_ready = matches!(app.camera_status(), CameraStatus::Ready);
    if ui
        .add_enabled(camera_ready, egui::Button::new("● Record clip"))
        .on_hover_text(format!(
            "Hide these controls and record the view to a video file \
             (up to {MAX_CLIP_SECONDS:.0} s). Stop with the Esc key — the \
             mouse can stay right here for the next take — or the button \
             in the top-left corner.",
        ))
        .on_disabled_hover_text("Enable the camera first.")
        .clicked()
    {
        app.start_recording();
    }

    match app.last_clip() {
        Some(LastClip::Saved {
            filename,
            seconds,
            bytes,
        }) => caption(
            ui,
            &format!(
                "Saved {filename} ({seconds:.0} s, {:.1} MB).",
                bytes / (1024.0 * 1024.0)
            ),
        ),
        Some(LastClip::Error(msg)) => {
            ui.label(
                egui::RichText::new(format!("Recording failed: {msg}"))
                    .small()
                    .color(egui::Color32::RED),
            );
        }
        None => {}
    }
}

#[cfg(target_arch = "wasm32")]
fn draw_camera_controls(app: &mut PoiTrailsApp, ui: &mut egui::Ui) {
    match app.camera_status() {
        CameraStatus::NotStarted => {
            if ui
                .button("Enable Camera")
                .on_hover_text("Turn on your webcam. Your browser will ask for permission.")
                .clicked()
            {
                // Reuse the camera chosen on a previous visit, if any.
                app.request_camera(app.selected_device.clone());
            }
            caption(
                ui,
                "Everything runs in your browser — your video never leaves this device.",
            );
        }
        CameraStatus::Requesting => {
            ui.label("Requesting camera access…");
            caption(ui, "Please allow camera access in your browser's prompt.");
        }
        CameraStatus::Ready => {
            let devices = app.camera_devices();
            if !devices.is_empty() {
                let selected_label = devices
                    .iter()
                    .find(|d| Some(&d.device_id) == app.selected_device.as_ref())
                    .map(|d| device_label(d))
                    .unwrap_or_else(|| "Default".to_string());

                egui::ComboBox::from_label("Camera")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for device in &devices {
                            let is_selected =
                                Some(&device.device_id) == app.selected_device.as_ref();
                            if ui
                                .selectable_label(is_selected, device_label(device))
                                .clicked()
                            {
                                app.request_camera(Some(device.device_id.clone()));
                            }
                        }
                    });
            }

            draw_quality_picker(app, ui);
        }
        CameraStatus::Error(msg) => {
            ui.colored_label(egui::Color32::RED, format!("Camera error: {msg}"));
            caption(
                ui,
                "If you blocked the camera, allow it in your browser's site \
                 settings (often a camera icon in the address bar), then retry.",
            );
            if ui.button("Retry").clicked() {
                app.request_camera(None);
            }
        }
    }
}

/// Camera resolution picker. "Auto" requests the camera's native maximum; the
/// presets are lower fixed resolutions (a smaller delay buffer / less GPU work).
/// The browser only exposes a supported max, so presets above it are hidden.
#[cfg(target_arch = "wasm32")]
fn draw_quality_picker(app: &mut PoiTrailsApp, ui: &mut egui::Ui) {
    use crate::app::{resolution_label, RESOLUTION_PRESETS};

    let max = app.camera_max_resolution();
    let current = app.capture_resolution();

    egui::ComboBox::from_label("Quality")
        .selected_text(resolution_label(current))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(current.is_none(), resolution_label(None))
                .clicked()
            {
                app.set_capture_resolution(None);
            }
            for preset in RESOLUTION_PRESETS {
                // Skip presets the camera can't reach (when its max is known).
                let fits = max.map_or(true, |(mw, mh)| preset.0 <= mw && preset.1 <= mh);
                if fits
                    && ui
                        .selectable_label(current == Some(preset), resolution_label(Some(preset)))
                        .clicked()
                {
                    app.set_capture_resolution(Some(preset));
                }
            }
        })
        .response
        .on_hover_text(
            "Camera resolution. Auto uses the camera's max — sharper but more \
             memory for the delay buffer and more GPU work.",
        );

    if let Some((mw, mh)) = max {
        caption(ui, &format!("Camera supports up to {mw}×{mh}."));
    }
}

#[cfg(target_arch = "wasm32")]
fn device_label(device: &crate::video_frame::CameraDevice) -> String {
    if device.label.is_empty() {
        device.device_id.clone()
    } else {
        device.label.clone()
    }
}

/// Web: the composited frame lives in a GPU texture; draw it via a paint
/// callback, letterboxed and optionally mirrored.
#[cfg(target_arch = "wasm32")]
fn draw_video(app: &PoiTrailsApp, ui: &mut egui::Ui) {
    let Some((sw, sh)) = crate::gpu::source_dims() else {
        ui.centered_and_justified(|ui| {
            ui.label("Waiting for camera…");
        });
        return;
    };
    if sw <= 0 || sh <= 0 {
        return;
    }

    let lb = letterbox_rect(egui::vec2(sw as f32, sh as f32), ui.max_rect());

    // The letterbox rect in physical pixels (top-left origin); the GPU code
    // flips Y to GL's bottom-left origin itself.
    let ppp = ui.ctx().pixels_per_point();
    let rect_px = [
        (lb.min.x * ppp).round() as i32,
        (lb.min.y * ppp).round() as i32,
        (lb.width() * ppp).round() as i32,
        (lb.height() * ppp).round() as i32,
    ];

    ui.painter()
        .add(crate::gpu::display_callback(lb, rect_px, app.mirror_enabled));
}

/// Native: the composited frame is an egui texture uploaded from the CPU.
#[cfg(not(target_arch = "wasm32"))]
fn draw_video(app: &PoiTrailsApp, ui: &mut egui::Ui) {
    let Some(texture) = app.texture() else {
        ui.centered_and_justified(|ui| {
            ui.label("Waiting for camera…");
        });
        return;
    };

    let uv = if app.mirror_enabled {
        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
    } else {
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
    };

    // Letterbox: scale the video to fit the available area while keeping its
    // aspect ratio, then center it. Whatever it doesn't cover stays black.
    let tex_size = texture.size_vec2();
    if tex_size.x <= 0.0 || tex_size.y <= 0.0 {
        return;
    }
    let rect = letterbox_rect(tex_size, ui.max_rect());
    egui::Image::new(texture).uv(uv).paint_at(ui, rect);
}

/// The largest rect with `content`'s aspect ratio that fits inside `area`,
/// centered within it (i.e. letterboxed / pillarboxed).
fn letterbox_rect(content: egui::Vec2, area: egui::Rect) -> egui::Rect {
    let scale = (area.width() / content.x).min(area.height() / content.y);
    let size = content * scale;
    egui::Align2::CENTER_CENTER.align_size_within_rect(size, area)
}

#[cfg(test)]
mod tests {
    use super::letterbox_rect;
    use eframe::egui::{pos2, vec2, Rect};

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} != {b}");
    }

    #[test]
    fn pillarboxes_wide_area_and_centers() {
        // 4:3 video into a 1000x500 area -> height-limited, 666.7x500, centered.
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 500.0));
        let r = letterbox_rect(vec2(640.0, 480.0), area);
        approx(r.height(), 500.0);
        approx(r.width(), 500.0 * 640.0 / 480.0);
        // Aspect ratio preserved and centered within the area.
        approx(r.width() / r.height(), 640.0 / 480.0);
        approx(r.center().x, area.center().x);
        approx(r.center().y, area.center().y);
        // Fully contained.
        assert!(r.min.x >= area.min.x - 1e-3 && r.max.x <= area.max.x + 1e-3);
    }

    #[test]
    fn letterboxes_tall_area() {
        // 4:3 video into a 400x1000 area -> width-limited, 400x300, centered.
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 1000.0));
        let r = letterbox_rect(vec2(640.0, 480.0), area);
        approx(r.width(), 400.0);
        approx(r.height(), 300.0);
        approx(r.center().y, area.center().y);
        assert!(r.min.y >= area.min.y - 1e-3 && r.max.y <= area.max.y + 1e-3);
    }
}

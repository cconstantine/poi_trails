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
    if !app.show_controls && !app.is_immersive() {
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
                }

                if app.mode == Mode::Trails {
                    ui.separator();
                    ui.label("Trails settings");
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
                        egui::Slider::new(&mut app.trails.fade_seconds, 0.2..=3.0)
                            .text("Trail fade (s)"),
                    )
                    .on_hover_text(
                        "How many seconds a trail takes to fade away. \
                     Higher = longer-lasting streaks.",
                    );
                    ui.add(
                        egui::Slider::new(&mut app.trails.dim_factor, 0.0..=1.0)
                            .text("Background dim"),
                    )
                    .on_hover_text(
                        "How dark the live picture behind the trails is. \
                     Lower = trails stand out more.",
                    );

                    ui.separator();
                    ui.checkbox(&mut app.trails.motion_gate, "Suppress static background")
                        .on_hover_text(
                            "Ignores parts of the scene that stay still, so only moving \
                         lights leave trails.",
                        );
                    caption(
                        ui,
                        "Keeps still-but-bright things (lamps, windows) from trailing — \
                     only movement paints.",
                    );
                    if app.trails.motion_gate {
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
                        if ui
                            .button("Reset background")
                            .on_hover_text(
                                "Re-learn what's part of the still background right now. \
                             Use after the scene or lighting changes.",
                            )
                            .clicked()
                        {
                            app.trails.reset_background();
                        }
                    }

                    ui.separator();
                    if ui
                        .button("Clear trails")
                        .on_hover_text("Erase the trails currently on screen.")
                        .clicked()
                    {
                        app.trails.clear();
                    }
                }
            });
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            draw_video(app, ui);
        });
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
                app.request_camera(None);
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

#[cfg(target_arch = "wasm32")]
fn device_label(device: &crate::video_frame::CameraDevice) -> String {
    if device.label.is_empty() {
        device.device_id.clone()
    } else {
        device.label.clone()
    }
}

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

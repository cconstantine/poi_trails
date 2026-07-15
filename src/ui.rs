use eframe::egui;

use crate::app::{Mode, PoiTrailsApp};

#[cfg(target_arch = "wasm32")]
use crate::camera::CameraStatus;

pub fn draw(app: &mut PoiTrailsApp, ui: &mut egui::Ui) {
    egui::Panel::right("controls")
        .resizable(true)
        .default_size(230.0)
        .show(ui, |ui| {
            ui.heading("Poi Trails");
            ui.separator();

            #[cfg(target_arch = "wasm32")]
            draw_camera_controls(app, ui);
            #[cfg(not(target_arch = "wasm32"))]
            ui.label("Native preview: synthetic test pattern (no camera).");

            ui.separator();
            ui.label("Mode");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.mode, Mode::Live, "Mirror");
                ui.selectable_value(&mut app.mode, Mode::Trails, "Trails");
            });

            ui.checkbox(&mut app.mirror_enabled, "Mirror flip");

            if app.mode == Mode::Trails {
                ui.separator();
                ui.label("Trails settings");
                ui.add(
                    egui::Slider::new(&mut app.trails.threshold, 0.0..=1.0)
                        .text("Brightness threshold"),
                );
                ui.add(
                    egui::Slider::new(&mut app.trails.intensity_gain, 0.1..=2.0)
                        .text("Brightness boost"),
                );
                ui.add(
                    egui::Slider::new(&mut app.trails.fade_seconds, 0.2..=3.0)
                        .text("Trail fade (s)"),
                );
                ui.add(
                    egui::Slider::new(&mut app.trails.dim_factor, 0.0..=1.0)
                        .text("Background dim"),
                );

                ui.separator();
                ui.checkbox(&mut app.trails.motion_gate, "Suppress static background");
                if app.trails.motion_gate {
                    ui.add(
                        egui::Slider::new(&mut app.trails.motion_sensitivity, 0.0..=1.0)
                            .text("Motion sensitivity"),
                    );
                    ui.add(
                        egui::Slider::new(&mut app.trails.background_seconds, 0.5..=15.0)
                            .text("Background adapt (s)"),
                    );
                    if ui.button("Reset background").clicked() {
                        app.trails.reset_background();
                    }
                }

                ui.separator();
                if ui.button("Clear trails").clicked() {
                    app.trails.clear();
                }
            }
        });

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
            if ui.button("Enable Camera").clicked() {
                app.request_camera(None);
            }
        }
        CameraStatus::Requesting => {
            ui.label("Requesting camera access…");
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

    let size = ui.available_size();
    ui.add(egui::Image::new(texture).uv(uv).fit_to_exact_size(size));
}

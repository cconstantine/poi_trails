mod app;
mod delay;
mod trails;
mod ui;
mod video_frame;

#[cfg(target_arch = "wasm32")]
mod camera;
#[cfg(target_arch = "wasm32")]
mod fullscreen;
#[cfg(target_arch = "wasm32")]
mod gpu;
#[cfg(target_arch = "wasm32")]
mod record;

/// The static "Loading…" placeholder from index.html, shown until the app
/// starts (or repurposed as an error notice if it can't).
#[cfg(target_arch = "wasm32")]
fn loading_element() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.get_element_by_id("loading")
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;

    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .expect("no global window")
            .document()
            .expect("no document");
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("missing #the_canvas_id")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("#the_canvas_id is not a canvas element");

        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(app::PoiTrailsApp::new(cc)))),
            )
            .await;

        match result {
            Ok(()) => {
                if let Some(el) = loading_element() {
                    el.remove();
                }
            }
            Err(err) => {
                log::error!("failed to start eframe: {err:?}");
                if let Some(el) = loading_element() {
                    el.set_text_content(Some(
                        "Failed to start — this app needs WebAssembly and WebGL2. \
                         Try a current Chrome, Firefox, or Safari.",
                    ));
                }
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init();
    eframe::run_native(
        "Poi Trails (native preview)",
        eframe::NativeOptions::default(),
        Box::new(|cc| Ok(Box::new(app::PoiTrailsApp::new(cc)))),
    )
}

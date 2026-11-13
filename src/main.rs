// On Windows, link as a GUI-subsystem app so no console window is created alongside the UI
// (and closing that console no longer kills the app). Debug builds keep the console so
// `eprintln!` output stays visible during development.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use gemini_native_client::app::GeminiApp;

fn main() -> eframe::Result<()> {
    let runtime = gemini_native_client::runtime::build().expect("failed to start tokio runtime");
    // Makes `tokio::spawn` usable from the UI thread. The worker, not this thread, polls it.
    let _guard = runtime.enter();

    let icon_data = eframe::icon_data::from_png_bytes(include_bytes!("../assets/512x512.png")).ok();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1000.0, 700.0])
        .with_title("Gemini Native Client");

    if let Some(icon) = icon_data {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Gemini Native Client",
        native_options,
        Box::new(|cc| Box::new(GeminiApp::new(cc))),
    )
}

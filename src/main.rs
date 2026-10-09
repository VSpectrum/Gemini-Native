// On Windows, link as a GUI-subsystem app so no console window is created alongside the UI
// (and closing that console no longer kills the app). Debug builds keep the console so
// `eprintln!` output stays visible during development.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use gemini_native_client::app::GeminiApp;

fn main() -> eframe::Result<()> {
    // The UI thread is blocked inside the winit event loop, so spawned tasks need a worker
    // thread to make progress. `#[tokio::main]` would start one worker *per CPU core*; all we
    // do is await a handful of child processes, so a single worker is plenty.
    // (The blocking pool is left uncapped: on Windows tokio reads child pipes on blocking
    // threads, and those threads are spawned on demand and exit after ~10s idle.)
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("gemini-io")
        .enable_all()
        .build()
        .expect("failed to start tokio runtime");
    // Makes `tokio::spawn` usable from the UI thread.
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

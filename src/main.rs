use eframe::egui;
use gemini_native_client::app::GeminiApp;

#[tokio::main]
async fn main() -> eframe::Result<()> {
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

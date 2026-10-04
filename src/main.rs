use eframe::egui;
use gemini_native_client::app::GeminiApp;

#[tokio::main]
async fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            .with_title("Gemini Native Client"),
        ..Default::default()
    };

    eframe::run_native(
        "Gemini Native Client",
        native_options,
        Box::new(|cc| Box::new(GeminiApp::new(cc))),
    )
}

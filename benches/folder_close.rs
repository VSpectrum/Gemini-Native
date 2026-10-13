use std::time::Instant;
use gemini_native_client::app::{GeminiApp, HistoryEntry};
use gemini_native_client::pane::Pane;

fn main() {
    let mut app = GeminiApp::default();
    let folder_id = app.create_folder("Test Folder".to_string());

    // Create many conversations to simulate large scale
    let num_items = 10_000;

    for i in 0..num_items {
        app.past_conversations.push(HistoryEntry {
            id: i,
            title: format!("Conv {}", i),
            content: String::new(),
            folder_id: Some(folder_id),
            gemini_metadata: None,
        });

        let pane = Pane::new(format!("Pane {}", i), "".to_string()).with_conversation_id(i);
        app.open_pane(pane);
    }

    // Simulate the close_all_folder logic from the original code
    let start = Instant::now();

    let close_all_folder = Some(folder_id);

    // Original Code logic to benchmark
    if let Some(folder_id) = close_all_folder {
        for entry in &app.past_conversations {
            if entry.folder_id == Some(folder_id) {
                if let Some(tile_id) = app.find_open_tile(entry.id) {
                    if let Some(egui_tiles::Tile::Pane(pane)) = app.tree.tiles.get_mut(tile_id) {
                        pane.should_close = true;
                    }
                }
            }
        }
    }

    let duration = start.elapsed();
    println!("Baseline duration: {:?}", duration);
}

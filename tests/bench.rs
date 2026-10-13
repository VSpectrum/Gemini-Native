use gemini_native_client::app::{GeminiApp, HistoryEntry};
use std::time::Instant;

#[test]
fn test_bench_folder_close() {
    let mut app = GeminiApp::default();
    let folder_id = 1;

    app.past_conversations.clear();

    let num_items = 2_000;

    for i in 0..num_items {
        app.new_conversation();
    }

    for conv in &mut app.past_conversations {
        conv.folder_id = Some(folder_id);
    }

    let start = Instant::now();
    let close_all_folder = Some(folder_id);

    // Baseline logic
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
    let duration1 = start.elapsed();
    println!("Baseline duration: {:?}", duration1);

    // Reset panes
    for (_, tile) in app.tree.tiles.iter_mut() {
        if let egui_tiles::Tile::Pane(pane) = tile {
            pane.should_close = false;
        }
    }

    // Optimized Logic 2: just collect target IDs into a Vec and iterate over tiles.
    let start = Instant::now();
    if let Some(folder_id) = close_all_folder {
        let mut target_ids = Vec::new();
        for entry in &app.past_conversations {
            if entry.folder_id == Some(folder_id) {
                target_ids.push(entry.id);
            }
        }

        for (_, tile) in app.tree.tiles.iter_mut() {
            if let egui_tiles::Tile::Pane(pane) = tile {
                if !pane.should_close && target_ids.contains(&pane.conversation_id) {
                    pane.should_close = true;
                }
            }
        }
    }

    let duration2 = start.elapsed();
    println!("Optimized duration (Vec contains): {:?}", duration2);

    // Reset panes
    for (_, tile) in app.tree.tiles.iter_mut() {
        if let egui_tiles::Tile::Pane(pane) = tile {
            pane.should_close = false;
        }
    }

    // Optimized Logic 3: HashSet for lookups.
    let start = Instant::now();
    if let Some(folder_id) = close_all_folder {
        let mut target_ids = std::collections::HashSet::new();
        for entry in &app.past_conversations {
            if entry.folder_id == Some(folder_id) {
                target_ids.insert(entry.id);
            }
        }

        for (_, tile) in app.tree.tiles.iter_mut() {
            if let egui_tiles::Tile::Pane(pane) = tile {
                if !pane.should_close && target_ids.contains(&pane.conversation_id) {
                    pane.should_close = true;
                }
            }
        }
    }

    let duration3 = start.elapsed();
    println!("Optimized duration (HashSet): {:?}", duration3);
}

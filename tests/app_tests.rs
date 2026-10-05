use gemini_native_client::app::GeminiApp;
use gemini_native_client::pane::Pane;

#[test]
fn test_pane_creation() {
    let pane = Pane::new("Test Pane".to_string(), "Initial chat".to_string());

    assert_eq!(pane.title, "Test Pane");
    assert_eq!(pane.chat_messages[0].text, "Initial chat");
    assert!(!pane.is_loading);
    assert_eq!(pane.selected_model, "gemini-flash");
}

#[test]
fn test_render_with_huge_input() {
    let mut app = GeminiApp::default();
    let ctx = egui::Context::default();

    // Set a huge input on the first pane
    for (_, tile) in app.tree.tiles.iter_mut() {
        if let egui_tiles::Tile::Pane(pane) = tile {
            pane.current_input = "line of text\n".repeat(500);
        }
    }

    // Run a frame of GeminiApp
    let _ = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Render the tree
            let mut behavior = gemini_native_client::pane::TreeBehavior;
            app.tree.ui(&mut behavior, ui);
        });
    });
}

#[test]
fn test_scroll_area_with_short_and_huge_input() {
    let ctx = egui::Context::default();
    let mut short_input = "short text".to_string();
    let mut huge_input = "huge text\n".repeat(500);

    let _ = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let available_width = 400.0;
            // Short input: height stays compact (~60px)
            let before_short_y = ui.cursor().top();
            let _ = egui::ScrollArea::vertical()
                .id_source("test_short")
                .max_height(200.0)
                .max_width(available_width)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut short_input)
                            .desired_width(f32::INFINITY)
                            .min_size(egui::vec2(0.0, 60.0)),
                    )
                });
            let diff_short = ui.cursor().top() - before_short_y;
            assert!(diff_short <= 65.0);

            // Huge input: height capped at 200px and scrolls
            let before_huge_y = ui.cursor().top();
            let res_huge = egui::ScrollArea::vertical()
                .id_source("test_huge")
                .max_height(200.0)
                .max_width(available_width)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut huge_input)
                            .desired_width(f32::INFINITY)
                            .min_size(egui::vec2(0.0, 60.0)),
                    )
                });
            let diff_huge = ui.cursor().top() - before_huge_y;
            assert!(diff_huge <= 205.0);
            assert!(res_huge.content_size.y > 500.0);
        });
    });
}

fn pane_count(app: &GeminiApp) -> usize {
    app.tree
        .tiles
        .iter()
        .filter(|(_, t)| matches!(t, egui_tiles::Tile::Pane(_)))
        .count()
}

#[test]
fn test_new_conversation_appears_in_history_immediately() {
    let mut app = GeminiApp::default();
    let before = app.past_conversations.len();

    let tile_id = app.new_conversation();

    assert_eq!(app.past_conversations.len(), before + 1);
    let newest = &app.past_conversations[0];
    assert_eq!(app.find_open_tile(newest.id), Some(tile_id));
}

#[test]
fn test_initial_conversation_is_in_history() {
    let app = GeminiApp::default();
    let newest = &app.past_conversations[0];
    assert_eq!(newest.title, "Conversation 1");
    assert!(app.find_open_tile(newest.id).is_some());
}

#[test]
fn test_clicking_open_conversation_focuses_instead_of_duplicating() {
    let mut app = GeminiApp::default();
    let tile_id = app.new_conversation();
    let id = app.past_conversations[0].id;
    let panes_before = pane_count(&app);

    assert_eq!(app.open_or_focus(id), Some(tile_id));
    assert_eq!(pane_count(&app), panes_before);

    match app.tree.tiles.get(tile_id) {
        Some(egui_tiles::Tile::Pane(p)) => assert!(p.request_focus),
        _ => panic!("expected pane"),
    }
}

#[test]
fn test_closed_conversation_stays_in_history_and_reopens_with_chat() {
    let mut app = GeminiApp::default();
    let tile_id = app.new_conversation();
    let id = app.past_conversations[0].id;

    if let Some(egui_tiles::Tile::Pane(p)) = app.tree.tiles.get_mut(tile_id) {
        p.chat_messages
            .push(gemini_native_client::pane::ChatMessage {
                text: "\n**You:** hello\n".to_string(),
                media: vec![],
            });
        p.should_close = true;
    }
    app.remove_closed_panes();

    assert!(app.find_open_tile(id).is_none());
    let entry = app.past_conversations.iter().find(|e| e.id == id).unwrap();
    assert!(entry.content.contains("hello"));

    let reopened = app.open_or_focus(id).unwrap();
    match app.tree.tiles.get(reopened) {
        Some(egui_tiles::Tile::Pane(p)) => {
            assert_eq!(p.conversation_id, id);
            assert!(p.chat_messages.iter().any(|msg| msg.text.contains("hello")));
        }
        _ => panic!("expected pane"),
    }
}

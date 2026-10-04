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

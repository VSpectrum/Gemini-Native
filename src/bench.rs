#[cfg(test)]
mod tests {
    use crate::app::{GeminiApp, HistoryEntry};
    use std::time::Instant;

    #[test]
    fn bench_folder_close() {
        let mut app = GeminiApp::default();
        let folder_id = 1;

        let num_items = 10_000;

        for i in 0..num_items {
            app.past_conversations.push(HistoryEntry {
                id: i,
                title: format!("Conv {}", i),
                content: String::new(),
                folder_id: Some(folder_id),
                gemini_metadata: None,
            });
            let title = format!("Pane {}", i);
            let id = app.add_history_entry(title.clone(), "".to_string());
            app.new_conversation();
            // Actually it's easier to just push to tiles directly to bypass open pane.
        }
    }
}

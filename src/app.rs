use crate::pane::{Pane, TreeBehavior};
use eframe::egui;
use egui_tiles::{TileId, Tree};
use std::collections::HashSet;

const READY_TEXT: &str = "### Ready\nType a prompt to begin.";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Folder {
    pub id: usize,
    pub name: String,
}

/// A conversation listed in the Session History sidebar.
/// Entries exist for the whole session, whether or not a pane is currently open for them.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct HistoryEntry {
    pub id: usize,
    pub title: String,
    /// Snapshot of the chat. Only refreshed when the pane closes; while a pane is open,
    /// the pane itself is the source of truth.
    pub content: String,
    #[serde(default)]
    pub folder_id: Option<usize>,
}

// 4. Main Application
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct GeminiApp {
    pub tree: Tree<Pane>,
    pub next_pane_id: usize,
    pub next_conversation_id: usize,
    /// Newest first.
    pub past_conversations: Vec<HistoryEntry>,
    #[serde(default)]
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub next_folder_id: usize,
    #[serde(skip)]
    pub active_folder_selection: Option<usize>,
    #[serde(skip)]
    pub renaming_folder_id: Option<usize>,
    #[serde(skip)]
    pub renaming_id: Option<usize>,
}

impl Default for GeminiApp {
    fn default() -> Self {
        let mut app = Self {
            tree: Tree::empty("gemini_tree"),
            next_pane_id: 0,
            next_conversation_id: 0,
            past_conversations: Vec::new(),
            folders: Vec::new(),
            next_folder_id: 0,
            active_folder_selection: None,
            renaming_folder_id: None,
            renaming_id: None,
        };

        let samples = [
            ("Discussing Rust macros", "### Discussing Rust macros\n**You:** How do declarative macros work?\n**Gemini:** ..."),
            ("Python async/await", "### Python async/await\n**You:** Explain asyncio.\n**Gemini:** ..."),
            ("Refactoring UI layout", "### Refactoring UI layout\n**You:** I need to build a split pane UI.\n**Gemini:** ..."),
        ];
        // Insert oldest-last so the list stays in the original display order.
        for (title, content) in samples.iter().rev() {
            app.add_history_entry(title.to_string(), content.to_string());
        }

        app.new_conversation();
        app
    }
}

impl GeminiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        if let Some(storage) = cc.storage {
            if let Some(app) = eframe::get_value::<GeminiApp>(storage, eframe::APP_KEY) {
                return app;
            }
        }
        Self::default()
    }

    /// Adds an entry to the top of the history list and returns its id.
    fn add_history_entry(&mut self, title: String, content: String) -> usize {
        let id = self.next_conversation_id;
        self.next_conversation_id += 1;
        self.past_conversations.insert(
            0,
            HistoryEntry {
                id,
                title,
                content,
                folder_id: None,
            },
        );
        id
    }

    /// Deletes a conversation from history and closes its pane if open.
    pub fn delete_conversation(&mut self, conversation_id: usize) {
        self.past_conversations.retain(|e| e.id != conversation_id);
        if let Some(tile_id) = self.find_open_tile(conversation_id) {
            if let Some(egui_tiles::Tile::Pane(pane)) = self.tree.tiles.get_mut(tile_id) {
                pane.should_close = true;
            }
        }
    }

    /// Creates a new conversation, lists it in history immediately, and opens it in a pane.
    pub fn new_conversation(&mut self) -> TileId {
        self.next_pane_id += 1;
        let title = format!("Conversation {}", self.next_pane_id);
        let id = self.add_history_entry(title.clone(), READY_TEXT.to_string());
        self.open_pane(Pane::new(title, READY_TEXT.to_string()).with_conversation_id(id))
    }

    /// Returns the tile currently showing the given conversation, if any.
    pub fn find_open_tile(&self, conversation_id: usize) -> Option<TileId> {
        self.tree
            .tiles
            .iter()
            .find_map(|(tile_id, tile)| match tile {
                egui_tiles::Tile::Pane(pane)
                    if pane.conversation_id == conversation_id && !pane.should_close =>
                {
                    Some(*tile_id)
                }
                _ => None,
            })
    }

    /// Focuses the conversation's pane if it's open; otherwise reopens it from history.
    pub fn open_or_focus(&mut self, conversation_id: usize) -> Option<TileId> {
        let tile_id = match self.find_open_tile(conversation_id) {
            Some(tile_id) => {
                self.tree.make_active(|id, _| id == tile_id);
                tile_id
            }
            None => {
                let entry = self
                    .past_conversations
                    .iter()
                    .find(|e| e.id == conversation_id)?
                    .clone();
                let mut pane = Pane::new(entry.title, entry.content).with_conversation_id(entry.id);
                for msg in &mut pane.chat_messages {
                    for m in &mut msg.media {
                        if matches!(m.status, crate::pane::MediaStatus::Downloading) {
                            m.status = crate::pane::MediaStatus::NotDownloaded;
                        }
                    }
                }
                self.open_pane(pane)
            }
        };

        if let Some(egui_tiles::Tile::Pane(pane)) = self.tree.tiles.get_mut(tile_id) {
            pane.request_focus = true;
        }
        Some(tile_id)
    }

    /// Inserts a pane into the layout (side-by-side with existing panes) and activates it.
    fn open_pane(&mut self, pane: Pane) -> TileId {
        let tile_id = self.tree.tiles.insert_pane(pane);

        if let Some(root_id) = self.tree.root {
            let mut is_container = false;
            if let Some(egui_tiles::Tile::Container(container)) = self.tree.tiles.get_mut(root_id) {
                container.add_child(tile_id);
                is_container = true;
            }

            if !is_container {
                // The tree simplified the root into a pane, so we recreate the layout container
                let new_root = self
                    .tree
                    .tiles
                    .insert_horizontal_tile(vec![root_id, tile_id]);
                self.tree.root = Some(new_root);
            }
        } else {
            let new_root = self.tree.tiles.insert_horizontal_tile(vec![tile_id]);
            self.tree.root = Some(new_root);
        }

        self.tree.make_active(|id, _| id == tile_id);
        tile_id
    }

    /// Removes panes flagged for closing, saving their chat back into history first.
    pub fn remove_closed_panes(&mut self) {
        let mut closed = Vec::new();
        for (tile_id, tile) in self.tree.tiles.iter() {
            if let egui_tiles::Tile::Pane(pane) = tile {
                if pane.should_close {
                    let history = serde_json::to_string(&pane.chat_messages).unwrap_or_default();
                    closed.push((*tile_id, pane.conversation_id, history));
                }
            }
        }
        for (tile_id, conversation_id, chat) in closed {
            if let Some(entry) = self
                .past_conversations
                .iter_mut()
                .find(|e| e.id == conversation_id)
            {
                entry.content = chat;
            }
            self.tree.remove_recursively(tile_id);
        }
    }

    fn open_conversation_ids(&self) -> HashSet<usize> {
        self.tree
            .tiles
            .iter()
            .filter_map(|(_, tile)| match tile {
                egui_tiles::Tile::Pane(pane) if !pane.should_close => Some(pane.conversation_id),
                _ => None,
            })
            .collect()
    }

    pub fn get_visual_pane_order(&self) -> Vec<usize> {
        let mut ordered_ids = Vec::new();
        if let Some(root_id) = self.tree.root {
            self.traverse_tiles(root_id, &mut ordered_ids);
        }
        ordered_ids
    }

    fn traverse_tiles(&self, tile_id: egui_tiles::TileId, ordered_ids: &mut Vec<usize>) {
        if let Some(tile) = self.tree.tiles.get(tile_id) {
            match tile {
                egui_tiles::Tile::Pane(pane) => {
                    ordered_ids.push(pane.conversation_id);
                }
                egui_tiles::Tile::Container(container) => {
                    for child_id in container.children() {
                        self.traverse_tiles(*child_id, ordered_ids);
                    }
                }
            }
        }
    }

    pub fn sync_history_order(&mut self) {
        let visual_order = self.get_visual_pane_order();
        if visual_order.is_empty() {
            return;
        }

        let mut positions = Vec::new();
        for (i, entry) in self.past_conversations.iter().enumerate() {
            if visual_order.contains(&entry.id) {
                positions.push(i);
            }
        }

        let mut present_visual_order = Vec::new();
        for id in &visual_order {
            if self.past_conversations.iter().any(|e| e.id == *id) {
                present_visual_order.push(*id);
            }
        }

        if positions.len() == present_visual_order.len() {
            let mut extracted = Vec::new();
            for id in present_visual_order {
                if let Some(entry) = self.past_conversations.iter().find(|e| e.id == id) {
                    extracted.push(entry.clone());
                }
            }

            for (i, pos) in positions.into_iter().enumerate() {
                self.past_conversations[pos] = extracted[i].clone();
            }
        }
    }
}

impl eframe::App for GeminiApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let mut open_updates = Vec::new();
        for (_, tile) in self.tree.tiles.iter() {
            if let egui_tiles::Tile::Pane(pane) = tile {
                let history = serde_json::to_string(&pane.chat_messages).unwrap_or_default();
                open_updates.push((pane.conversation_id, history));
            }
        }
        for (conversation_id, chat) in open_updates {
            if let Some(entry) = self
                .past_conversations
                .iter_mut()
                .find(|e| e.id == conversation_id)
            {
                entry.content = chat;
            }
        }
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_visuals(egui::Visuals::dark());

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("➕ New Conversation Pane").clicked() {
                    self.new_conversation();
                }

                ui.separator();
                ui.label("Zoom:");
                let current_zoom = ctx.zoom_factor();
                if ui.button("➖").clicked() {
                    ctx.set_zoom_factor((current_zoom - 0.1).max(0.5));
                }
                ui.label(format!("{:.0}%", current_zoom * 100.0));
                if ui.button("➕").clicked() {
                    ctx.set_zoom_factor((current_zoom + 0.1).min(3.0));
                }
                if current_zoom != 1.0 {
                    if ui.button("Reset").clicked() {
                        ctx.set_zoom_factor(1.0);
                    }
                }
            });
        });

        egui::SidePanel::left("global_history_panel")
            .resizable(true)
            .default_width(250.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Session History");
                    if ui.button("➕ Folder").clicked() {
                        self.folders.push(Folder {
                            id: self.next_folder_id,
                            name: format!("Folder {}", self.next_folder_id + 1),
                        });
                        self.next_folder_id += 1;
                    }
                });
                ui.separator();

                let open_ids = self.open_conversation_ids();
                let mut clicked = None;
                let mut deleted = None;
                let renaming_id = self.renaming_id;
                let mut new_renaming_id = renaming_id;
                let mut rename_finished = None;

                let mut toggle_folder = None;
                let mut folder_clicked = None;
                let mut new_renaming_folder_id = self.renaming_folder_id;
                let mut folder_rename_finished = None;
                let mut folder_deleted = None;
                let mut open_all_folder = None;
                let mut close_all_folder = None;

                egui::ScrollArea::vertical().show(ui, |ui| {
                    // 1. Render Folders and their conversations
                    for folder in &mut self.folders {
                        ui.horizontal(|ui| {
                            let row_height = 24.0;
                            ui.set_min_height(row_height);
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_sized([24.0, row_height], egui::Button::new("🗑"))
                                        .on_hover_text("Delete Folder")
                                        .clicked()
                                    {
                                        folder_deleted = Some(folder.id);
                                    }

                                    if self.renaming_folder_id == Some(folder.id) {
                                        let response = ui.add_sized(
                                            [ui.available_width(), row_height],
                                            egui::TextEdit::singleline(&mut folder.name),
                                        );
                                        if response.lost_focus()
                                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                        {
                                            folder_rename_finished = Some(folder.id);
                                        } else {
                                            response.request_focus();
                                        }
                                    } else {
                                        if ui
                                            .add_sized([24.0, row_height], egui::Button::new("✏️"))
                                            .on_hover_text("Rename Folder")
                                            .clicked()
                                        {
                                            new_renaming_folder_id = Some(folder.id);
                                        }
                                        if ui
                                            .add_sized([24.0, row_height], egui::Button::new("📖"))
                                            .on_hover_text("Open All in Folder")
                                            .clicked()
                                        {
                                            open_all_folder = Some(folder.id);
                                        }
                                        if ui
                                            .add_sized([24.0, row_height], egui::Button::new("📕"))
                                            .on_hover_text("Close All in Folder")
                                            .clicked()
                                        {
                                            close_all_folder = Some(folder.id);
                                        }

                                        let is_active =
                                            self.active_folder_selection == Some(folder.id);
                                        let text = if is_active {
                                            format!("📂 {}", folder.name)
                                        } else {
                                            format!("📁 {}", folder.name)
                                        };
                                        if ui
                                            .add_sized(
                                                [ui.available_width(), row_height],
                                                egui::SelectableLabel::new(is_active, text),
                                            )
                                            .clicked()
                                        {
                                            folder_clicked = Some(folder.id);
                                        }
                                    }
                                },
                            );
                        });

                        for entry in &mut self.past_conversations {
                            if entry.folder_id == Some(folder.id) {
                                ui.horizontal(|ui| {
                                    let row_height = 24.0;
                                    ui.set_min_height(row_height);
                                    ui.add_space(20.0); // Indent

                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            if let Some(active_folder) =
                                                self.active_folder_selection
                                            {
                                                let mut in_folder = true;
                                                if ui
                                                    .add_sized(
                                                        [24.0, row_height],
                                                        egui::Checkbox::new(&mut in_folder, ""),
                                                    )
                                                    .clicked()
                                                {
                                                    toggle_folder = Some((
                                                        entry.id,
                                                        if in_folder {
                                                            Some(active_folder)
                                                        } else {
                                                            None
                                                        },
                                                    ));
                                                }
                                            }

                                            if ui
                                                .add_sized(
                                                    [24.0, row_height],
                                                    egui::Button::new("🗑"),
                                                )
                                                .on_hover_text("Delete Conversation")
                                                .clicked()
                                            {
                                                deleted = Some(entry.id);
                                            }

                                            if renaming_id == Some(entry.id) {
                                                let response = ui.add_sized(
                                                    [ui.available_width(), row_height],
                                                    egui::TextEdit::singleline(&mut entry.title),
                                                );
                                                if response.lost_focus()
                                                    || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                                {
                                                    rename_finished =
                                                        Some((entry.id, entry.title.clone()));
                                                } else {
                                                    response.request_focus();
                                                }
                                            } else {
                                                if ui
                                                    .add_sized(
                                                        [24.0, row_height],
                                                        egui::Button::new("✏️"),
                                                    )
                                                    .on_hover_text("Rename Conversation")
                                                    .clicked()
                                                {
                                                    new_renaming_id = Some(entry.id);
                                                }

                                                let is_open = open_ids.contains(&entry.id);
                                                let hover = if is_open {
                                                    "Open — click to focus"
                                                } else {
                                                    "Click to reopen"
                                                };

                                                if self.active_folder_selection.is_some() {
                                                    if ui
                                                        .add_sized(
                                                            [ui.available_width(), row_height],
                                                            egui::SelectableLabel::new(
                                                                is_open,
                                                                &entry.title,
                                                            ),
                                                        )
                                                        .on_hover_text(
                                                            "Click to toggle folder membership",
                                                        )
                                                        .clicked()
                                                    {
                                                        toggle_folder = Some((entry.id, None));
                                                    }
                                                } else {
                                                    if ui
                                                        .add_sized(
                                                            [ui.available_width(), row_height],
                                                            egui::SelectableLabel::new(
                                                                is_open,
                                                                &entry.title,
                                                            ),
                                                        )
                                                        .on_hover_text(hover)
                                                        .clicked()
                                                    {
                                                        clicked = Some(entry.id);
                                                    }
                                                }
                                            }
                                        },
                                    );
                                });
                            }
                        }
                    }

                    // 2. Render unassigned conversations
                    ui.add_space(10.0);
                    if !self.folders.is_empty() {
                        ui.label("Conversations:");
                    }

                    for entry in &mut self.past_conversations {
                        if entry.folder_id.is_none() {
                            ui.horizontal(|ui| {
                                let row_height = 24.0;
                                ui.set_min_height(row_height);
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        if let Some(active_folder) = self.active_folder_selection {
                                            let mut in_folder = false;
                                            if ui
                                                .add_sized(
                                                    [24.0, row_height],
                                                    egui::Checkbox::new(&mut in_folder, ""),
                                                )
                                                .clicked()
                                            {
                                                toggle_folder = Some((
                                                    entry.id,
                                                    if in_folder {
                                                        Some(active_folder)
                                                    } else {
                                                        None
                                                    },
                                                ));
                                            }
                                        }

                                        if ui
                                            .add_sized([24.0, row_height], egui::Button::new("🗑"))
                                            .on_hover_text("Delete Conversation")
                                            .clicked()
                                        {
                                            deleted = Some(entry.id);
                                        }

                                        if renaming_id == Some(entry.id) {
                                            let response = ui.add_sized(
                                                [ui.available_width(), row_height],
                                                egui::TextEdit::singleline(&mut entry.title),
                                            );
                                            if response.lost_focus()
                                                || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            {
                                                rename_finished =
                                                    Some((entry.id, entry.title.clone()));
                                            } else {
                                                response.request_focus();
                                            }
                                        } else {
                                            if ui
                                                .add_sized(
                                                    [24.0, row_height],
                                                    egui::Button::new("✏️"),
                                                )
                                                .on_hover_text("Rename Conversation")
                                                .clicked()
                                            {
                                                new_renaming_id = Some(entry.id);
                                            }

                                            let is_open = open_ids.contains(&entry.id);
                                            let hover = if is_open {
                                                "Open — click to focus"
                                            } else {
                                                "Click to reopen"
                                            };

                                            if self.active_folder_selection.is_some() {
                                                if ui
                                                    .add_sized(
                                                        [ui.available_width(), row_height],
                                                        egui::SelectableLabel::new(
                                                            is_open,
                                                            &entry.title,
                                                        ),
                                                    )
                                                    .on_hover_text(
                                                        "Click to toggle folder membership",
                                                    )
                                                    .clicked()
                                                {
                                                    toggle_folder = Some((
                                                        entry.id,
                                                        Some(self.active_folder_selection.unwrap()),
                                                    ));
                                                }
                                            } else {
                                                if ui
                                                    .add_sized(
                                                        [ui.available_width(), row_height],
                                                        egui::SelectableLabel::new(
                                                            is_open,
                                                            &entry.title,
                                                        ),
                                                    )
                                                    .on_hover_text(hover)
                                                    .clicked()
                                                {
                                                    clicked = Some(entry.id);
                                                }
                                            }
                                        }
                                    },
                                );
                            });
                        }
                    }
                });

                self.renaming_id = new_renaming_id;
                self.renaming_folder_id = new_renaming_folder_id;

                if let Some(folder_id) = folder_clicked {
                    if self.active_folder_selection == Some(folder_id) {
                        self.active_folder_selection = None;
                    } else {
                        self.active_folder_selection = Some(folder_id);
                    }
                }

                if let Some(folder_id) = open_all_folder {
                    let mut ids_to_open = Vec::new();
                    for entry in &self.past_conversations {
                        if entry.folder_id == Some(folder_id) {
                            ids_to_open.push(entry.id);
                        }
                    }
                    for id in ids_to_open {
                        self.open_or_focus(id);
                    }
                }

                if let Some(folder_id) = close_all_folder {
                    for entry in &self.past_conversations {
                        if entry.folder_id == Some(folder_id) {
                            if let Some(tile_id) = self.find_open_tile(entry.id) {
                                if let Some(egui_tiles::Tile::Pane(pane)) =
                                    self.tree.tiles.get_mut(tile_id)
                                {
                                    pane.should_close = true;
                                }
                            }
                        }
                    }
                }

                if let Some(_) = folder_rename_finished {
                    self.renaming_folder_id = None;
                }

                if let Some(folder_id) = folder_deleted {
                    self.folders.retain(|f| f.id != folder_id);
                    for entry in &mut self.past_conversations {
                        if entry.folder_id == Some(folder_id) {
                            entry.folder_id = None;
                        }
                    }
                    if self.active_folder_selection == Some(folder_id) {
                        self.active_folder_selection = None;
                    }
                }

                if let Some((id, new_folder_id)) = toggle_folder {
                    if let Some(entry) = self.past_conversations.iter_mut().find(|e| e.id == id) {
                        entry.folder_id = new_folder_id;
                    }
                }

                if let Some((id, new_title)) = rename_finished {
                    self.renaming_id = None;
                    if let Some(tile_id) = self.find_open_tile(id) {
                        if let Some(egui_tiles::Tile::Pane(pane)) = self.tree.tiles.get_mut(tile_id)
                        {
                            pane.title = new_title;
                        }
                    }
                }

                if let Some(id) = clicked {
                    self.open_or_focus(id);
                }

                if let Some(id) = deleted {
                    self.delete_conversation(id);
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut behavior = TreeBehavior;
            self.tree.ui(&mut behavior, ui);
        });

        // --- Process custom drag-and-drop pane swap ---
        let mut swap_request = None;
        for (_, tile) in self.tree.tiles.iter() {
            if let egui_tiles::Tile::Pane(pane) = tile {
                if let Some((id1, id2)) = pane.request_swap {
                    swap_request = Some((id1, id2));
                    break;
                }
            }
        }

        if let Some((id1, id2)) = swap_request {
            // Clear the flag from all panes
            for (_, tile) in self.tree.tiles.iter_mut() {
                if let egui_tiles::Tile::Pane(pane) = tile {
                    pane.request_swap = None;
                }
            }

            if let (Some(t1), Some(t2)) = (self.find_open_tile(id1), self.find_open_tile(id2)) {
                if t1 != t2 {
                    // Temporarily extract both panes
                    let mut pane1 = None;
                    if let Some(egui_tiles::Tile::Pane(p)) = self.tree.tiles.get_mut(t1) {
                        pane1 = Some(std::mem::replace(p, Pane::new("".into(), "".into())));
                    }

                    let mut pane2 = None;
                    if let Some(egui_tiles::Tile::Pane(p)) = self.tree.tiles.get_mut(t2) {
                        pane2 = Some(std::mem::replace(p, Pane::new("".into(), "".into())));
                    }

                    // Swap them back into the opposing tiles!
                    if let (Some(p1), Some(p2)) = (pane1, pane2) {
                        if let Some(egui_tiles::Tile::Pane(p)) = self.tree.tiles.get_mut(t1) {
                            *p = p2;
                        }
                        if let Some(egui_tiles::Tile::Pane(p)) = self.tree.tiles.get_mut(t2) {
                            *p = p1;
                        }
                    }
                }
            }
        }

        self.sync_history_order();
        self.remove_closed_panes();
    }
}

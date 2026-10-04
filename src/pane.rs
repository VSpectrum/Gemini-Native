use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_tiles::{Behavior, TileId};
use serde::{Deserialize, Serialize};
use std::sync::mpsc;

fn resolve_executable(name: &str) -> (String, Vec<String>) {
    let mut exe_name = name.to_string();
    if cfg!(windows) {
        exe_name.push_str(".exe");
    }

    if let Ok(mut current_exe) = std::env::current_exe() {
        current_exe.pop();
        let bundled_exe = current_exe.join(&exe_name);
        if bundled_exe.exists() {
            return (bundled_exe.to_string_lossy().to_string(), vec![]);
        }
    }
    
    // Fallback to running python3 for local development
    ("python3".to_string(), vec![format!("{}.py", name)])
}

// 1. JSON Payload Structure
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct MediaItem {
    #[serde(rename = "type")]
    pub media_type: String,
    pub meta: serde_json::Value,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PythonResponse {
    pub text: String,
    pub quota: String,
    pub abuse: String,
    #[serde(default)]
    pub media: Vec<MediaItem>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub enum MediaStatus {
    NotDownloaded,
    Downloading,
    Downloaded(String),
    Failed(String),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PaneMedia {
    pub item: MediaItem,
    pub status: MediaStatus,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub text: String,
    #[serde(default)]
    pub media: Vec<PaneMedia>,
}

pub enum PaneEvent {
    ChatResponse(PythonResponse),
    ChatError(String),
    MediaDownloaded {
        msg_index: usize,
        media_index: usize,
        result: Result<String, String>,
    },
}

pub struct ChannelPair {
    pub tx: mpsc::Sender<PaneEvent>,
    pub rx: mpsc::Receiver<PaneEvent>,
}

impl Default for ChannelPair {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx }
    }
}

// 2. Pane State
#[derive(serde::Deserialize, serde::Serialize)]
pub struct Pane {
    pub title: String,
    pub chat_messages: Vec<ChatMessage>,
    pub current_input: String,
    #[serde(skip)]
    pub md_cache: CommonMarkCache,
    #[serde(skip)]
    pub is_loading: bool,
    pub quota_text: String,
    pub abuse_text: String,
    pub selected_model: String,
    #[serde(skip)]
    pub channel: ChannelPair,
    pub local_zoom: f32,
    #[serde(skip)]
    pub should_close: bool,
    pub conversation_id: usize,
    #[serde(skip)]
    pub request_focus: bool,
    #[serde(skip)]
    pub request_swap: Option<(usize, usize)>,
}

impl Pane {
    pub fn with_conversation_id(mut self, conversation_id: usize) -> Self {
        self.conversation_id = conversation_id;
        self
    }

    pub fn new(title: String, initial_chat: String) -> Self {
        let chat_messages =
            if let Ok(parsed) = serde_json::from_str::<Vec<ChatMessage>>(&initial_chat) {
                parsed
            } else {
                vec![ChatMessage {
                    text: initial_chat,
                    media: vec![],
                }]
            };

        Self {
            title,
            chat_messages,
            current_input: String::new(),
            md_cache: CommonMarkCache::default(),
            is_loading: false,
            quota_text: "Quota: Ready".to_string(),
            abuse_text: "Abuse: Ready".to_string(),
            selected_model: "gemini-flash".to_string(),
            channel: ChannelPair::default(),
            local_zoom: 1.0,
            should_close: false,
            conversation_id: 0,
            request_focus: false,
            request_swap: None,
        }
    }
}

// 3. UI and Interaction Behavior
pub struct TreeBehavior;

impl Behavior<Pane> for TreeBehavior {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        tile_id: TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        // --- 1. Process Incoming Data ---
        while let Ok(event) = pane.channel.rx.try_recv() {
            match event {
                PaneEvent::ChatResponse(data) => {
                    pane.chat_messages.push(ChatMessage {
                        text: format!("\n**Gemini:**\n{}\n", data.text),
                        media: data
                            .media
                            .into_iter()
                            .map(|item| PaneMedia {
                                item,
                                status: MediaStatus::NotDownloaded,
                            })
                            .collect(),
                    });
                    pane.quota_text = data.quota;
                    pane.abuse_text = data.abuse;
                    pane.is_loading = false;
                }
                PaneEvent::ChatError(err) => {
                    pane.chat_messages.push(ChatMessage {
                        text: format!("\n**System Error:**\n{}\n", err),
                        media: vec![],
                    });
                    pane.is_loading = false;
                }
                PaneEvent::MediaDownloaded {
                    msg_index,
                    media_index,
                    result,
                } => {
                    if let Some(msg) = pane.chat_messages.get_mut(msg_index) {
                        if let Some(media) = msg.media.get_mut(media_index) {
                            match result {
                                Ok(path) => media.status = MediaStatus::Downloaded(path),
                                Err(e) => media.status = MediaStatus::Failed(e),
                            }
                        }
                    }
                }
            }
        }

        // --- 3. Central Panel for Input and Status ---
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let pane_rect = ui.max_rect();

            if egui::DragAndDrop::has_payload_of_type::<usize>(ui.ctx()) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }

            if let Some(dragged_id) = egui::DragAndDrop::payload::<usize>(ui.ctx()).map(|p| *p.as_ref()) {
                if dragged_id != pane.conversation_id && ui.rect_contains_pointer(pane_rect) {
                    ui.painter().rect_stroke(
                        pane_rect.shrink(1.0),
                        2.0,
                        egui::Stroke::new(3.0_f32, egui::Color32::from_rgb(100, 200, 255)),
                    );
                    if ui.input(|i| i.pointer.any_released()) {
                        pane.request_swap = Some((dragged_id, pane.conversation_id));
                        egui::DragAndDrop::clear_payload(ui.ctx());
                    }
                }
            }

            ui.horizontal(|ui| {
                let title_response = ui.add(
                    egui::Label::new(egui::RichText::new(&pane.title).heading())
                        .selectable(false)
                        .sense(egui::Sense::drag())
                ).on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("Drag over another pane to swap");

                if title_response.drag_started() {
                    egui::DragAndDrop::set_payload(ui.ctx(), pane.conversation_id);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("❌").on_hover_text("Close Pane").clicked() {
                        pane.should_close = true;
                    }
                });
            });
            ui.separator();

            // Bottom Panel for Input and Status
            egui::TopBottomPanel::bottom(ui.id().with("pane_bottom")).show_inside(ui, |ui| {
                ui.vertical(|ui| {
                    ui.separator();

                    // Input Area
                    ui.horizontal(|ui| {
                        let available_width = ui.available_width() - 60.0;
                        let mut send_pressed = false;

                        ui.add_enabled_ui(!pane.is_loading, |ui| {
                            let text_res = ui.add_sized(
                                [available_width, 60.0],
                                egui::TextEdit::multiline(&mut pane.current_input)
                                    .hint_text("Type your prompt... (Enter to send, Shift+Enter for newline)")
                            );

                            if pane.request_focus {
                                text_res.request_focus();
                                pane.request_focus = false;
                            }

                            if ui.button("Send").clicked() {
                                send_pressed = true;
                            }

                            if text_res.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) {
                                send_pressed = true;
                                pane.current_input = pane.current_input.trim_end().to_string();
                            }
                        });

                        if send_pressed && !pane.current_input.trim().is_empty() {
                            let prompt = pane.current_input.clone();
                            pane.chat_messages.push(ChatMessage {
                                text: format!("\n**You:** {}\n", prompt),
                                media: vec![],
                            });
                            pane.current_input.clear();
                            pane.is_loading = true;

                            let tx = pane.channel.tx.clone();
                            let ctx = ui.ctx().clone();
                            let selected_model = pane.selected_model.clone();

                            tokio::spawn(async move {
                                let (program, args) = resolve_executable("gemini_auto");
                                let mut cmd = tokio::process::Command::new(program);
                                cmd.args(&args);
                                cmd.arg(&prompt);
                                cmd.arg(&selected_model);
                                let output = cmd.output().await;

                                match output {
                                    Ok(out) => {
                                        let response = String::from_utf8_lossy(&out.stdout).to_string();
                                        let json_line = response.lines().last().unwrap_or("").to_string();
                                        if let Ok(data) = serde_json::from_str::<PythonResponse>(&json_line) {
                                            let _ = tx.send(PaneEvent::ChatResponse(data));
                                        } else {
                                            let _ = tx.send(PaneEvent::ChatError(format!("Invalid JSON: {}", json_line)));
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(PaneEvent::ChatError(format!("Failed to launch Python: {}", e)));
                                    }
                                }

                                ctx.request_repaint();
                            });
                        }
                    });

                    // Status Bar / Quota
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Status:");
                        let is_ok = !pane.quota_text.to_lowercase().contains("error");
                        let status_color = if is_ok { egui::Color32::GREEN } else { egui::Color32::RED };

                        let response = ui.add(egui::Label::new(
                            egui::RichText::new("●").color(status_color)
                        ).sense(egui::Sense::click()));

                        let popup_id = ui.make_persistent_id(format!("status_popup_{:?}", tile_id));
                        if response.clicked() {
                            ui.memory_mut(|mem| mem.toggle_popup(popup_id));
                        }

                        egui::popup::popup_below_widget(ui, popup_id, &response, |ui| {
                            ui.set_min_width(300.0);
                            ui.label("Tokens / Quota:");
                            ui.label(egui::RichText::new(&pane.quota_text).size(11.0).color(egui::Color32::DARK_GRAY));
                            ui.separator();
                            ui.label("Abuse Info:");
                            ui.label(egui::RichText::new(&pane.abuse_text).size(11.0).color(egui::Color32::DARK_GRAY));
                        });

                        ui.label(" | Text: ");
                        if ui.button("➖").clicked() {
                            pane.local_zoom = (pane.local_zoom - 0.1).max(0.5);
                        }
                        if ui.button("➕").clicked() {
                            pane.local_zoom = (pane.local_zoom + 0.1).min(3.0);
                        }

                        ui.label(" | Model: ");

                        egui::ComboBox::from_id_source(format!("model_selector_{:?}", tile_id))
                            .selected_text(pane.selected_model.clone())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut pane.selected_model, "gemini-flash".to_string(), "3.8 Flash");
                                ui.selectable_value(&mut pane.selected_model, "gemini-flash-lite".to_string(), "3.5 Flash-Lite");
                                ui.selectable_value(&mut pane.selected_model, "gemini-pro".to_string(), "3.1 Pro");
                            });
                    });
                });
            });

            // Conversation Response in Main Window Pane
            egui::CentralPanel::default().show_inside(ui, |ui| {
                ui.scope(|ui| {
                    if pane.local_zoom != 1.0 {
                        for (_text_style, font_id) in ui.style_mut().text_styles.iter_mut() {
                            font_id.size *= pane.local_zoom;
                        }
                    }

                    #[derive(Debug, PartialEq)]
                    enum MdBlock {
                        Normal(String),
                        Scrollable(String),
                    }

                    fn split_markdown(text: &str) -> Vec<MdBlock> {
                        let mut blocks = Vec::new();
                        let mut current_normal = String::new();
                        let mut lines = text.lines().peekable();

                        while let Some(line) = lines.next() {
                            if line.trim_start().starts_with("```") {
                                if !current_normal.trim().is_empty() {
                                    blocks.push(MdBlock::Normal(current_normal.clone()));
                                    current_normal.clear();
                                }
                                let mut code_block = line.to_string() + "\n";
                                for code_line in lines.by_ref() {
                                    code_block.push_str(code_line);
                                    code_block.push('\n');
                                    if code_line.trim_start().starts_with("```") {
                                        break;
                                    }
                                }
                                blocks.push(MdBlock::Scrollable(code_block));
                            } else if line.trim_start().starts_with('|') && line.trim_end().ends_with('|') {
                                if !current_normal.trim().is_empty() {
                                    blocks.push(MdBlock::Normal(current_normal.clone()));
                                    current_normal.clear();
                                }
                                let mut table_block = line.to_string() + "\n";
                                while let Some(table_line) = lines.peek() {
                                    if table_line.trim_start().starts_with('|') && table_line.trim_end().ends_with('|') {
                                        table_block.push_str(table_line);
                                        table_block.push('\n');
                                        lines.next();
                                    } else {
                                        break;
                                    }
                                }
                                blocks.push(MdBlock::Scrollable(table_block));
                            } else {
                                current_normal.push_str(line);
                                current_normal.push('\n');
                            }
                        }

                        if !current_normal.trim().is_empty() {
                            blocks.push(MdBlock::Normal(current_normal));
                        }

                        blocks
                    }

                    let available_width = ui.available_width();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (msg_idx, msg) in pane.chat_messages.iter_mut().enumerate() {
                                let blocks = split_markdown(&msg.text);
                                for (block_idx, block) in blocks.into_iter().enumerate() {
                                    match block {
                                        MdBlock::Normal(text) => {
                                            ui.horizontal(|ui| {
                                                ui.set_max_width(available_width);
                                                ui.style_mut().wrap = Some(true);
                                                CommonMarkViewer::new(format!("viewer_{:?}_{}_{}_normal", tile_id, msg_idx, block_idx))
                                                    .show(ui, &mut pane.md_cache, &text);
                                            });
                                        }
                                        MdBlock::Scrollable(text) => {
                                            egui::ScrollArea::horizontal()
                                                .id_source(format!("scroll_{:?}_{}_{}", tile_id, msg_idx, block_idx))
                                                .auto_shrink([false, true])
                                                .show(ui, |ui| {
                                                    CommonMarkViewer::new(format!("viewer_{:?}_{}_{}_scroll", tile_id, msg_idx, block_idx))
                                                        .show(ui, &mut pane.md_cache, &text);
                                                });
                                        }
                                    }
                                }

                                if !msg.media.is_empty() {
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        for (media_idx, media) in msg.media.iter_mut().enumerate() {
                                            match &media.status {
                                                MediaStatus::NotDownloaded => {
                                                    let label = if media.item.media_type.contains("video") {
                                                        "🎬 Download Video"
                                                    } else {
                                                        "🖼️ Download Image"
                                                    };
                                                    if ui.button(label).clicked() {
                                                        media.status = MediaStatus::Downloading;
                                                        let tx = pane.channel.tx.clone();
                                                        let ctx = ui.ctx().clone();
                                                        let json_item = serde_json::to_string(&media.item).unwrap_or_default();
                                                        tokio::spawn(async move {
                                                            let (program, args) = resolve_executable("gemini_download");
                                                            let mut cmd = tokio::process::Command::new(program);
                                                            cmd.args(&args);
                                                            cmd.arg(&json_item);
                                                            let output = cmd.output().await;

                                                            let res = match output {
                                                                Ok(out) => {
                                                                    let out_str = String::from_utf8_lossy(&out.stdout).to_string();
                                                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&out_str) {
                                                                        if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
                                                                            Ok(path.to_string())
                                                                        } else {
                                                                            Err(json.get("error").and_then(|v| v.as_str()).unwrap_or("Unknown error").to_string())
                                                                        }
                                                                    } else {
                                                                        Err("Failed to parse output".to_string())
                                                                    }
                                                                },
                                                                Err(e) => Err(e.to_string()),
                                                            };

                                                            let _ = tx.send(PaneEvent::MediaDownloaded { msg_index: msg_idx, media_index: media_idx, result: res });
                                                            ctx.request_repaint();
                                                        });
                                                    }
                                                },
                                                MediaStatus::Downloading => {
                                                    ui.spinner();
                                                    ui.label("Downloading...");
                                                },
                                                MediaStatus::Failed(err) => {
                                                    ui.colored_label(egui::Color32::RED, format!("Failed to download: {}", err));
                                                },
                                                MediaStatus::Downloaded(path) => {
                                                    if media.item.media_type.contains("video") {
                                                        if ui.button("🎬 Open Video").clicked() {
                                                            let _ = std::process::Command::new("open").arg(path).spawn();
                                                        }
                                                    } else {
                                                        ui.vertical(|ui| {
                                                            let max_size = egui::vec2(ui.available_width().min(600.0), 300.0);
                                                            ui.add(
                                                                egui::Image::new(format!("file://{}", path))
                                                                    .max_size(max_size)
                                                                    .maintain_aspect_ratio(true)
                                                            );
                                                            if ui.button("↗ Open in System").clicked() {
                                                                let _ = std::process::Command::new("open").arg(path).spawn();
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                            ui.add_space(8.0);
                                        }
                                    });
                                }
                            }

                            if pane.is_loading {
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(" Thinking (or checking cookies)...");
                                });
                            }
                        });
                });
            });
        });

        egui_tiles::UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title.clone().into()
    }
}

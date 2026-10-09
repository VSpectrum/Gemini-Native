use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_tiles::{Behavior, TileId};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;
use std::sync::mpsc;

fn is_safe_path(path_str: &str) -> bool {
    let path = Path::new(path_str);

    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();

    if home_dir.is_empty() {
        return false;
    }

    let expected_dir = Path::new(&home_dir).join(".gemini_local").join("media");

    let canonical_path = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let canonical_expected = match std::fs::canonicalize(&expected_dir) {
        Ok(p) => p,
        Err(_) => return false,
    };

    canonical_path.starts_with(canonical_expected)
}

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

/// Builds the command for a bundled helper (or its `.py` fallback).
fn helper_command(name: &str) -> tokio::process::Command {
    let (program, args) = resolve_executable(name);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&args);
    #[cfg(windows)]
    {
        // The helpers are console executables. Now that the UI runs without a console, Windows
        // would otherwise pop up a fresh console window for every request.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
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
    pub metadata: Option<serde_json::Value>,
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

/// Per-message layout cache. Messages are append-only, so entries are keyed by index.
#[derive(Default)]
pub struct MsgRenderCache {
    /// `split_markdown_ranges` output, plus the text length it was computed for.
    blocks: Option<(usize, Vec<(BlockKind, Range<usize>)>)>,
    /// Height from the last time the message was actually laid out, and the inputs it depends on.
    height: Option<f32>,
    width: f32,
    zoom: f32,
}

// 2. Pane State
#[derive(serde::Deserialize, serde::Serialize)]
pub struct Pane {
    pub title: String,
    pub chat_messages: Vec<ChatMessage>,
    pub current_input: String,
    #[serde(skip)]
    pub render_cache: Vec<MsgRenderCache>,
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
    #[serde(default)]
    pub gemini_metadata: Option<serde_json::Value>,
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
            render_cache: Vec::new(),
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
            gemini_metadata: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum MdBlock {
    Normal(String),
    Scrollable(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlockKind {
    Normal,
    Scrollable,
}

fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|') && line.trim_end().ends_with('|')
}

/// Splits markdown into normal / horizontally-scrollable (code fences, tables) blocks.
/// Returns byte ranges into `text` so callers can cache the result without copying the text.
pub fn split_markdown_ranges(text: &str) -> Vec<(BlockKind, Range<usize>)> {
    let mut blocks = Vec::new();
    let mut normal_start: Option<usize> = None;

    let flush_normal = |blocks: &mut Vec<(BlockKind, Range<usize>)>, start: &mut Option<usize>, end: usize| {
        if let Some(s) = start.take() {
            if !text[s..end].trim().is_empty() {
                blocks.push((BlockKind::Normal, s..end));
            }
        }
    };

    // (start, end, line-without-terminator) for each line, mirroring `str::lines()`.
    let mut lines = text
        .split_inclusive('\n')
        .scan(0usize, |offset, raw| {
            let start = *offset;
            *offset += raw.len();
            Some((start, *offset, raw.trim_end_matches(['\n', '\r'])))
        })
        .peekable();

    while let Some((start, end, line)) = lines.next() {
        if line.trim_start().starts_with("```") {
            flush_normal(&mut blocks, &mut normal_start, start);
            let mut block_end = end;
            for (_, code_end, code_line) in lines.by_ref() {
                block_end = code_end;
                if code_line.trim_start().starts_with("```") {
                    break;
                }
            }
            blocks.push((BlockKind::Scrollable, start..block_end));
        } else if is_table_line(line) {
            flush_normal(&mut blocks, &mut normal_start, start);
            let mut block_end = end;
            while let Some(&(_, table_end, table_line)) = lines.peek() {
                if !is_table_line(table_line) {
                    break;
                }
                block_end = table_end;
                lines.next();
            }
            blocks.push((BlockKind::Scrollable, start..block_end));
        } else if normal_start.is_none() {
            normal_start = Some(start);
        }
    }
    flush_normal(&mut blocks, &mut normal_start, text.len());

    blocks
}

/// Owned-string variant of [`split_markdown_ranges`] (each block normalised to end in `\n`).
pub fn split_markdown(text: &str) -> Vec<MdBlock> {
    split_markdown_ranges(text)
        .into_iter()
        .map(|(kind, range)| {
            let mut s = text[range].replace("\r\n", "\n");
            if !s.ends_with('\n') {
                s.push('\n');
            }
            match kind {
                BlockKind::Normal => MdBlock::Normal(s),
                BlockKind::Scrollable => MdBlock::Scrollable(s),
            }
        })
        .collect()
}

/// Low-cost "busy" indicator. `ui.spinner()` requests a repaint every frame (~60 fps of full UI
/// layout for as long as a request runs); this animates at 2 Hz instead.
fn activity_label(ui: &mut egui::Ui, text: &str) {
    let dots = (ui.input(|i| i.time) * 2.0) as usize % 4;
    ui.label(format!("{text}{:<3}", ".".repeat(dots)));
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(500));
}

/// Messages whose height can change without their text changing (images still loading, etc.)
/// are always laid out rather than replaced by a cached-height spacer.
fn has_live_media(msg: &ChatMessage) -> bool {
    msg.media
        .iter()
        .any(|m| matches!(m.status, MediaStatus::Downloading | MediaStatus::Downloaded(_)))
}

// 3. UI and Interaction Behavior
/// All panes share one markdown cache: each `CommonMarkCache` deserializes syntect's full
/// syntax + theme sets (several MB), so a per-pane cache multiplied that by the pane count.
pub struct TreeBehavior<'a> {
    pub md_cache: &'a mut CommonMarkCache,
}

impl<'a> TreeBehavior<'a> {
    pub fn new(md_cache: &'a mut CommonMarkCache) -> Self {
        Self { md_cache }
    }
}

impl Behavior<Pane> for TreeBehavior<'_> {
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
                    if let Some(meta) = data.metadata {
                        pane.gemini_metadata = Some(meta);
                    }
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
                        let available_width = (ui.available_width() - 60.0).max(60.0);
                        let mut send_pressed = false;

                        ui.add_enabled_ui(!pane.is_loading, |ui| {
                            let text_res = egui::ScrollArea::vertical()
                                .id_source(ui.id().with("input_scroll"))
                                .max_height(200.0)
                                .max_width(available_width)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut pane.current_input)
                                            .hint_text("Type your prompt... (Enter to send, Shift+Enter for newline)")
                                            .desired_width(f32::INFINITY)
                                            .min_size(egui::vec2(0.0, 60.0)),
                                    )
                                })
                                .inner;

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
                            let metadata_arg = match &pane.gemini_metadata {
                                Some(meta) => serde_json::to_string(meta).unwrap_or_default(),
                                None => String::new(),
                            };

                            tokio::spawn(async move {
                                let mut cmd = helper_command("gemini_auto");
                                cmd.arg(&prompt);
                                cmd.arg(&selected_model);
                                cmd.arg(&metadata_arg);
                                let output = cmd.output().await;

                                match output {
                                    Ok(out) => {
                                        let response = String::from_utf8_lossy(&out.stdout).to_string();
                                        let json_line = response.lines().last().unwrap_or("").to_string();
                                        if let Ok(data) = serde_json::from_str::<PythonResponse>(&json_line) {
                                            let _ = tx.send(PaneEvent::ChatResponse(data));
                                        } else {
                                            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                                            let err_msg = if stderr.trim().is_empty() {
                                                format!("Invalid JSON: '{}'", json_line)
                                            } else {
                                                format!("Python Error:\n{}", stderr.trim())
                                            };
                                            let _ = tx.send(PaneEvent::ChatError(err_msg));
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

                    let available_width = ui.available_width();
                    let local_zoom = pane.local_zoom;
                    pane.render_cache
                        .resize_with(pane.chat_messages.len(), MsgRenderCache::default);

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show_viewport(ui, |ui, viewport| {
                            // `viewport` is in content coordinates, relative to the content's top.
                            let origin_y = ui.max_rect().top();
                            // Lay out a margin beyond the visible area so small scrolls stay exact.
                            let keep = viewport.expand2(egui::vec2(0.0, viewport.height() * 0.5));

                            for (msg_idx, msg) in pane.chat_messages.iter_mut().enumerate() {
                                let cache = &mut pane.render_cache[msg_idx];

                                // Off-screen messages whose layout inputs haven't changed are
                                // replaced by a spacer of their last measured height, so markdown
                                // parsing/layout cost scales with what's visible, not chat length.
                                if let Some(height) = cache.height {
                                    let top = ui.cursor().top() - origin_y;
                                    let layout_unchanged = (cache.width - available_width).abs() < 0.5
                                        && cache.zoom == local_zoom
                                        && !has_live_media(msg);
                                    if layout_unchanged && (top + height < keep.min.y || top > keep.max.y) {
                                        ui.allocate_space(egui::vec2(available_width, height));
                                        continue;
                                    }
                                }

                                let text_len = msg.text.len();
                                if !matches!(&cache.blocks, Some((len, _)) if *len == text_len) {
                                    cache.blocks = Some((text_len, split_markdown_ranges(&msg.text)));
                                }

                                let rendered = ui.vertical(|ui| {
                                let blocks = cache.blocks.as_ref().map(|(_, b)| b.as_slice()).unwrap_or_default();
                                for (block_idx, (kind, range)) in blocks.iter().enumerate() {
                                    let text = &msg.text[range.clone()];
                                    match kind {
                                        BlockKind::Normal => {
                                            ui.horizontal(|ui| {
                                                ui.set_max_width(available_width);
                                                ui.style_mut().wrap = Some(true);
                                                CommonMarkViewer::new(format!("viewer_{:?}_{}_{}_normal", tile_id, msg_idx, block_idx))
                                                    .show(ui, self.md_cache, text);
                                            });
                                        }
                                        BlockKind::Scrollable => {
                                            egui::ScrollArea::horizontal()
                                                .id_source(format!("scroll_{:?}_{}_{}", tile_id, msg_idx, block_idx))
                                                .auto_shrink([false, true])
                                                .show(ui, |ui| {
                                                    CommonMarkViewer::new(format!("viewer_{:?}_{}_{}_scroll", tile_id, msg_idx, block_idx))
                                                        .show(ui, self.md_cache, text);
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
                                                            let mut cmd = helper_command("gemini_download");
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
                                                    activity_label(ui, "Downloading");
                                                },
                                                MediaStatus::Failed(err) => {
                                                    ui.colored_label(egui::Color32::RED, format!("Failed to download: {}", err));
                                                },
                                                MediaStatus::Downloaded(path) => {
                                                    if media.item.media_type.contains("video") {
                                                        if ui.button("🎬 Open Video").clicked() {
                                                            if is_safe_path(path) {
                                                                let _ = open::that(path);
                                                            } else {
                                                                eprintln!("Security alert: attempt to open an unsafe path: {}", path);
                                                            }
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
                                                                if is_safe_path(path) {
                                                                    let _ = open::that(path);
                                                                } else {
                                                                    eprintln!("Security alert: attempt to open an unsafe path: {}", path);
                                                                }
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                            ui.add_space(8.0);
                                        }
                                    });
                                }
                                });

                                cache.height = Some(rendered.response.rect.height());
                                cache.width = available_width;
                                cache.zoom = local_zoom;
                            }

                            if pane.is_loading {
                                ui.add_space(10.0);
                                activity_label(ui, "Thinking (or checking cookies)");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_path() {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());

        let media_dir = format!("{}/.gemini_local/media", home);
        let _ = std::fs::create_dir_all(&media_dir);

        let safe_file = format!("{}/test_file_secure_123.txt", media_dir);
        let _ = std::fs::write(&safe_file, "test");

        let is_safe = is_safe_path(&safe_file);

        let unsafe_file = format!("{}/test_file_unsafe_123.txt", home);
        let _ = std::fs::write(&unsafe_file, "test");
        let is_unsafe = !is_safe_path(&unsafe_file);

        let _ = std::fs::remove_file(safe_file);
        let _ = std::fs::remove_file(unsafe_file);

        assert!(is_safe);
        assert!(is_unsafe);
    }

    #[test]
    fn test_split_markdown_plain_text() {
        let text = "Just some normal text\nwith a newline.";
        let blocks = split_markdown(text);
        assert_eq!(blocks, vec![MdBlock::Normal("Just some normal text\nwith a newline.\n".to_string())]);
    }

    #[test]
    fn test_split_markdown_code_block() {
        let text = "Here is some code:\n```rust\nfn main() {}\n```\nAnd more text.";
        let blocks = split_markdown(text);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], MdBlock::Normal("Here is some code:\n".to_string()));
        assert_eq!(blocks[1], MdBlock::Scrollable("```rust\nfn main() {}\n```\n".to_string()));
        assert_eq!(blocks[2], MdBlock::Normal("And more text.\n".to_string()));
    }

    #[test]
    fn test_split_markdown_table() {
        let text = "A table:\n| Header 1 | Header 2 |\n|---|---|\n| Row 1 | Row 1 |\nEnd table.";
        let blocks = split_markdown(text);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], MdBlock::Normal("A table:\n".to_string()));
        assert_eq!(blocks[1], MdBlock::Scrollable("| Header 1 | Header 2 |\n|---|---|\n| Row 1 | Row 1 |\n".to_string()));
        assert_eq!(blocks[2], MdBlock::Normal("End table.\n".to_string()));
    }

    #[test]
    fn test_split_markdown_unclosed_code_block() {
        let text = "Some text.\n```python\nprint('hello')\nmore text here without closing the block";
        let blocks = split_markdown(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], MdBlock::Normal("Some text.\n".to_string()));
        assert_eq!(blocks[1], MdBlock::Scrollable("```python\nprint('hello')\nmore text here without closing the block\n".to_string()));
    }

    #[test]
    fn test_split_markdown_empty_string() {
        let text = "";
        let blocks = split_markdown(text);
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn test_split_markdown_multiple_blocks() {
        let text = "Text 1\n```\ncode\n```\nText 2\n| a | b |\n| c | d |\nText 3";
        let blocks = split_markdown(text);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0], MdBlock::Normal("Text 1\n".to_string()));
        assert_eq!(blocks[1], MdBlock::Scrollable("```\ncode\n```\n".to_string()));
        assert_eq!(blocks[2], MdBlock::Normal("Text 2\n".to_string()));
        assert_eq!(blocks[3], MdBlock::Scrollable("| a | b |\n| c | d |\n".to_string()));
        assert_eq!(blocks[4], MdBlock::Normal("Text 3\n".to_string()));
    }

    #[test]
    fn test_resolve_executable_fallback() {
        let name = "nonexistent_executable_123456789";
        let (prog, args) = resolve_executable(name);
        assert_eq!(prog, "python3");
        assert_eq!(args, vec![format!("{}.py", name)]);
    }

    #[test]
    fn test_resolve_executable_bundled() {
        let name = "dummy_bundled_executable";
        let mut exe_name = name.to_string();
        if cfg!(windows) {
            exe_name.push_str(".exe");
        }

        // Create a dummy executable next to the current exe
        if let Ok(mut current_exe) = std::env::current_exe() {
            current_exe.pop();
            let bundled_exe = current_exe.join(&exe_name);
            std::fs::write(&bundled_exe, "dummy content").unwrap();

            let (prog, args) = resolve_executable(name);

            // Clean up
            let _ = std::fs::remove_file(&bundled_exe);

            assert_eq!(prog, bundled_exe.to_string_lossy().to_string());
            assert!(args.is_empty());
        }
    }
}

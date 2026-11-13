//! Turn a Gemini reply into something you can scan.
//!
//! Fenced code (` ``` ` and `~~~`) becomes a block with the language name, a copy
//! button, and color for keywords, strings, comments, and numbers. The rest of the
//! reply stays markdown: headings, lists, quotes, tables, and inline code.

use std::borrow::Cow;
use std::ops::Range;

use eframe::egui::{self, Color32, FontId, TextFormat, TextStyle};
use egui::text::LayoutJob;

use crate::theme;

const CODE_BG: Color32 = Color32::from_rgb(17, 19, 24);
const CODE_BORDER: Color32 = Color32::from_rgb(52, 60, 74);
const CODE_TEXT: Color32 = Color32::from_rgb(214, 220, 230);
const CODE_KW: Color32 = Color32::from_rgb(196, 164, 244);
const CODE_STR: Color32 = Color32::from_rgb(152, 208, 158);
const CODE_COM: Color32 = Color32::from_rgb(122, 132, 150);
const CODE_NUM: Color32 = Color32::from_rgb(232, 184, 120);
const CODE_PUNCT: Color32 = Color32::from_rgb(154, 174, 204);

/// Stop coloring enormous pastes on the UI thread. The block still shows.
const HIGHLIGHT_LIMIT: usize = 48_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Error,
    Note,
    Plain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageSlice {
    pub role: Role,
    /// Byte range of the reply with the `**You:**` / `**Gemini:**` label removed.
    pub body: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct Bubble {
    pub label: &'static str,
    pub accent: Color32,
    pub fill: Color32,
    pub border: Color32,
}

pub struct Fence<'a> {
    pub lang: &'a str,
    pub code: &'a str,
}

/// A line that opens or closes a markdown fence. The char is `` ` `` or `~`.
pub fn line_opens_fence(line: &str) -> Option<char> {
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if first != '`' && first != '~' {
        return None;
    }
    if chars.next() == Some(first) && chars.next() == Some(first) {
        Some(first)
    } else {
        None
    }
}

pub fn is_fence_block(text: &str) -> bool {
    text.lines()
        .next()
        .is_some_and(|line| line_opens_fence(line).is_some())
}

pub fn parse_fence(block: &str) -> Option<Fence<'_>> {
    let first_end = block.find('\n').unwrap_or(block.len());
    let first = block[..first_end].trim_end_matches('\r');
    let trimmed = first.trim_start();
    let marker = line_opens_fence(trimmed)?;
    let after_run = trimmed
        .char_indices()
        .find(|(_, ch)| *ch != marker)
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let info = trimmed[after_run..].trim();
    let lang = info
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .find(|token| !token.is_empty())
        .unwrap_or("");

    let code_start = if first_end < block.len() {
        first_end + 1
    } else {
        block.len()
    };
    let region = &block[code_start..];
    let code_end = closing_fence_rel(region, marker)
        .map(|rel| code_start + rel)
        .unwrap_or(block.len());
    Some(Fence {
        lang,
        code: &block[code_start..code_end],
    })
}

fn closing_fence_rel(region: &str, marker: char) -> Option<usize> {
    let trimmed = region.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return None;
    }
    let last_start = trimmed.rfind('\n').map(|index| index + 1).unwrap_or(0);
    if line_opens_fence(&trimmed[last_start..]) == Some(marker) {
        Some(last_start)
    } else {
        None
    }
}

pub fn message_slice(text: &str) -> MessageSlice {
    let start = text
        .char_indices()
        .find(|(_, ch)| *ch != '\n' && *ch != '\r')
        .map(|(index, _)| index)
        .unwrap_or(0);
    let rest = &text[start..];
    if let Some(after) = rest.strip_prefix("**You:**") {
        return MessageSlice {
            role: Role::User,
            body: trim_edge(text, start + (rest.len() - after.len()))..text.len(),
        };
    }
    if let Some(after) = rest.strip_prefix("**Gemini:**") {
        return MessageSlice {
            role: Role::Assistant,
            body: trim_edge(text, start + (rest.len() - after.len()))..text.len(),
        };
    }
    if let Some(after) = rest.strip_prefix("**System Error:**") {
        return MessageSlice {
            role: Role::Error,
            body: trim_edge(text, start + (rest.len() - after.len()))..text.len(),
        };
    }
    let trimmed = rest.trim_start();
    if trimmed.starts_with("*[Request cancelled]*") || trimmed.starts_with("*[Session cleared]*")
    {
        return MessageSlice {
            role: Role::Note,
            body: start..text.len(),
        };
    }
    MessageSlice {
        role: Role::Plain,
        body: start..text.len(),
    }
}

fn trim_edge(text: &str, mut index: usize) -> usize {
    while let Some(ch) = text[index..].chars().next() {
        if ch == ' ' || ch == '\n' || ch == '\r' || ch == '\t' {
            index += ch.len_utf8();
        } else {
            break;
        }
    }
    index
}

pub fn note_label(body: &str) -> &str {
    let trimmed = body.trim();
    if let Some(inner) = trimmed
        .strip_prefix("*[")
        .and_then(|rest| rest.strip_suffix("]*"))
    {
        return inner;
    }
    trimmed
}

pub fn bubble(role: Role) -> Option<Bubble> {
    match role {
        Role::User => Some(Bubble {
            label: "You",
            accent: theme::USER,
            fill: Color32::from_rgba_unmultiplied(120, 164, 240, 28),
            border: Color32::from_rgba_unmultiplied(140, 186, 255, 80),
        }),
        Role::Assistant => Some(Bubble {
            label: "Gemini",
            accent: theme::ASSISTANT,
            fill: Color32::from_rgba_unmultiplied(255, 255, 255, 10),
            border: Color32::from_rgba_unmultiplied(255, 255, 255, 22),
        }),
        Role::Error => Some(Bubble {
            label: "Error",
            accent: theme::DANGER,
            fill: Color32::from_rgba_unmultiplied(240, 96, 96, 28),
            border: Color32::from_rgba_unmultiplied(240, 140, 140, 90),
        }),
        Role::Note | Role::Plain => None,
    }
}

pub fn show_code_block(ui: &mut egui::Ui, id: egui::Id, fence: &str) {
    let (lang, code) = match parse_fence(fence) {
        Some(parsed) => (parsed.lang, display_code(parsed.code)),
        None => ("code", Cow::Borrowed(fence)),
    };
    let label = language_name(lang);
    let job = highlight_job(ui, lang, code.as_ref());

    let now = ui.input(|input| input.time);
    let copied_id = id.with("copied");
    let copied = ui
        .data(|data| data.get_temp::<f64>(copied_id))
        .is_some_and(|at| now - at < 1.2);

    egui::Frame::none()
        .fill(CODE_BG)
        .stroke(egui::Stroke::new(1.0_f32, CODE_BORDER))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(label)
                        .monospace()
                        .size(11.5)
                        .color(theme::ACCENT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let caption = if copied { "Copied" } else { "Copy" };
                    if ui
                        .add_sized(
                            [72.0, 20.0],
                            egui::Button::new(
                                egui::RichText::new(caption).small().color(theme::ACCENT),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(code.as_ref().to_owned());
                        let stamp = ui.input(|input| input.time);
                        ui.data_mut(|data| data.insert_temp(copied_id, stamp));
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(1300));
                    }
                });
            });
            if !code.as_ref().is_empty() {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                egui::ScrollArea::horizontal()
                    .id_source(id.with("scroll"))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(job).selectable(true));
                    });
            }
        });
}

fn language_name(lang: &str) -> String {
    let lang = lang.trim();
    if lang.is_empty() {
        return "code".to_string();
    }
    let mut out = String::new();
    for (index, ch) in lang.chars().enumerate() {
        if index == 24 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn display_code(code: &str) -> Cow<'_, str> {
    let code = code.strip_suffix('\n').unwrap_or(code);
    if code.contains('\r') {
        Cow::Owned(code.replace('\r', ""))
    } else {
        Cow::Borrowed(code)
    }
}

fn highlight_job(ui: &egui::Ui, lang: &str, code: &str) -> LayoutJob {
    let size = TextStyle::Monospace.resolve(ui.style()).size;
    let font = FontId::monospace(size);
    let format = |color: Color32| TextFormat::simple(font.clone(), color);
    let mut job = LayoutJob::default();
    for (kind, range) in tokenize(lang, code) {
        let color = match kind {
            Kind::Text => CODE_TEXT,
            Kind::Keyword => CODE_KW,
            Kind::String => CODE_STR,
            Kind::Comment => CODE_COM,
            Kind::Number => CODE_NUM,
            Kind::Punct => CODE_PUNCT,
        };
        job.append(&code[range], 0.0, format(color));
    }
    job
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Keyword,
    String,
    Comment,
    Number,
    Punct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    Js,
    Go,
    Java,
    Kotlin,
    Swift,
    Cpp,
    Php,
    Lua,
    Ruby,
    Sql,
    Shell,
    Json,
    Css,
    Html,
    Toml,
    Yaml,
    Plain,
    Literal,
}

fn classify(raw: &str) -> Lang {
    let raw = raw.trim();
    if raw.is_empty() {
        return Lang::Plain;
    }
    if any(raw, &["text", "txt", "plain", "plaintext"]) {
        Lang::Literal
    } else if any(raw, &["rs", "rust"]) {
        Lang::Rust
    } else if any(raw, &["py", "python", "py3"]) {
        Lang::Python
    } else if any(raw, &["js", "javascript", "jsx", "mjs", "cjs", "ts", "typescript", "tsx"]) {
        Lang::Js
    } else if any(raw, &["go", "golang"]) {
        Lang::Go
    } else if any(raw, &["java"]) {
        Lang::Java
    } else if any(raw, &["kt", "kotlin"]) {
        Lang::Kotlin
    } else if any(raw, &["swift"]) {
        Lang::Swift
    } else if any(raw, &["c", "h", "cpp", "c++", "cc", "hpp", "cxx", "hxx", "objc"]) {
        Lang::Cpp
    } else if any(raw, &["php"]) {
        Lang::Php
    } else if any(raw, &["lua"]) {
        Lang::Lua
    } else if any(raw, &["rb", "ruby"]) {
        Lang::Ruby
    } else if any(raw, &["sql"]) {
        Lang::Sql
    } else if any(raw, &["sh", "bash", "zsh", "shell", "fish"]) {
        Lang::Shell
    } else if any(raw, &["json"]) {
        Lang::Json
    } else if any(raw, &["css", "scss", "sass"]) {
        Lang::Css
    } else if any(raw, &["html", "xml", "svg"]) {
        Lang::Html
    } else if any(raw, &["toml"]) {
        Lang::Toml
    } else if any(raw, &["yml", "yaml"]) {
        Lang::Yaml
    } else {
        Lang::Plain
    }
}

fn any(raw: &str, names: &[&str]) -> bool {
    names.iter().any(|name| raw.eq_ignore_ascii_case(name))
}

impl Lang {
    fn slash_comments(self) -> bool {
        matches!(
            self,
            Self::Rust
                | Self::Js
                | Self::Go
                | Self::Java
                | Self::Kotlin
                | Self::Swift
                | Self::Cpp
                | Self::Php
                | Self::Css
                | Self::Plain
        )
    }

    fn hash_comments(self) -> bool {
        matches!(
            self,
            Self::Python | Self::Shell | Self::Ruby | Self::Toml | Self::Yaml | Self::Php
        )
    }

    fn dash_comments(self) -> bool {
        matches!(self, Self::Sql | Self::Lua)
    }

    fn block_comments(self) -> bool {
        matches!(
            self,
            Self::Rust
                | Self::Js
                | Self::Go
                | Self::Java
                | Self::Kotlin
                | Self::Swift
                | Self::Cpp
                | Self::Php
                | Self::Css
                | Self::Sql
                | Self::Plain
        )
    }

    fn double_strings(self) -> bool {
        !matches!(self, Self::Literal)
    }

    fn single_strings(self) -> bool {
        matches!(
            self,
            Self::Python
                | Self::Js
                | Self::Shell
                | Self::Ruby
                | Self::Php
                | Self::Toml
                | Self::Yaml
                | Self::Plain
                | Self::Css
                | Self::Html
                | Self::Lua
                | Self::Sql
        )
    }

    fn char_literals(self) -> bool {
        matches!(
            self,
            Self::Rust | Self::Cpp | Self::Java | Self::Go | Self::Kotlin | Self::Swift
        )
    }

    fn backticks(self) -> bool {
        matches!(self, Self::Js | Self::Go | Self::Shell)
    }

    fn triples(self) -> bool {
        matches!(self, Self::Python)
    }
}

fn tokenize(lang: &str, text: &str) -> Vec<(Kind, Range<usize>)> {
    let lang = classify(lang);
    if lang == Lang::Literal || text.len() > HIGHLIGHT_LIMIT {
        return vec![(Kind::Text, 0..text.len())];
    }

    let mut tokens = Vec::new();
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let prev = prev_char(text, index);

        if lang == Lang::Html && rest.starts_with("<!--") {
            let end = scan_until(text, index + 4, "-->");
            push(&mut tokens, Kind::Comment, index..end);
            index = end;
            continue;
        }
        if lang.block_comments() && rest.starts_with("/*") {
            let end = scan_until(text, index + 2, "*/");
            push(&mut tokens, Kind::Comment, index..end);
            index = end;
            continue;
        }
        if lang.slash_comments() && rest.starts_with("//") && prev != Some(':') {
            let end = scan_line(text, index);
            push(&mut tokens, Kind::Comment, index..end);
            index = end;
            continue;
        }
        if lang.hash_comments() && rest.starts_with('#') && hash_comment_ok(lang, prev) {
            let end = scan_line(text, index);
            push(&mut tokens, Kind::Comment, index..end);
            index = end;
            continue;
        }
        if lang.dash_comments() && rest.starts_with("--") {
            let end = scan_line(text, index);
            push(&mut tokens, Kind::Comment, index..end);
            index = end;
            continue;
        }
        if lang == Lang::Cpp && rest.starts_with('#') {
            let after_hash = consume_while(text, index + 1, |ch| ch == ' ' || ch == '\t');
            let word_end = consume_while(text, after_hash, |ch| ch.is_ascii_alphabetic());
            if word_end > after_hash {
                push(&mut tokens, Kind::Keyword, index..word_end);
                index = word_end;
                continue;
            }
        }
        if lang == Lang::Html && rest.starts_with('<') {
            push(&mut tokens, Kind::Punct, index..index + 1);
            index += 1;
            if text[index..].starts_with('/') {
                push(&mut tokens, Kind::Punct, index..index + 1);
                index += 1;
            }
            let tag = consume_while(text, index, |ch| {
                ch.is_ascii_alphanumeric() || ch == '-' || ch == ':'
            });
            if tag > index {
                push(&mut tokens, Kind::Keyword, index..tag);
                index = tag;
            }
            continue;
        }
        if lang.triples() && (rest.starts_with("\"\"\"") || rest.starts_with("'''")) {
            let quote = &rest[..3];
            let end = scan_until(text, index + 3, quote);
            push(&mut tokens, Kind::String, index..end);
            index = end;
            continue;
        }
        if lang.double_strings() && rest.starts_with('"') {
            let end = scan_quoted(text, index, '"', true);
            push(&mut tokens, Kind::String, index..end);
            index = end;
            continue;
        }
        if lang.single_strings() && rest.starts_with('\'') {
            let end = scan_quoted(text, index, '\'', true);
            push(&mut tokens, Kind::String, index..end);
            index = end;
            continue;
        }
        if lang.char_literals() && rest.starts_with('\'') {
            if let Some(end) = char_literal_end(text, index) {
                push(&mut tokens, Kind::String, index..end);
                index = end;
                continue;
            }
        }
        if lang.backticks() && rest.starts_with('`') {
            let escape = lang != Lang::Go;
            let end = scan_quoted(text, index, '`', escape);
            push(&mut tokens, Kind::String, index..end);
            index = end;
            continue;
        }

        let ch = rest.chars().next().unwrap();
        if ch.is_ascii_digit() {
            let end = scan_number(text, index);
            push(&mut tokens, Kind::Number, index..end);
            index = end;
            continue;
        }
        if ch == '_' || ch.is_ascii_alphabetic() {
            let end = consume_while(text, index, |item| {
                item == '_' || item.is_ascii_alphanumeric()
            });
            let word = &text[index..end];
            let kind = if is_keyword(lang, word) {
                Kind::Keyword
            } else {
                Kind::Text
            };
            push(&mut tokens, kind, index..end);
            index = end;
            continue;
        }
        if ch.is_whitespace() {
            let end = consume_while(text, index, |item| item.is_whitespace());
            push(&mut tokens, Kind::Text, index..end);
            index = end;
            continue;
        }
        let end = index + ch.len_utf8();
        push(&mut tokens, Kind::Punct, index..end);
        index = end;
    }
    tokens
}

fn hash_comment_ok(lang: Lang, prev: Option<char>) -> bool {
    if lang == Lang::Shell && prev == Some('$') {
        return false;
    }
    true
}

fn prev_char(text: &str, index: usize) -> Option<char> {
    if index == 0 {
        None
    } else {
        text[..index].chars().next_back()
    }
}

fn consume_while(text: &str, start: usize, mut pred: impl FnMut(char) -> bool) -> usize {
    let mut end = start;
    for (offset, ch) in text[start..].char_indices() {
        if !pred(ch) {
            break;
        }
        end = start + offset + ch.len_utf8();
    }
    end
}

fn scan_line(text: &str, start: usize) -> usize {
    match text[start..].find('\n') {
        Some(rel) => start + rel + 1,
        None => text.len(),
    }
}

fn scan_until(text: &str, start: usize, pat: &str) -> usize {
    match text[start..].find(pat) {
        Some(rel) => start + rel + pat.len(),
        None => text.len(),
    }
}

fn scan_quoted(text: &str, start: usize, quote: char, escape: bool) -> usize {
    let mut escaped = false;
    for (offset, ch) in text[start + quote.len_utf8()..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if escape && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            return start + quote.len_utf8() + offset + ch.len_utf8();
        }
        if ch == '\n' && escape {
            return start + quote.len_utf8() + offset;
        }
    }
    text.len()
}

fn char_literal_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if start >= bytes.len() || bytes[start] != b'\'' {
        return None;
    }
    let mut cursor = start + 1;
    if cursor >= bytes.len() {
        return None;
    }
    if bytes[cursor] == b'\\' {
        cursor += 1;
        let ch = text[cursor..].chars().next()?;
        cursor += ch.len_utf8();
    } else if bytes[cursor] == b'\'' || bytes[cursor] == b'\n' {
        return None;
    } else {
        let ch = text[cursor..].chars().next()?;
        cursor += ch.len_utf8();
    }
    if cursor < bytes.len() && bytes[cursor] == b'\'' {
        Some(cursor + 1)
    } else {
        None
    }
}

fn scan_number(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    if rest.len() >= 2 && (rest.starts_with("0x") || rest.starts_with("0X")) {
        let hex = consume_while(text, start + 2, |ch| ch.is_ascii_hexdigit());
        if hex > start + 2 {
            return hex;
        }
    }
    let mut end = consume_while(text, start, |ch| ch.is_ascii_digit());
    if text[end..].starts_with('.') {
        let fraction = consume_while(text, end + 1, |ch| ch.is_ascii_digit());
        if fraction > end + 1 {
            end = fraction;
        }
    }
    let exp = text[end..].chars().next();
    if matches!(exp, Some('e' | 'E')) {
        let mut cursor = end + 1;
        if text[cursor..].starts_with(['+', '-']) {
            cursor += 1;
        }
        let digits = consume_while(text, cursor, |ch| ch.is_ascii_digit());
        if digits > cursor {
            end = digits;
        }
    }
    end.max(start + 1)
}

fn push(tokens: &mut Vec<(Kind, Range<usize>)>, kind: Kind, range: Range<usize>) {
    if range.start == range.end {
        return;
    }
    if let Some((prev_kind, prev_range)) = tokens.last_mut() {
        if *prev_kind == kind && prev_range.end == range.start {
            prev_range.end = range.end;
            return;
        }
    }
    tokens.push((kind, range));
}

fn is_keyword(lang: Lang, word: &str) -> bool {
    match lang {
        Lang::Rust => {
            matches!(
                word,
                "as" | "async"
                    | "await"
                    | "break"
                    | "const"
                    | "continue"
                    | "crate"
                    | "dyn"
                    | "else"
                    | "enum"
                    | "extern"
                    | "false"
                    | "fn"
                    | "for"
                    | "if"
                    | "impl"
                    | "in"
                    | "let"
                    | "loop"
                    | "match"
                    | "mod"
                    | "move"
                    | "mut"
                    | "pub"
                    | "ref"
                    | "return"
                    | "self"
                    | "Self"
                    | "static"
                    | "struct"
                    | "super"
                    | "trait"
                    | "true"
                    | "type"
                    | "unsafe"
                    | "use"
                    | "where"
                    | "while"
            )
        }
        Lang::Python => {
            matches!(
                word,
                "and" | "as"
                    | "assert"
                    | "async"
                    | "await"
                    | "break"
                    | "class"
                    | "continue"
                    | "def"
                    | "del"
                    | "elif"
                    | "else"
                    | "except"
                    | "False"
                    | "finally"
                    | "for"
                    | "from"
                    | "global"
                    | "if"
                    | "import"
                    | "in"
                    | "is"
                    | "lambda"
                    | "None"
                    | "nonlocal"
                    | "not"
                    | "or"
                    | "pass"
                    | "raise"
                    | "return"
                    | "True"
                    | "try"
                    | "while"
                    | "with"
                    | "yield"
                    | "match"
                    | "case"
            )
        }
        Lang::Js => {
            matches!(
                word,
                "as" | "async"
                    | "await"
                    | "break"
                    | "case"
                    | "catch"
                    | "class"
                    | "const"
                    | "continue"
                    | "debugger"
                    | "default"
                    | "delete"
                    | "do"
                    | "else"
                    | "enum"
                    | "export"
                    | "extends"
                    | "false"
                    | "finally"
                    | "for"
                    | "from"
                    | "function"
                    | "if"
                    | "implements"
                    | "import"
                    | "in"
                    | "instanceof"
                    | "interface"
                    | "let"
                    | "new"
                    | "null"
                    | "of"
                    | "package"
                    | "private"
                    | "protected"
                    | "public"
                    | "return"
                    | "static"
                    | "super"
                    | "switch"
                    | "this"
                    | "throw"
                    | "true"
                    | "try"
                    | "type"
                    | "typeof"
                    | "undefined"
                    | "var"
                    | "void"
                    | "while"
                    | "yield"
            )
        }
        Lang::Go => {
            matches!(
                word,
                "break" | "case"
                    | "chan"
                    | "const"
                    | "continue"
                    | "default"
                    | "defer"
                    | "else"
                    | "fallthrough"
                    | "for"
                    | "func"
                    | "go"
                    | "goto"
                    | "if"
                    | "import"
                    | "interface"
                    | "map"
                    | "package"
                    | "range"
                    | "return"
                    | "select"
                    | "struct"
                    | "switch"
                    | "type"
                    | "var"
                    | "nil"
                    | "true"
                    | "false"
            )
        }
        Lang::Java | Lang::Kotlin => kotlin_or_java(lang, word),
        Lang::Swift => {
            matches!(
                word,
                "as" | "async"
                    | "await"
                    | "break"
                    | "case"
                    | "catch"
                    | "class"
                    | "continue"
                    | "defer"
                    | "else"
                    | "enum"
                    | "extension"
                    | "false"
                    | "for"
                    | "func"
                    | "guard"
                    | "if"
                    | "import"
                    | "in"
                    | "init"
                    | "let"
                    | "nil"
                    | "protocol"
                    | "return"
                    | "self"
                    | "Self"
                    | "static"
                    | "struct"
                    | "switch"
                    | "throw"
                    | "throws"
                    | "true"
                    | "try"
                    | "var"
                    | "where"
                    | "while"
            )
        }
        Lang::Cpp => {
            matches!(
                word,
                "auto" | "bool"
                    | "break"
                    | "case"
                    | "catch"
                    | "char"
                    | "class"
                    | "const"
                    | "continue"
                    | "default"
                    | "delete"
                    | "do"
                    | "double"
                    | "else"
                    | "enum"
                    | "explicit"
                    | "export"
                    | "extern"
                    | "false"
                    | "float"
                    | "for"
                    | "friend"
                    | "goto"
                    | "if"
                    | "inline"
                    | "int"
                    | "long"
                    | "namespace"
                    | "new"
                    | "noexcept"
                    | "nullptr"
                    | "operator"
                    | "private"
                    | "protected"
                    | "public"
                    | "return"
                    | "short"
                    | "signed"
                    | "sizeof"
                    | "static"
                    | "struct"
                    | "switch"
                    | "template"
                    | "this"
                    | "throw"
                    | "true"
                    | "try"
                    | "typedef"
                    | "typename"
                    | "union"
                    | "unsigned"
                    | "using"
                    | "virtual"
                    | "void"
                    | "while"
            )
        }
        Lang::Php => {
            matches!(
                word,
                "as" | "break"
                    | "case"
                    | "catch"
                    | "class"
                    | "const"
                    | "continue"
                    | "default"
                    | "echo"
                    | "else"
                    | "elseif"
                    | "extends"
                    | "false"
                    | "finally"
                    | "for"
                    | "foreach"
                    | "function"
                    | "if"
                    | "implements"
                    | "include"
                    | "namespace"
                    | "new"
                    | "null"
                    | "private"
                    | "protected"
                    | "public"
                    | "require"
                    | "return"
                    | "static"
                    | "switch"
                    | "throw"
                    | "trait"
                    | "true"
                    | "try"
                    | "use"
                    | "while"
            )
        }
        Lang::Lua => {
            matches!(
                word,
                "and" | "break"
                    | "do"
                    | "else"
                    | "elseif"
                    | "end"
                    | "false"
                    | "for"
                    | "function"
                    | "goto"
                    | "if"
                    | "in"
                    | "local"
                    | "nil"
                    | "not"
                    | "or"
                    | "repeat"
                    | "return"
                    | "then"
                    | "true"
                    | "until"
                    | "while"
            )
        }
        Lang::Ruby => {
            matches!(
                word,
                "begin" | "class"
                    | "def"
                    | "do"
                    | "else"
                    | "elsif"
                    | "end"
                    | "ensure"
                    | "false"
                    | "for"
                    | "if"
                    | "in"
                    | "module"
                    | "next"
                    | "nil"
                    | "require"
                    | "rescue"
                    | "return"
                    | "self"
                    | "then"
                    | "true"
                    | "unless"
                    | "until"
                    | "when"
                    | "while"
                    | "yield"
            )
        }
        Lang::Sql => {
            let word = word.to_ascii_lowercase();
            matches!(
                word.as_str(),
                "all" | "and"
                    | "as"
                    | "asc"
                    | "between"
                    | "by"
                    | "case"
                    | "create"
                    | "delete"
                    | "desc"
                    | "distinct"
                    | "drop"
                    | "else"
                    | "end"
                    | "false"
                    | "from"
                    | "group"
                    | "having"
                    | "in"
                    | "inner"
                    | "insert"
                    | "into"
                    | "is"
                    | "join"
                    | "left"
                    | "like"
                    | "limit"
                    | "not"
                    | "null"
                    | "on"
                    | "or"
                    | "order"
                    | "outer"
                    | "right"
                    | "select"
                    | "set"
                    | "table"
                    | "then"
                    | "true"
                    | "union"
                    | "update"
                    | "values"
                    | "where"
                    | "with"
            )
        }
        Lang::Shell => {
            matches!(
                word,
                "case" | "do"
                    | "done"
                    | "echo"
                    | "elif"
                    | "else"
                    | "esac"
                    | "exit"
                    | "export"
                    | "fi"
                    | "for"
                    | "function"
                    | "if"
                    | "in"
                    | "local"
                    | "return"
                    | "then"
                    | "while"
            )
        }
        Lang::Json => matches!(word, "true" | "false" | "null"),
        Lang::Toml => matches!(word, "true" | "false"),
        Lang::Css | Lang::Html | Lang::Yaml | Lang::Plain | Lang::Literal => false,
    }
}

fn kotlin_or_java(lang: Lang, word: &str) -> bool {
    if matches!(
        word,
        "abstract" | "boolean"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "continue"
            | "default"
            | "do"
            | "else"
            | "enum"
            | "extends"
            | "false"
            | "final"
            | "finally"
            | "for"
            | "if"
            | "implements"
            | "import"
            | "instanceof"
            | "interface"
            | "new"
            | "null"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "throws"
            | "true"
            | "try"
            | "void"
            | "while"
    ) {
        return true;
    }
    lang == Lang::Kotlin
        && matches!(
            word,
            "fun" | "val"
                | "var"
                | "when"
                | "object"
                | "data"
                | "sealed"
                | "companion"
                | "suspend"
                | "typealias"
                | "it"
                | "in"
                | "is"
                | "as"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds<'a>(lang: &str, text: &'a str, kind: Kind) -> Vec<&'a str> {
        tokenize(lang, text)
            .into_iter()
            .filter(|(token, _)| *token == kind)
            .map(|(_, range)| &text[range])
            .collect()
    }

    #[test]
    fn message_roles_strip_the_speaker_label() {
        let user = "\n**You:** hi there\n";
        let slice = message_slice(user);
        assert_eq!(slice.role, Role::User);
        assert_eq!(&user[slice.body], "hi there\n");

        let gemini = "\n**Gemini:**\nhello\n```rust\nfn main() {}\n```\n";
        let slice = message_slice(gemini);
        assert_eq!(slice.role, Role::Assistant);
        assert_eq!(
            &gemini[slice.body],
            "hello\n```rust\nfn main() {}\n```\n"
        );

        let err = "\n**System Error:**\nboom\n";
        let slice = message_slice(err);
        assert_eq!(slice.role, Role::Error);
        assert_eq!(&err[slice.body], "boom\n");

        let note = "\n*[Request cancelled]*\n";
        let slice = message_slice(note);
        assert_eq!(slice.role, Role::Note);
        assert_eq!(note_label(&note[slice.body]), "Request cancelled");

        let ready = "### Ready\nType a prompt to begin.";
        assert_eq!(message_slice(ready).role, Role::Plain);
    }

    #[test]
    fn fences_keep_the_language_and_drop_the_markers() {
        let block = "```rust\nfn main() {}\n```\n";
        let fence = parse_fence(block).unwrap();
        assert_eq!(fence.lang, "rust");
        assert_eq!(fence.code, "fn main() {}\n");

        let tilde = "~~~py\nprint(1)\n~~~\n";
        let fence = parse_fence(tilde).unwrap();
        assert_eq!(fence.lang, "py");
        assert_eq!(fence.code, "print(1)\n");

        let open = "```python\nprint('hello')\nmore";
        let fence = parse_fence(open).unwrap();
        assert_eq!(fence.lang, "python");
        assert_eq!(fence.code, "print('hello')\nmore");

        let info = "```rust,ignore\nlet x = 1;\n```\n";
        assert_eq!(parse_fence(info).unwrap().lang, "rust");

        let nested = "~~~\n```\ncode\n~~~\n";
        let fence = parse_fence(nested).unwrap();
        assert_eq!(fence.lang, "");
        assert!(fence.code.contains("code"));
        assert!(fence.code.contains("```"));
    }

    #[test]
    fn highlighter_colors_keywords_strings_and_comments() {
        let rust = "fn main() {\n    let s = \"https://example.com\"; // note\n}\n";
        assert_eq!(kinds("rust", rust, Kind::Keyword), vec!["fn", "let"]);
        assert_eq!(
            kinds("rust", rust, Kind::String),
            vec!["\"https://example.com\""]
        );
        assert_eq!(kinds("rust", rust, Kind::Comment), vec!["// note\n"]);
        assert!(kinds("rust", "see https://example.com", Kind::Comment).is_empty());

        let python = "def hi():\n    return \"fn\"  # done\n";
        assert_eq!(kinds("python", python, Kind::Keyword), vec!["def", "return"]);
        assert_eq!(kinds("python", python, Kind::String), vec!["\"fn\""]);
        assert!(kinds("python", python, Kind::Comment)[0].contains("done"));

        assert_eq!(kinds("json", "{ \"ok\": true }", Kind::Keyword), vec!["true"]);
        assert_eq!(
            kinds("js", "const x = 1; // hi", Kind::Keyword),
            vec!["const"]
        );
        assert_eq!(kinds("sql", "SELECT name FROM t", Kind::Keyword).len(), 2);
        assert!(kinds("bash", "echo $#", Kind::Comment).is_empty());
        assert_eq!(kinds("text", "fn main() {}", Kind::Keyword).len(), 0);
    }
}

//! Gemini web request and response shapes.
//!
//! The page token, the StreamGenerate body, and the length-prefixed frames are
//! the ones the Gemini page uses. Nothing here opens a socket.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

pub const INIT_URL: &str = "https://gemini.google.com/app";
pub const GENERATE_URL: &str = "https://gemini.google.com/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate";
pub const BATCH_URL: &str = "https://gemini.google.com/_/BardChatUi/data/batchexecute";
pub const USER_STATUS_RPC: &str = "otAQ7b";
pub const QUOTA_RPC: &str = "qpEbW";
pub const FLASH_QUOTA_PAYLOAD: &str = "[[[1,11],[2,11],[6,11]]]";
pub const MODEL_HEADER: &str = "x-goog-ext-525001261-jspb";

#[derive(Debug)]
pub struct ProtocolError(pub String);

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Clone, Debug)]
pub struct ModelChoice {
    pub model_id: String,
    pub model_name: String,
    pub capacity: i64,
    pub capacity_field: i64,
    pub model_number: i64,
    pub aliases: Vec<String>,
}

impl ModelChoice {
    /// Header array before the per-request session suffix.
    pub fn header_values(&self) -> Vec<Value> {
        let mut header = vec![
            json!(1),
            Value::Null,
            Value::Null,
            Value::Null,
            json!(self.model_id),
            Value::Null,
            Value::Null,
            json!(0),
            json!([4, 5, 6, 8]),
            Value::Null,
            Value::Null,
        ];
        if self.capacity_field == 13 {
            header.push(Value::Null);
            header.push(json!(self.capacity));
        } else {
            header.push(json!(self.capacity));
        }
        header.push(Value::Null);
        header.push(Value::Null);
        header.push(json!(self.model_number));
        header
    }
}

#[derive(Clone, Debug)]
pub struct ParsedInit {
    pub access_token: String,
    pub build_label: Option<String>,
    pub session_id: Option<String>,
    pub language: String,
}

#[derive(Clone, Debug)]
pub struct ParsedReply {
    pub text: String,
    pub metadata: Vec<Value>,
    pub media: Vec<Value>,
}

static FOLLOW_UP: OnceLock<Regex> = OnceLock::new();

fn follow_up() -> &'static Regex {
    FOLLOW_UP.get_or_init(|| Regex::new(r"(?s)\s*<FollowUp\b[^>]*/>").unwrap())
}

fn pattern(source: &str) -> &'static Regex {
    static ACCESS: OnceLock<Regex> = OnceLock::new();
    static BUILD: OnceLock<Regex> = OnceLock::new();
    static SESSION: OnceLock<Regex> = OnceLock::new();
    static LANGUAGE: OnceLock<Regex> = OnceLock::new();
    static LENGTH: OnceLock<Regex> = OnceLock::new();
    static ARTIFACTS: OnceLock<Regex> = OnceLock::new();
    static CARD: OnceLock<Regex> = OnceLock::new();
    match source {
        "access" => ACCESS.get_or_init(|| Regex::new(r#""SNlM0e":\s*"(.*?)""#).unwrap()),
        "build" => BUILD.get_or_init(|| Regex::new(r#""cfb2h":\s*"(.*?)""#).unwrap()),
        "session" => SESSION.get_or_init(|| Regex::new(r#""FdrFJe":\s*"(.*?)""#).unwrap()),
        "language" => LANGUAGE.get_or_init(|| Regex::new(r#""TuX5cc":\s*"(.*?)""#).unwrap()),
        "length" => LENGTH.get_or_init(|| Regex::new(r"^(\d+)\n").unwrap()),
        "artifacts" => ARTIFACTS.get_or_init(|| {
            Regex::new(r"https?://googleusercontent\.com/(?:\w+/)+\d+\n*").unwrap()
        }),
        _ => CARD.get_or_init(|| {
            Regex::new(r"^https?://googleusercontent\.com/card_content/\d+").unwrap()
        }),
    }
}

fn grab(html: &str, kind: &str) -> Option<String> {
    pattern(kind)
        .captures(html)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}

pub fn parse_init_page(html: &str) -> Result<ParsedInit, ProtocolError> {
    let token = grab(html, "access").ok_or_else(|| {
        ProtocolError(
            "Gemini did not return a session token. The saved cookies may have expired.".into(),
        )
    })?;
    Ok(ParsedInit {
        access_token: token,
        build_label: grab(html, "build"),
        session_id: grab(html, "session"),
        language: grab(html, "language").unwrap_or_else(|| "en".into()),
    })
}

fn utf16_units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// One length-prefixed frame the parser accepts. Used by tests.
pub fn encode_frame(payload: &Value) -> String {
    let body = serde_json::to_string(payload).unwrap_or_default();
    let framed = format!("\n{body}");
    format!("{}\n{body}", utf16_units(&framed))
}

pub struct FrameParser {
    buffer: String,
    expected: Option<usize>,
    payload_start: usize,
    scanned_chars: usize,
    scanned_units: usize,
    prefix_checked: bool,
}

impl FrameParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            expected: None,
            payload_start: 0,
            scanned_chars: 0,
            scanned_units: 0,
            prefix_checked: false,
        }
    }

    pub fn feed(&mut self, content: &str) -> Vec<Value> {
        if !content.is_empty() {
            self.buffer.push_str(content);
        }
        self.strip_prefix();
        let mut frames = Vec::new();
        while (self.expected.is_some() || self.read_length()) && self.expected.is_some() {
            self.scan();
            if self.scanned_units < self.expected.unwrap_or(0) {
                break;
            }
            let end = self.payload_start + self.scanned_chars;
            let chunk: String = self.buffer.chars().skip(self.payload_start).take(self.scanned_chars).collect();
            self.buffer = self.buffer.chars().skip(end).collect();
            self.expected = None;
            self.payload_start = 0;
            self.scanned_chars = 0;
            self.scanned_units = 0;
            if chunk.trim().is_empty() {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<Value>(&chunk) else {
                continue;
            };
            if let Some(items) = parsed.as_array() {
                frames.extend(items.iter().cloned());
            } else {
                frames.push(parsed);
            }
        }
        frames
    }

    fn strip_prefix(&mut self) {
        if self.prefix_checked {
            return;
        }
        let prefix = ")]}'";
        if self.buffer.chars().count() < prefix.chars().count() && prefix.starts_with(&self.buffer) {
            return;
        }
        if let Some(rest) = self.buffer.strip_prefix(prefix) {
            self.buffer = rest.trim_start().to_string();
        }
        self.prefix_checked = true;
    }

    fn read_length(&mut self) -> bool {
        self.buffer = self.buffer.trim_start().to_string();
        let Some(caps) = pattern("length").captures(&self.buffer) else {
            return false;
        };
        let digits = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        self.expected = digits.parse().ok();
        self.payload_start = digits.chars().count();
        self.scanned_chars = 0;
        self.scanned_units = 0;
        self.expected.is_some()
    }

    fn scan(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut index = self.payload_start + self.scanned_chars;
        let expected = self.expected.unwrap_or(0);
        while self.scanned_units < expected && index < chars.len() {
            let units = if chars[index] as u32 > 0xFFFF { 2 } else { 1 };
            if self.scanned_units + units > expected {
                break;
            }
            self.scanned_units += units;
            self.scanned_chars += 1;
            index += 1;
        }
    }
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

enum Step {
    Index(usize),
}

fn nested<'a>(value: &'a Value, path: &[Step]) -> Option<&'a Value> {
    let mut current = value;
    for step in path {
        current = match step {
            Step::Index(index) => {
                let list = current.as_array()?;
                list.get(*index)?
            }
        };
    }
    if current.is_null() {
        None
    } else {
        Some(current)
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Bool(true) => true,
        Value::Number(number) => number.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn emptyish(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn field<'a>(container: &'a Value, index: usize) -> Option<&'a Value> {
    let list = container.as_array()?;
    if list.is_empty() {
        return None;
    }
    let positional = list.get(index);
    let needs_bundle = match positional {
        None => true,
        Some(value) => emptyish(value) || value.is_object(),
    };
    let value = if needs_bundle {
        list.last()
            .and_then(|last| last.as_object())
            .and_then(|bundle| bundle.get(&(index + 1).to_string()))
    } else {
        positional
    };
    match value {
        Some(value) if !emptyish(value) => Some(value),
        _ => None,
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
}

pub fn build_generate_body(
    prompt: &str,
    access_token: &str,
    language: &str,
    metadata: Option<&[Value]>,
    model: Option<&ModelChoice>,
    session_id: &str,
) -> (Vec<(&'static str, String)>, Vec<(String, String)>) {
    let mut chat_meta = vec![
        json!(""),
        json!(""),
        json!(""),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        json!(""),
    ];
    if let Some(metadata) = metadata {
        for (index, value) in metadata.iter().take(10).enumerate() {
            if !value.is_null() {
                chat_meta[index] = value.clone();
            }
        }
    }

    let request_id = uuid_upper();
    let mut inner = vec![Value::Null; 81];
    inner[0] = json!([prompt, 0, Value::Null, Value::Null, Value::Null, Value::Null, 0]);
    inner[1] = json!([language]);
    inner[2] = Value::Array(chat_meta);
    inner[6] = json!([1]);
    inner[7] = json!(1);
    inner[10] = json!(1);
    inner[11] = json!(0);
    inner[17] = json!([[0]]);
    inner[18] = json!(0);
    inner[27] = json!(1);
    inner[30] = json!([4]);
    inner[41] = json!([1]);
    inner[53] = json!(0);
    inner[59] = json!(request_id);
    inner[61] = json!([]);
    inner[68] = json!(1);
    inner[79] = json!(1);
    inner[80] = json!(1);

    let mut headers = vec![
        ("Content-Type".into(), "application/x-www-form-urlencoded;charset=utf-8".into()),
        ("Origin".into(), "https://gemini.google.com".into()),
        ("Referer".into(), "https://gemini.google.com/".into()),
        ("X-Same-Domain".into(), "1".into()),
        ("x-goog-ext-525005358-jspb".into(), format!("[\"{request_id}\",1]")),
    ];
    if let Some(model) = model {
        let mut header = model.header_values();
        if let Some(number) = header.last().and_then(as_i64) {
            inner[79] = json!(number);
        }
        header.push(json!(1));
        header.push(json!(session_id));
        headers.push((
            MODEL_HEADER.into(),
            serde_json::to_string(&header).unwrap_or_else(|_| "[]".into()),
        ));
        headers.push(("x-goog-ext-73010989-jspb".into(), "[0]".into()));
        headers.push(("x-goog-ext-73010990-jspb".into(), "[0,0,0]".into()));
    }

    let inner_json = serde_json::to_string(&inner).unwrap_or_else(|_| "[]".into());
    let freq = serde_json::to_string(&json!([Value::Null, inner_json])).unwrap_or_else(|_| "[]".into());
    let form = vec![("at", access_token.to_string()), ("f.req", freq)];
    (form, headers)
}

pub fn parse_generate_frames(frames: &[Value]) -> Result<ParsedReply, ProtocolError> {
    let mut text = String::new();
    let mut metadata = Vec::new();
    let mut media = Vec::new();
    for part in frames {
        if let Some(code) = nested(part, &[Step::Index(5), Step::Index(2), Step::Index(0), Step::Index(1), Step::Index(0)]) {
            if truthy(code) {
                return Err(ProtocolError(format!("Gemini rejected the request (code {code}).")));
            }
        }
        let Some(inner_raw) = nested(part, &[Step::Index(2)]).and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(inner) = serde_json::from_str::<Value>(inner_raw) else {
            continue;
        };
        if let Some(meta) = nested(&inner, &[Step::Index(1)]).and_then(|v| v.as_array()) {
            if !meta.is_empty() {
                metadata = meta.clone();
            }
        }
        let cid = metadata.first().and_then(|v| v.as_str()).unwrap_or("");
        let rid = metadata.get(1).and_then(|v| v.as_str()).unwrap_or("");
        let Some(candidates) = nested(&inner, &[Step::Index(4)]).and_then(|v| v.as_array()) else {
            continue;
        };
        for candidate in candidates {
            let rcid = nested(candidate, &[Step::Index(0)]).and_then(|v| v.as_str()).unwrap_or("");
            let mut candidate_text = nested(candidate, &[Step::Index(1), Step::Index(0)])
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if pattern("card").is_match(&candidate_text) {
                if let Some(card) = nested(candidate, &[Step::Index(22), Step::Index(0)]).and_then(|v| v.as_str()) {
                    candidate_text = card.to_string();
                }
            }
            if !candidate_text.is_empty() {
                let cleaned = pattern("artifacts").replace_all(&candidate_text, "");
                text = follow_up().replace_all(&cleaned, "").trim().to_string();
            }
            if let Some(rich) = nested(candidate, &[Step::Index(12)]) {
                for image in field(rich, 1).and_then(|v| v.as_array()).into_iter().flatten() {
                    if let Some(url) = nested(image, &[Step::Index(0), Step::Index(0), Step::Index(0)]).and_then(|v| v.as_str()) {
                        let title = nested(image, &[Step::Index(0), Step::Index(4)]).and_then(|v| v.as_str()).unwrap_or("");
                        media.push(json!({
                            "type": "web_image",
                            "meta": {"url": url, "title": title, "alt": title}
                        }));
                    }
                }
                let generated = field(rich, 7)
                    .and_then(|v| nested(v, &[Step::Index(0)]))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for image in generated {
                    if let Some(url) = nested(&image, &[Step::Index(0), Step::Index(3), Step::Index(3)]).and_then(|v| v.as_str()) {
                        media.push(json!({
                            "type": "generated_image",
                            "meta": {
                                "url": url,
                                "title": "",
                                "alt": nested(&image, &[Step::Index(0), Step::Index(3), Step::Index(2)]).and_then(|v| v.as_str()).unwrap_or(""),
                                "cid": cid,
                                "rid": rid,
                                "rcid": rcid,
                                "image_id": nested(&image, &[Step::Index(1), Step::Index(0)]).and_then(|v| v.as_str()).unwrap_or(""),
                            }
                        }));
                    }
                }
            }
        }
    }
    Ok(ParsedReply { text, metadata, media })
}

pub fn reply_complete(frames: &[Value]) -> bool {
    for part in frames {
        let Some(raw) = nested(part, &[Step::Index(2)]).and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(inner) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let Some(candidates) = nested(&inner, &[Step::Index(4)]).and_then(|v| v.as_array()) else {
            continue;
        };
        for candidate in candidates {
            if nested(candidate, &[Step::Index(8), Step::Index(0)]).and_then(as_i64) == Some(2) {
                return true;
            }
        }
    }
    false
}

pub fn unusable_reply(text: &str) -> bool {
    let folded = text.trim().to_lowercase();
    if folded.is_empty() {
        return true;
    }
    const OPENINGS: &[&str] = &[
        "sorry, something went wrong",
        "i seem to be encountering an error",
        "i encountered an error",
        "i'm having a hard time fulfilling your request",
    ];
    OPENINGS.iter().any(|opening| folded.starts_with(opening))
}

pub fn resolve_model(models: &[ModelChoice], name: Option<&str>) -> Result<Option<ModelChoice>, ProtocolError> {
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };
    let target = name.to_lowercase();
    for model in models {
        if target == model.model_name
            || model.aliases.iter().any(|alias| alias == &target)
            || target == model.model_id.to_lowercase()
        {
            return Ok(Some(model.clone()));
        }
    }
    let tail = target.strip_prefix("gemini-").unwrap_or(&target);
    if !tail.is_empty() {
        for model in models {
            if model.model_name.contains(tail) {
                return Ok(Some(model.clone()));
            }
        }
    }
    if !models.is_empty() {
        let known = models
            .iter()
            .map(|model| model.model_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ProtocolError(format!(
            "Unknown model '{name}'. This account offers: {known}"
        )));
    }
    Ok(None)
}

fn capacity(tier_flags: &[Value], capability_flags: &[Value]) -> (i64, i64) {
    let has = |flags: &[Value], needle: i64| flags.iter().any(|flag| as_i64(flag) == Some(needle));
    if has(tier_flags, 21) {
        (1, 13)
    } else if has(tier_flags, 22) {
        (2, 13)
    } else if has(capability_flags, 115) {
        (4, 12)
    } else if has(tier_flags, 16) || has(capability_flags, 106) {
        (3, 12)
    } else if has(tier_flags, 8) || has(capability_flags, 19) {
        (2, 12)
    } else {
        (1, 12)
    }
}

pub fn models_from_status(body: &Value) -> Vec<ModelChoice> {
    let tiers = nested(body, &[Step::Index(16)]).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let caps = nested(body, &[Step::Index(17)]).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let (capacity, capacity_field) = capacity(&tiers, &caps);
    let mut models = Vec::new();
    let Some(items) = nested(body, &[Step::Index(15)]).and_then(|v| v.as_array()) else {
        return models;
    };
    for item in items {
        let Some(model_id) = nested(item, &[Step::Index(0)]).and_then(|v| v.as_str()).filter(|id| !id.is_empty()) else {
            continue;
        };
        let category = nested(item, &[Step::Index(1)])
            .or_else(|| nested(item, &[Step::Index(10)]))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let display = nested(item, &[Step::Index(11)])
            .or_else(|| nested(item, &[Step::Index(19)]))
            .and_then(|v| v.as_str())
            .filter(|text| !text.is_empty())
            .unwrap_or(category);
        let number = nested(item, &[Step::Index(17)])
            .and_then(as_i64)
            .or_else(|| nested(item, &[Step::Index(9)]).and_then(as_i64))
            .unwrap_or(1);
        let slug = category.trim().to_lowercase().replace(' ', "-");
        let model_name = if slug.is_empty() {
            format!("gemini-{model_id}")
        } else {
            format!("gemini-{slug}")
        };
        let mut aliases = vec![
            model_name.clone(),
            model_id.to_lowercase(),
            category.trim().to_lowercase(),
            display.trim().to_lowercase(),
            display.trim().to_lowercase().replace(' ', "-"),
        ];
        aliases.retain(|alias| !alias.is_empty());
        aliases.sort();
        aliases.dedup();
        models.push(ModelChoice {
            model_id: model_id.to_string(),
            model_name,
            capacity,
            capacity_field,
            model_number: number,
            aliases,
        });
    }
    models
}

pub fn quota_lines(body: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(items) = nested(body, &[Step::Index(0)]).and_then(|v| v.as_array()) else {
        return lines;
    };
    for item in items {
        let action = nested(item, &[Step::Index(0), Step::Index(1)]).and_then(as_i64);
        let label = match action {
            Some(4) => "Gemini Pro",
            Some(11) => "Gemini Flash",
            Some(15) => "Gemini Flash Thinking",
            _ => "Gemini",
        };
        let total = nested(item, &[Step::Index(4)]);
        let remaining = nested(item, &[Step::Index(5)]);
        if as_i64(total.unwrap_or(&Value::Null)) == Some(0) && as_i64(remaining.unwrap_or(&Value::Null)) == Some(0) {
            lines.push(format!("{label}: Unlimited"));
        } else if let (Some(total), Some(remaining)) = (total.and_then(as_i64), remaining.and_then(as_i64)) {
            lines.push(format!("{label}: {remaining}/{total} credits remaining"));
        }
    }
    lines
}

pub fn uuid_upper() -> String {
    // Four random u32s is enough for the request id Gemini echoes. The format
    // matches `uuid.uuid4()` so the header shape stays the same.
    let mut bytes = [0u8; 16];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    bytes[..8].copy_from_slice(&(nanos as u64).to_le_bytes());
    bytes[8..].copy_from_slice(&((nanos >> 64) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

pub fn random_reqid() -> i64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    10_000 + (nanos % 90_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT_HTML: &str = r#"
<html><script>
"SNlM0e":"token-123",
"cfb2h":"boq_assistant-bard-web-server_20260101.00_p0",
"FdrFJe":"session-abc",
"TuX5cc":"en"
</script></html>
"#;

    fn candidate_frame(text: &str) -> Value {
        json!([[
            "wrb.fr",
            Value::Null,
            serde_json::to_string(&json!([
                Value::Null,
                ["cid-1", "rid-1"],
                Value::Null,
                Value::Null,
                [["rcid-1", [text]]]
            ])).unwrap()
        ]])
    }

    #[test]
    fn init_page_extracts_session_tokens() {
        let parsed = parse_init_page(INIT_HTML).unwrap();
        assert_eq!(parsed.access_token, "token-123");
        assert_eq!(
            parsed.build_label.as_deref(),
            Some("boq_assistant-bard-web-server_20260101.00_p0")
        );
        assert_eq!(parsed.session_id.as_deref(), Some("session-abc"));
        assert_eq!(parsed.language, "en");
    }

    #[test]
    fn init_page_rejects_a_signed_out_page() {
        assert!(parse_init_page("<html>sign in</html>").is_err());
    }

    #[test]
    fn frame_parser_reads_a_length_prefixed_reply() {
        let raw = format!(")]}}'\n{}", encode_frame(&candidate_frame("pong")));
        let frames = FrameParser::new().feed(&raw);
        let parsed = parse_generate_frames(&frames).unwrap();
        assert_eq!(parsed.text, "pong");
        assert_eq!(parsed.metadata[0], json!("cid-1"));
        assert_eq!(parsed.metadata[1], json!("rid-1"));
        assert!(parsed.media.is_empty());
    }

    #[test]
    fn frame_parser_reassembles_a_split_chunk() {
        let raw = format!(")]}}'\n{}", encode_frame(&candidate_frame("hello there")));
        let mut parser = FrameParser::new();
        let split = raw.chars().count().min(12);
        let (first, second) = raw.split_at(raw.char_indices().nth(split).map(|(i, _)| i).unwrap_or(raw.len()));
        let mut frames = parser.feed(first);
        frames.extend(parser.feed(second));
        let parsed = parse_generate_frames(&frames).unwrap();
        assert_eq!(parsed.text, "hello there");
    }

    #[test]
    fn generate_body_carries_the_prompt_and_prior_chat() {
        let model = ModelChoice {
            model_id: "abc123".into(),
            model_name: "gemini-flash".into(),
            capacity: 1,
            capacity_field: 12,
            model_number: 3,
            aliases: vec![],
        };
        let (form, headers) = build_generate_body(
            "say pong",
            "token-123",
            "en",
            Some(&[json!("cid-1"), json!("rid-1")]),
            Some(&model),
            "SESSION",
        );
        let freq = form.iter().find(|(key, _)| *key == "f.req").unwrap().1.clone();
        let outer: Value = serde_json::from_str(&freq).unwrap();
        let inner: Value = serde_json::from_str(outer[1].as_str().unwrap()).unwrap();
        assert_eq!(form[0].1, "token-123");
        assert_eq!(inner[0][0].as_str(), Some("say pong"));
        assert_eq!(inner[2][0].as_str(), Some("cid-1"));
        assert_eq!(inner[2][1].as_str(), Some("rid-1"));
        assert_eq!(inner.as_array().unwrap().len(), 81);
        assert_eq!(inner[7].as_i64(), Some(1));
        let header = headers.iter().find(|(key, _)| key == MODEL_HEADER).unwrap();
        assert!(header.1.contains("abc123"));
    }

    #[test]
    fn unknown_model_is_rejected_when_the_account_listed_others() {
        let models = vec![ModelChoice {
            model_id: "abc".into(),
            model_name: "gemini-flash".into(),
            capacity: 1,
            capacity_field: 12,
            model_number: 1,
            aliases: vec!["gemini-flash".into()],
        }];
        assert!(resolve_model(&models, Some("gemini-does-not-exist")).is_err());
    }

    #[test]
    fn short_model_name_matches_a_versioned_discovery_name() {
        let models = vec![ModelChoice {
            model_id: "abc".into(),
            model_name: "gemini-3-flash".into(),
            capacity: 1,
            capacity_field: 12,
            model_number: 1,
            aliases: vec!["gemini-3-flash".into()],
        }];
        let chosen = resolve_model(&models, Some("gemini-flash")).unwrap().unwrap();
        assert_eq!(chosen.model_name, "gemini-3-flash");
    }

    #[test]
    fn generated_image_url_is_kept_for_download() {
        let mut image = vec![Value::Null; 2];
        image[0] = json!([Value::Null, Value::Null, Value::Null, [Value::Null, Value::Null, "a cat", "https://example.test/cat.png"]]);
        image[1] = json!(["image-1"]);
        let mut rich = vec![Value::Null; 8];
        rich[7] = json!([[image]]);
        let mut candidate = vec![Value::Null; 13];
        candidate[0] = json!("rcid-9");
        candidate[1] = json!(["a cat"]);
        candidate[12] = Value::Array(rich);
        let inner = json!([
            Value::Null,
            ["cid", "rid"],
            Value::Null,
            Value::Null,
            [candidate]
        ]);
        let frames = vec![json!(["wrb.fr", Value::Null, serde_json::to_string(&inner).unwrap()])];
        let parsed = parse_generate_frames(&frames).unwrap();
        assert_eq!(parsed.text, "a cat");
        assert_eq!(parsed.media[0]["type"], "generated_image");
        assert_eq!(parsed.media[0]["meta"]["url"], "https://example.test/cat.png");
        assert_eq!(parsed.media[0]["meta"]["cid"], "cid");
    }
}

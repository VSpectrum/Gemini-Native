//! Signed-in Gemini session. One client stays warm for a pane.

use crate::browser::{self, BrowserError};
use crate::protocol::{
    self, models_from_status, quota_lines, resolve_model, unusable_reply, FrameParser, ModelChoice,
    ParsedReply, BATCH_URL, FLASH_QUOTA_PAYLOAD, GENERATE_URL, INIT_URL, MODEL_HEADER, QUOTA_RPC,
    USER_STATUS_RPC,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;
use wreq::cookie::Jar;
use wreq_util::Emulation;

#[derive(Clone, Debug)]
pub struct Reply {
    pub text: String,
    pub metadata: Option<Value>,
    pub media: Vec<Value>,
    pub quota: String,
    pub abuse: String,
}

pub struct GeminiSession {
    client: wreq::Client,
    access_token: String,
    build_label: Option<String>,
    page_session: Option<String>,
    language: String,
    models: Vec<ModelChoice>,
    reqid: i64,
    client_session: String,
    quota: String,
    abuse: String,
}

impl GeminiSession {
    pub async fn open(browser_pid: Arc<AtomicU32>) -> Result<Self, String> {
        match Self::open_once(false, Arc::clone(&browser_pid)).await {
            Ok(session) => Ok(session),
            Err(err) => {
                eprintln!("Notice: Initial authentication failed ({err}), refreshing session...");
                Self::open_once(true, browser_pid).await
            }
        }
    }

    async fn open_once(force_refresh: bool, browser_pid: Arc<AtomicU32>) -> Result<Self, String> {
        let (sid, sidts) = tokio::task::spawn_blocking(move || {
            browser::acquire_cookies(force_refresh, &browser_pid).map_err(|err| err.to_string())
        })
        .await
        .map_err(|err| err.to_string())??;

        let jar = Arc::new(Jar::default());
        let cookie_url = "https://google.com/";
        jar.add(
            format!("__Secure-1PSID={sid}; Domain=.google.com; Path=/; Secure"),
            cookie_url,
        );
        jar.add(
            format!("__Secure-1PSIDTS={sidts}; Domain=.google.com; Path=/; Secure"),
            cookie_url,
        );
        let client = wreq::Client::builder()
            .emulation(Emulation::Chrome145)
            .cookie_provider(Arc::clone(&jar))
            .build()
            .map_err(|err| err.to_string())?;

        let mut session = Self {
            client,
            access_token: String::new(),
            build_label: None,
            page_session: None,
            language: "en".into(),
            models: Vec::new(),
            reqid: protocol::random_reqid(),
            client_session: protocol::uuid_upper(),
            quota: "Quota: Unknown".into(),
            abuse: "Abuse Status: Unknown".into(),
        };
        let page = session
            .client
            .get(INIT_URL)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let status = page.status().as_u16();
        let html = page.text().await.map_err(|err| err.to_string())?;
        if status != 200 {
            return Err(format!("Gemini sign-in page returned HTTP {status}."));
        }
        let parsed = protocol::parse_init_page(&html).map_err(|err| err.to_string())?;
        session.access_token = parsed.access_token;
        session.build_label = parsed.build_label;
        session.page_session = parsed.session_id;
        session.language = parsed.language;
        let _ = tokio::time::timeout(Duration::from_secs(15), session.load_models()).await;
        let _ = tokio::time::timeout(Duration::from_secs(15), session.load_quota()).await;
        Ok(session)
    }

    pub async fn generate(
        &mut self,
        prompt: &str,
        model_name: Option<&str>,
        metadata: Option<&Value>,
    ) -> Result<Reply, String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Prompt cannot be empty.".into());
        }
        let model = resolve_model(&self.models, model_name).map_err(|err| err.to_string())?;
        let prior = metadata_list(metadata);
        let first = self.read_generation(prompt, model.as_ref(), prior.as_deref()).await?;
        let parsed = if model.is_some() && unusable_reply(&first.text) {
            self.read_generation(prompt, None, prior.as_deref()).await?
        } else {
            first
        };
        if parsed.text.trim().is_empty() {
            return Err("Gemini returned no reply text.".into());
        }
        let metadata = if parsed.metadata.is_empty() {
            metadata.cloned()
        } else {
            Some(Value::Array(parsed.metadata))
        };
        Ok(Reply {
            text: parsed.text,
            metadata,
            media: parsed.media,
            quota: self.quota.clone(),
            abuse: self.abuse.clone(),
        })
    }

    async fn read_generation(
        &mut self,
        prompt: &str,
        model: Option<&ModelChoice>,
        metadata: Option<&[Value]>,
    ) -> Result<ParsedReply, String> {
        let (form, headers) = protocol::build_generate_body(
            prompt,
            &self.access_token,
            &self.language,
            metadata,
            model,
            &self.client_session,
        );
        let mut params = vec![
            ("hl".to_string(), self.language.clone()),
            ("_reqid".to_string(), self.reqid.to_string()),
            ("rt".to_string(), "c".to_string()),
        ];
        self.reqid += 100_000;
        if let Some(label) = &self.build_label {
            params.push(("bl".to_string(), label.clone()));
        }
        if let Some(session) = &self.page_session {
            params.push(("f.sid".to_string(), session.clone()));
        }
        let form_pairs: Vec<(String, String)> = form
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        let mut request = self
            .client
            .post(GENERATE_URL)
            .query(&params)
            .form(&form_pairs)
            .read_timeout(Duration::from_secs(12));
        for (key, value) in headers {
            request = request.header(key, value);
        }
        let response = request.send().await.map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(180).collect();
            return Err(format!("Generation failed with HTTP {status}: {snippet}"));
        }
        let mut parser = FrameParser::new();
        let mut frames = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                break;
            };
            let piece = String::from_utf8_lossy(&chunk);
            frames.extend(parser.feed(&piece));
            match protocol::parse_generate_frames(&frames) {
                Ok(parsed) if !parsed.text.is_empty() && protocol::reply_complete(&frames) => {
                    return Ok(parsed);
                }
                Ok(_) => {}
                Err(err) => return Err(err.to_string()),
            }
        }
        frames.extend(parser.feed(""));
        protocol::parse_generate_frames(&frames).map_err(|err| err.to_string())
    }

    pub async fn save_url(&self, url: &str, dest_dir: &Path) -> Result<PathBuf, String> {
        let response = self
            .client
            .get(url)
            .header("Referer", "https://gemini.google.com/")
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("Download failed with HTTP {status}."));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response.bytes().await.map_err(|err| err.to_string())?;
        std::fs::create_dir_all(dest_dir).map_err(|err| err.to_string())?;
        let suffix = if content_type.contains("jpeg") {
            ".jpg"
        } else if content_type.contains("webp") {
            ".webp"
        } else if content_type.contains("gif") {
            ".gif"
        } else if content_type.contains("mp4") {
            ".mp4"
        } else {
            ".png"
        };
        let path = dest_dir.join(format!("{}{suffix}", protocol::uuid_upper().replace('-', "").to_lowercase()));
        std::fs::write(&path, &bytes).map_err(|err| err.to_string())?;
        Ok(path)
    }

    async fn batch(&mut self, rpc_id: &str, payload: &str) -> Result<Vec<Value>, String> {
        let mut params = vec![
            ("rpcids".to_string(), rpc_id.to_string()),
            ("hl".to_string(), self.language.clone()),
            ("_reqid".to_string(), self.reqid.to_string()),
            ("rt".to_string(), "c".to_string()),
            ("source-path".to_string(), "/app".to_string()),
        ];
        self.reqid += 100_000;
        if let Some(label) = &self.build_label {
            params.push(("bl".to_string(), label.clone()));
        }
        if let Some(session) = &self.page_session {
            params.push(("f.sid".to_string(), session.clone()));
        }
        let mut header = serde_json::from_str::<Vec<Value>>(
            "[1,null,null,null,null,null,null,null,[4,5,6,8],null,null,null,null,null,null,null]",
        )
        .unwrap_or_default();
        header.push(json!(self.client_session));
        let form = vec![
            ("at".to_string(), self.access_token.clone()),
            (
                "f.req".to_string(),
                serde_json::to_string(&json!([[[rpc_id, payload, Value::Null, "generic"]]])).unwrap_or_default(),
            ),
        ];
        let response = self
            .client
            .post(BATCH_URL)
            .query(&params)
            .header("Content-Type", "application/x-www-form-urlencoded;charset=utf-8")
            .header("Origin", "https://gemini.google.com")
            .header("Referer", "https://gemini.google.com/")
            .header("X-Same-Domain", "1")
            .header(MODEL_HEADER, serde_json::to_string(&header).unwrap_or_else(|_| "[]".into()))
            .header("x-goog-ext-73010989-jspb", "[0]")
            .form(&form)
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|err| err.to_string())?;
        if status != 200 {
            return Err(format!("RPC {rpc_id} failed with HTTP {status}."));
        }
        let mut parser = FrameParser::new();
        let mut frames = parser.feed(&text);
        frames.extend(parser.feed(""));
        let mut bodies = Vec::new();
        for part in frames {
            if part.get(1).and_then(|value| value.as_str()) != Some(rpc_id) {
                continue;
            }
            let Some(raw) = part.get(2).and_then(|value| value.as_str()) else {
                continue;
            };
            if let Ok(body) = serde_json::from_str::<Value>(raw) {
                bodies.push(body);
            }
        }
        Ok(bodies)
    }

    async fn load_models(&mut self) {
        if let Ok(bodies) = self.batch(USER_STATUS_RPC, "[]").await {
            for body in bodies {
                let models = models_from_status(&body);
                if !models.is_empty() {
                    self.models = models;
                    return;
                }
            }
        }
    }

    async fn load_quota(&mut self) {
        if let Ok(bodies) = self.batch(QUOTA_RPC, FLASH_QUOTA_PAYLOAD).await {
            let mut lines = Vec::new();
            for body in bodies {
                lines.extend(quota_lines(&body));
            }
            if !lines.is_empty() {
                self.quota = lines.join(" | ");
            }
        }
    }
}

fn metadata_list(metadata: Option<&Value>) -> Option<Vec<Value>> {
    metadata.and_then(|value| value.as_array().cloned())
}

pub async fn download_saved(url: &str) -> Result<PathBuf, String> {
    let (sid, sidts) = browser::load_saved_cookies()
        .or_else(browser::cookies_from_env)
        .ok_or_else(|| "No saved Gemini session. Send a message to sign in first.".to_string())?;
    let jar = Arc::new(Jar::default());
    jar.add(
        format!("__Secure-1PSID={sid}; Domain=.google.com; Path=/; Secure"),
        "https://google.com/",
    );
    jar.add(
        format!("__Secure-1PSIDTS={sidts}; Domain=.google.com; Path=/; Secure"),
        "https://google.com/",
    );
    let client = wreq::Client::builder()
        .emulation(Emulation::Chrome145)
        .cookie_provider(jar)
        .build()
        .map_err(|err| err.to_string())?;
    let response = client
        .get(url)
        .header("Referer", "https://gemini.google.com/")
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("Download failed with HTTP {status}."));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.bytes().await.map_err(|err| err.to_string())?;
    let dest = browser::media_dir();
    std::fs::create_dir_all(&dest).map_err(|err| err.to_string())?;
    let suffix = if content_type.contains("jpeg") {
        ".jpg"
    } else if content_type.contains("webp") {
        ".webp"
    } else if content_type.contains("gif") {
        ".gif"
    } else if content_type.contains("mp4") {
        ".mp4"
    } else {
        ".png"
    };
    let path = dest.join(format!("{}{suffix}", protocol::uuid_upper().replace('-', "").to_lowercase()));
    std::fs::write(&path, &bytes).map_err(|err| err.to_string())?;
    Ok(path)
}

impl From<BrowserError> for String {
    fn from(err: BrowserError) -> Self {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "calls Gemini with the saved session"]
    fn live_generate_answers() {
        if browser::load_saved_cookies().is_none() {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut session = GeminiSession::open(Arc::new(AtomicU32::new(0)))
                .await
                .expect("session");
            let reply = tokio::time::timeout(
                Duration::from_secs(90),
                session.generate("Reply with exactly the word pong and nothing else.", Some("gemini-flash"), None),
            )
            .await
            .expect("timed out")
            .expect("generate");
            assert!(!reply.text.trim().is_empty(), "{}", reply.text);
        });
    }
}

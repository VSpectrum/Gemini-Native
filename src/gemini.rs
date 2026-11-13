//! Signed-in Gemini session. One client stays warm for a pane.
//!
//! Model discovery and the quota check share that client and run together.
//! The single IO worker polls both sockets; neither call gets its own thread.

use crate::browser::{self, BrowserError};
use crate::protocol::{
    self, models_from_status, quota_lines, resolve_model, unusable_reply, FrameParser, ModelChoice,
    ParsedReply, BATCH_URL, FLASH_QUOTA_PAYLOAD, GENERATE_URL, INIT_URL, MODEL_HEADER, QUOTA_RPC,
    USER_STATUS_RPC,
};
use crate::safety;
use crate::status::{ActivityStep, Reporter};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;
use wreq::cookie::Jar;
use wreq_util::Emulation;

const MAX_MEDIA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Reply {
    pub text: String,
    pub metadata: Option<Value>,
    pub media: Vec<Value>,
    pub quota: String,
    pub abuse: String,
}

#[derive(Clone)]
struct BatchCtx {
    client: wreq::Client,
    language: String,
    build_label: Option<String>,
    page_session: Option<String>,
    access_token: String,
    client_session: String,
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
    pub async fn open(browser_pid: Arc<AtomicU32>, report: &Reporter) -> Result<Self, String> {
        match Self::open_once(false, Arc::clone(&browser_pid), report).await {
            Ok(session) => Ok(session),
            Err(err) => {
                eprintln!("Notice: Initial authentication failed ({err}), refreshing session...");
                Self::open_once(true, browser_pid, report).await
            }
        }
    }

    async fn open_once(force_refresh: bool, browser_pid: Arc<AtomicU32>, report: &Reporter) -> Result<Self, String> {
        let report_for_cookies = report.clone();
        let (sid, sidts) = tokio::task::spawn_blocking(move || {
            browser::acquire_cookies(force_refresh, &browser_pid, &report_for_cookies).map_err(|err| err.to_string())
        })
        .await
        .map_err(|err| err.to_string())??;

        let jar = Arc::new(Jar::default());
        install_session(&jar, &sid, &sidts)?;
        report.report(ActivityStep::OpeningGemini);
        let client = build_client(jar)?;

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
        let _ = tokio::time::timeout(Duration::from_secs(15), session.load_account(report)).await;
        Ok(session)
    }

    pub async fn generate(
        &mut self,
        prompt: &str,
        model_name: Option<&str>,
        metadata: Option<&Value>,
        report: &Reporter,
    ) -> Result<Reply, String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Prompt cannot be empty.".into());
        }
        let model = resolve_model(&self.models, model_name).map_err(|err| err.to_string())?;
        let prior = metadata_list(metadata);
        report.report(ActivityStep::SendingPrompt);
        let first = self.read_generation(prompt, model.as_ref(), prior.as_deref(), report).await?;
        let parsed = if model.is_some() && unusable_reply(&first.text) {
            report.report(ActivityStep::RetryingWithoutModel);
            report.report(ActivityStep::SendingPrompt);
            self.read_generation(prompt, None, prior.as_deref(), report).await?
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
        report: &Reporter,
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
            return Err(format!(
                "Generation failed with HTTP {status}: {}",
                safety::plain_error(&body)
            ));
        }
        report.report(ActivityStep::ReadingReply);
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
        if !safety::media_url_allowed(url) {
            return Err("That file is not hosted on a Gemini media server.".into());
        }
        let response = self
            .client
            .get(url)
            .redirect(media_redirect())
            .header("Referer", "https://gemini.google.com/")
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let bytes = read_download(response).await?;
        let dest_dir = dest_dir.to_path_buf();
        write_media(dest_dir, bytes).await
    }

    fn batch_ctx(&self) -> BatchCtx {
        BatchCtx {
            client: self.client.clone(),
            language: self.language.clone(),
            build_label: self.build_label.clone(),
            page_session: self.page_session.clone(),
            access_token: self.access_token.clone(),
            client_session: self.client_session.clone(),
        }
    }

    async fn load_account(&mut self, report: &Reporter) {
        report.report(ActivityStep::LoadingModels);
        let models_id = self.reqid;
        self.reqid += 100_000;
        let quota_id = self.reqid;
        self.reqid += 100_000;
        let ctx = self.batch_ctx();
        let mut models_fut = std::pin::pin!(batch_once(&ctx, USER_STATUS_RPC, "[]", models_id));
        let mut quota_fut = std::pin::pin!(batch_once(&ctx, QUOTA_RPC, FLASH_QUOTA_PAYLOAD, quota_id));
        let mut models = None;
        let mut quota = None;
        let mut reported_quota = false;
        while models.is_none() || quota.is_none() {
            tokio::select! {
                biased;
                result = &mut models_fut, if models.is_none() => {
                    models = Some(result);
                    if !reported_quota {
                        report.report(ActivityStep::CheckingQuota);
                        reported_quota = true;
                    }
                }
                result = &mut quota_fut, if quota.is_none() => {
                    quota = Some(result);
                }
            }
        }
        if !reported_quota {
            report.report(ActivityStep::CheckingQuota);
        }
        if let Some(Ok(bodies)) = models {
            for body in bodies {
                let found = models_from_status(&body);
                if !found.is_empty() {
                    self.models = found;
                    break;
                }
            }
        }
        if let Some(Ok(bodies)) = quota {
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

async fn batch_once(ctx: &BatchCtx, rpc_id: &str, payload: &str, reqid: i64) -> Result<Vec<Value>, String> {
    let mut params = vec![
        ("rpcids".to_string(), rpc_id.to_string()),
        ("hl".to_string(), ctx.language.clone()),
        ("_reqid".to_string(), reqid.to_string()),
        ("rt".to_string(), "c".to_string()),
        ("source-path".to_string(), "/app".to_string()),
    ];
    if let Some(label) = &ctx.build_label {
        params.push(("bl".to_string(), label.clone()));
    }
    if let Some(session) = &ctx.page_session {
        params.push(("f.sid".to_string(), session.clone()));
    }
    let mut header = serde_json::from_str::<Vec<Value>>(
        "[1,null,null,null,null,null,null,null,[4,5,6,8],null,null,null,null,null,null,null]",
    )
    .unwrap_or_default();
    header.push(json!(ctx.client_session));
    let form = vec![
        ("at".to_string(), ctx.access_token.clone()),
        (
            "f.req".to_string(),
            serde_json::to_string(&json!([[[rpc_id, payload, Value::Null, "generic"]]])).unwrap_or_default(),
        ),
    ];
    let response = ctx
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

fn metadata_list(metadata: Option<&Value>) -> Option<Vec<Value>> {
    metadata.and_then(|value| value.as_array().cloned())
}

fn install_session(jar: &Jar, sid: &str, sidts: &str) -> Result<(), String> {
    if !safety::cookie_value_ok(sid) || !safety::cookie_value_ok(sidts) {
        return Err("saved session cookies were rejected".into());
    }
    let url = "https://google.com/";
    jar.add(
        format!("__Secure-1PSID={sid}; Domain=.google.com; Path=/; Secure"),
        url,
    );
    jar.add(
        format!("__Secure-1PSIDTS={sidts}; Domain=.google.com; Path=/; Secure"),
        url,
    );
    Ok(())
}

fn build_client(jar: Arc<Jar>) -> Result<wreq::Client, String> {
    wreq::Client::builder()
        .emulation(Emulation::Chrome145)
        .cookie_provider(jar)
        .build()
        .map_err(|err| err.to_string())
}

fn media_redirect() -> wreq::redirect::Policy {
    wreq::redirect::Policy::custom(|attempt| {
        if safety::allow_media_hop(attempt.uri.scheme_str(), attempt.uri.host(), attempt.previous.len()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn extension_for(content_type: &str) -> &'static str {
    if content_type.contains("jpeg") {
        ".jpg"
    } else if content_type.contains("webp") {
        ".webp"
    } else if content_type.contains("gif") {
        ".gif"
    } else if content_type.contains("mp4") {
        ".mp4"
    } else {
        ".png"
    }
}

async fn read_download(response: wreq::Response) -> Result<(Vec<u8>, String), String> {
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("Download failed with HTTP {status}."));
    }
    if let Some(len) = response.content_length() {
        if len > MAX_MEDIA_BYTES as u64 {
            return Err("download is larger than 32 MB".into());
        }
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut out = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| err.to_string())?;
        if out.len().saturating_add(chunk.len()) > MAX_MEDIA_BYTES {
            return Err("download is larger than 32 MB".into());
        }
        out.extend_from_slice(&chunk);
    }
    Ok((out, extension_for(&content_type).to_string()))
}

async fn write_media(dest_dir: PathBuf, download: (Vec<u8>, String)) -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(move || {
        let (bytes, suffix) = download;
        std::fs::create_dir_all(&dest_dir).map_err(|err| err.to_string())?;
        browser::restrict_dir(&dest_dir);
        let path = dest_dir.join(format!(
            "{}{suffix}",
            protocol::uuid_upper().replace('-', "").to_lowercase()
        ));
        std::fs::write(&path, &bytes).map_err(|err| err.to_string())?;
        Ok(path)
    })
    .await
    .map_err(|err| err.to_string())?
}

pub async fn download_saved(url: &str) -> Result<PathBuf, String> {
    if !safety::media_url_allowed(url) {
        return Err("That file is not hosted on a Gemini media server.".into());
    }
    let (sid, sidts) = browser::load_saved_cookies()
        .or_else(browser::cookies_from_env)
        .ok_or_else(|| "No saved Gemini session. Send a message to sign in first.".to_string())?;
    let jar = Arc::new(Jar::default());
    install_session(&jar, &sid, &sidts)?;
    let client = build_client(jar)?;
    let response = client
        .get(url)
        .redirect(media_redirect())
        .header("Referer", "https://gemini.google.com/")
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let download = read_download(response).await?;
    write_media(browser::media_dir(), download).await
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
    fn media_extension_follows_the_content_type() {
        assert_eq!(extension_for("image/jpeg"), ".jpg");
        assert_eq!(extension_for("image/webp"), ".webp");
        assert_eq!(extension_for("image/gif"), ".gif");
        assert_eq!(extension_for("video/mp4"), ".mp4");
        assert_eq!(extension_for("image/png"), ".png");
        assert_eq!(extension_for(""), ".png");
    }

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
            let report = Reporter::noop();
            let mut session = GeminiSession::open(Arc::new(AtomicU32::new(0)), &report)
                .await
                .expect("session");
            let reply = tokio::time::timeout(
                Duration::from_secs(90),
                session.generate(
                    "Reply with exactly the word pong and nothing else.",
                    Some("gemini-flash"),
                    None,
                    &report,
                ),
            )
            .await
            .expect("timed out")
            .expect("generate");
            assert!(!reply.text.trim().is_empty(), "{}", reply.text);
        });
    }
}

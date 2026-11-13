//! The two Gemini session cookies, in memory and on disk.
//!
//! Values are checked before they are stored or returned. The file is written
//! mode `0600` on Unix, via a temporary file in a `0700` directory, so a crash
//! cannot leave a world-readable half-written session.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::BrowserError;
use crate::safety;

static EPOCH: AtomicU64 = AtomicU64::new(0);

fn session_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

pub(crate) fn current_epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

fn bump_epoch() {
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

/// A browser cookie. Debug output redacts the value so a log cannot keep it.
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
}

impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cookie")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .field("domain", &self.domain)
            .finish()
    }
}

impl Clone for Cookie {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            value: self.value.clone(),
            domain: self.domain.clone(),
        }
    }
}

pub fn extract_sid_and_sidts(cookies: &[Cookie]) -> (Option<String>, Option<String>) {
    let mut sid = None;
    let mut sidts = None;
    for cookie in cookies {
        let value = cookie.value.trim();
        if !safety::cookie_value_ok(value) || !safety::is_google_domain(&cookie.domain) {
            continue;
        }
        if cookie.name == "__Secure-1PSID" {
            sid = Some(value.to_string());
        } else if cookie.name == "__Secure-1PSIDTS" {
            sidts = Some(value.to_string());
        }
    }
    (sid, sidts)
}

pub(crate) fn closed_error() -> BrowserError {
    BrowserError::Message("Browser window was closed before sign-in completed.".into())
}

pub(crate) fn timeout_error() -> BrowserError {
    BrowserError::Message("Timed out waiting for sign-in cookies (5 minutes exceeded).".into())
}

/// Pure wait loop. Tests inject the clock and the sleep so this never blocks.
pub fn wait_for_google_cookies<F, S, N, C, R>(
    mut fetch: F,
    mut still_open: S,
    timeout: f64,
    interval: f64,
    mut sleep_fn: N,
    mut clock: C,
    mut on_ready: R,
) -> Result<(String, String), BrowserError>
where
    F: FnMut() -> Vec<Cookie>,
    S: FnMut() -> Result<bool, BrowserError>,
    N: FnMut(f64),
    C: FnMut() -> f64,
    R: FnMut(),
{
    let start = clock();
    loop {
        if !still_open()? {
            return Err(closed_error());
        }
        if clock() - start > timeout {
            return Err(timeout_error());
        }
        let (sid, sidts) = extract_sid_and_sidts(&fetch());
        if sid.is_some() && sidts.is_some() {
            on_ready();
            sleep_fn(1.0);
            if !still_open()? {
                return Err(closed_error());
            }
            let (sid2, sidts2) = extract_sid_and_sidts(&fetch());
            return Ok((
                sid2.or(sid).unwrap_or_default(),
                sidts2.or(sidts).unwrap_or_default(),
            ));
        }
        sleep_fn(interval);
    }
}

/// What a sign-in poll needs from a browser. Each call does its own short
/// blocking work and then returns, so the runtime can sleep without pinning
/// a thread.
pub(crate) trait CookieSource {
    async fn read_cookies(&mut self) -> Vec<Cookie>;
    async fn still_open(&mut self) -> Result<bool, BrowserError>;
}

/// Async form of [`wait_for_google_cookies`]. `interval` is the pause between
/// empty reads. `confirm` is the pause after both values first appear, so a
/// rotation can land before we keep them.
pub(crate) async fn poll_for_cookies(
    source: &mut impl CookieSource,
    timeout: Duration,
    interval: Duration,
    confirm: Duration,
    mut on_ready: impl FnMut(),
) -> Result<(String, String), BrowserError> {
    let start = Instant::now();
    loop {
        if !source.still_open().await? {
            return Err(closed_error());
        }
        if start.elapsed() > timeout {
            return Err(timeout_error());
        }
        let (sid, sidts) = extract_sid_and_sidts(&source.read_cookies().await);
        if sid.is_some() && sidts.is_some() {
            on_ready();
            tokio::time::sleep(confirm).await;
            if !source.still_open().await? {
                return Err(closed_error());
            }
            let (sid2, sidts2) = extract_sid_and_sidts(&source.read_cookies().await);
            return Ok((
                sid2.or(sid).unwrap_or_default(),
                sidts2.or(sidts).unwrap_or_default(),
            ));
        }
        tokio::time::sleep(interval).await;
    }
}

pub(crate) fn cookies_from_json(values: Vec<serde_json::Value>) -> Vec<Cookie> {
    values
        .into_iter()
        .filter_map(|value| {
            Some(Cookie {
                name: value.get("name")?.as_str()?.to_string(),
                value: value.get("value")?.as_str()?.to_string(),
                domain: value
                    .get("domain")
                    .and_then(|item| item.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn pair_ok(sid: &str, sidts: &str) -> bool {
    safety::cookie_value_ok(sid) && safety::cookie_value_ok(sidts)
}

/// `Some` when both environment variables are set and at least one is not safe
/// to store. The message does not include the values.
pub(crate) fn rejected_env_cookies() -> Option<String> {
    let sid = std::env::var("GEMINI_PSID").ok();
    let sidts = std::env::var("GEMINI_PSIDTS").ok();
    let (Some(sid), Some(sidts)) = (sid, sidts) else {
        return None;
    };
    if pair_ok(sid.trim(), sidts.trim()) {
        None
    } else {
        Some("GEMINI_PSID or GEMINI_PSIDTS was rejected.".into())
    }
}

pub fn cookies_from_env() -> Option<(String, String)> {
    let sid = std::env::var("GEMINI_PSID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let sidts = std::env::var("GEMINI_PSIDTS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    pair_ok(&sid, &sidts).then_some((sid, sidts))
}

pub fn read_cookie_file(path: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sid = value
        .get("sid")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())?;
    let sidts = value
        .get("sidts")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())?;
    pair_ok(sid, sidts).then_some((sid.to_string(), sidts.to_string()))
}

pub fn load_saved_cookies() -> Option<(String, String)> {
    let _guard = session_lock();
    read_cookie_file(&super::cookie_file())
}

fn write_cookie_file_unlocked(directory: &Path, sid: &str, sidts: &str) -> std::io::Result<()> {
    if !pair_ok(sid, sidts) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cookie value rejected",
        ));
    }
    std::fs::create_dir_all(directory)?;
    super::restrict_dir(directory);
    let body = serde_json::json!({ "sid": sid, "sidts": sidts }).to_string();
    let dest = directory.join("cookies.json");
    let temporary = directory.join(format!(".cookies.{}.tmp", std::process::id()));
    super::write_private(&temporary, body.as_bytes())?;
    if let Err(err) = std::fs::rename(&temporary, &dest) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn write_cookie_file(directory: &Path, sid: &str, sidts: &str) -> std::io::Result<()> {
    let _guard = session_lock();
    write_cookie_file_unlocked(directory, sid, sidts)
}

pub fn save_cookies(sid: &str, sidts: &str) -> std::io::Result<()> {
    write_cookie_file(&super::cache_dir(), sid, sidts)
}

pub(crate) fn save_if_current(sid: &str, sidts: &str, epoch: u64) -> std::io::Result<bool> {
    let _guard = session_lock();
    if EPOCH.load(Ordering::SeqCst) != epoch {
        return Ok(false);
    }
    write_cookie_file_unlocked(&super::cache_dir(), sid, sidts)?;
    Ok(true)
}

pub fn clear_stale_session() {
    let _guard = session_lock();
    bump_epoch();
    let _ = std::fs::remove_file(super::cookie_file());
    let webapi = super::cache_dir().join("gemini_webapi_cache");
    let _ = std::fs::remove_dir_all(&webapi);
    let _ = std::fs::create_dir_all(&webapi);
    super::restrict_dir(&webapi);
    let _ = std::fs::remove_dir_all(super::profile_dir());
}

#[cfg(test)]
mod tests {
    use super::{
        poll_for_cookies, read_cookie_file, wait_for_google_cookies, write_cookie_file, BrowserError,
        Cookie, CookieSource,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn cookie(name: &str, value: &str, domain: &str) -> Cookie {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gemini-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn closed_browser_is_reported() {
        let err = wait_for_google_cookies(
            || Vec::new(),
            || Ok(false),
            5.0,
            1.0,
            |_| {},
            || 0.0,
            || {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("Browser window was closed"));
    }

    #[test]
    fn timeout_uses_injected_clock() {
        let clock = std::cell::Cell::new(0.0);
        let err = wait_for_google_cookies(
            || Vec::new(),
            || Ok(true),
            5.0,
            1.0,
            |seconds| clock.set(clock.get() + seconds),
            || clock.get(),
            || {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("5 minutes"));
        assert!(clock.get() > 5.0);
    }

    #[test]
    fn waits_for_both_cookies_then_rereads() {
        let calls = std::cell::Cell::new(0);
        let ready = std::cell::Cell::new(0);
        let (sid, sidts) = wait_for_google_cookies(
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    vec![cookie("__Secure-1PSID", "sid1", ".google.com")]
                } else {
                    vec![
                        cookie("__Secure-1PSID", "sid1", ".google.com"),
                        cookie("__Secure-1PSIDTS", "sidts1", ".google.com"),
                    ]
                }
            },
            || Ok(true),
            30.0,
            1.0,
            |_| {},
            || 0.0,
            || ready.set(ready.get() + 1),
        )
        .unwrap();
        assert_eq!((sid, sidts), ("sid1".into(), "sidts1".into()));
        assert!(calls.get() >= 3);
        assert_eq!(ready.get(), 1);
    }

    #[test]
    fn immediate_browser_exit_can_fall_through() {
        let err = wait_for_google_cookies(
            || Vec::new(),
            || Err(BrowserError::Launch("browser exited immediately".into())),
            5.0,
            1.0,
            |_| {},
            || 0.0,
            || {},
        )
        .unwrap_err();
        assert!(matches!(err, BrowserError::Launch(_)));
    }

    #[test]
    fn debug_output_redacts_the_value() {
        let text = format!("{:?}", cookie("__Secure-1PSID", "super-secret-value", ".google.com"));
        assert!(text.contains("redacted"));
        assert!(!text.contains("super-secret-value"));
    }

    #[test]
    fn cookie_file_roundtrip_rejects_injection_and_stays_private() {
        let dir = temp_dir("cookies");
        let err = write_cookie_file(&dir, "bad;cookie", "sidts-value").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!err.to_string().contains("bad;cookie"));

        write_cookie_file(&dir, "sid-value", "sidts-value").unwrap();
        let path = dir.join("cookies.json");
        assert_eq!(
            read_cookie_file(&path).as_ref().map(|(sid, _)| sid.as_str()),
            Some("sid-value")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            assert_eq!(dir_mode, 0o700);
        }

        std::fs::write(&path, r#"{"sid":"sid;evil","sidts":"sidts-value"}"#).unwrap();
        assert!(read_cookie_file(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct Script {
        reads: AtomicUsize,
        barrier: Arc<tokio::sync::Barrier>,
    }

    impl CookieSource for Script {
        async fn read_cookies(&mut self) -> Vec<Cookie> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Both polls must be inside this read together. A blocking
                // wait would stall the runtime and the other poll would never arrive.
                self.barrier.wait().await;
                Vec::new()
            } else {
                vec![
                    cookie("__Secure-1PSID", "sid1", ".google.com"),
                    cookie("__Secure-1PSIDTS", "sidts1", ".google.com"),
                ]
            }
        }

        async fn still_open(&mut self) -> Result<bool, BrowserError> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn cookie_polls_overlap_instead_of_taking_turns() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut first = Script {
            reads: AtomicUsize::new(0),
            barrier: Arc::clone(&barrier),
        };
        let mut second = Script {
            reads: AtomicUsize::new(0),
            barrier,
        };
        let joined = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                poll_for_cookies(
                    &mut first,
                    Duration::from_secs(2),
                    Duration::from_millis(10),
                    Duration::from_millis(10),
                    || {},
                ),
                poll_for_cookies(
                    &mut second,
                    Duration::from_secs(2),
                    Duration::from_millis(10),
                    Duration::from_millis(10),
                    || {},
                ),
            )
        })
        .await
        .expect("cookie polls did not overlap");
        assert!(joined.0.is_ok(), "{:?}", joined.0);
        assert!(joined.1.is_ok(), "{:?}", joined.1);
    }

    #[tokio::test]
    async fn cookie_poll_times_out_without_holding_the_caller() {
        struct Empty;
        impl CookieSource for Empty {
            async fn read_cookies(&mut self) -> Vec<Cookie> {
                Vec::new()
            }
            async fn still_open(&mut self) -> Result<bool, BrowserError> {
                Ok(true)
            }
        }
        let err = poll_for_cookies(
            &mut Empty,
            Duration::from_millis(40),
            Duration::from_millis(15),
            Duration::from_millis(15),
            || {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("5 minutes"));
    }

    #[test]
    fn session_epoch_is_one_shared_counter() {
        let before = super::current_epoch();
        super::bump_epoch();
        assert_eq!(super::current_epoch(), before + 1);
    }
}

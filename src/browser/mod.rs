//! Sign-in without a bundled browser.
//!
//! Chrome, Edge, Brave, Chromium, and Firefox get a profile under
//! `~/.gemini_local/browser_profile`. Safari's cookie file is read in place
//! and never written back.
//!
//! The work is split so each piece can be tested on its own:
//! - [`cookies`] checks and stores the two session values
//! - [`login`] starts a browser and polls until those values show up
//! - [`devtools`] talks to a Chromium debugging port on localhost
//! - [`readers`] parses Firefox and Safari cookie stores
//!
//! Polling sleeps on the async runtime. Filesystem, sqlite, and process calls
//! run on the blocking pool, and only for the duration of that call.

mod cookies;
mod devtools;
mod login;
mod readers;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use crate::status::Reporter;

pub use cookies::{
    cookies_from_env, extract_sid_and_sidts, load_saved_cookies, read_cookie_file, save_cookies,
    wait_for_google_cookies, write_cookie_file, Cookie,
};
pub use devtools::{encode_client_frame, pop_ws_frame};
pub use login::{collect_session_cookies, find_browsers};
pub use readers::{read_firefox_cookies, read_safari_cookies, webdriver_call};

#[derive(Debug)]
pub enum BrowserError {
    Launch(String),
    Message(String),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserError::Launch(text) | BrowserError::Message(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for BrowserError {}

pub(crate) fn home_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    } else {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
    }
}

pub fn cache_dir() -> PathBuf {
    home_dir().join(".gemini_local")
}

pub fn cookie_file() -> PathBuf {
    cache_dir().join("cookies.json")
}

pub fn profile_dir() -> PathBuf {
    cache_dir().join("browser_profile")
}

pub fn media_dir() -> PathBuf {
    cache_dir().join("media")
}

pub fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Deletes the on-disk session. Safe to run alongside a sign-in poll: a save
/// that started earlier will not write the file back.
pub async fn clear_saved_session() {
    let _ = tokio::task::spawn_blocking(cookies::clear_stale_session).await;
}

/// Loads a saved session, or opens a browser until both session cookies exist.
///
/// The long wait is a series of async sleeps. Reading the cookie store is a
/// short blocking call, so other panes keep their network tasks moving.
pub async fn acquire_cookies(
    force_refresh: bool,
    browser_pid: Arc<AtomicU32>,
    report: &Reporter,
) -> Result<(String, String), BrowserError> {
    use crate::status::ActivityStep;

    if force_refresh {
        report.report(ActivityStep::ClearingSavedSession);
        clear_saved_session().await;
    } else {
        report.report(ActivityStep::CheckingSavedSession);
        if let Some(saved) = login::blocking(cookies::load_saved_cookies).await {
            return Ok(saved);
        }
        if let Some(message) = cookies::rejected_env_cookies() {
            return Err(BrowserError::Message(message));
        }
        if let Some(pair) = cookies::cookies_from_env() {
            report.report(ActivityStep::UsingEnvironmentSession);
            return Ok(pair);
        }
    }

    eprintln!("Opening a browser to sign in to Gemini...");
    let epoch = cookies::current_epoch();
    let pair =
        login::collect_session_cookies(&profile_dir(), 300, None, &browser_pid, report).await?;
    report.report(ActivityStep::SavingSession);
    let (sid, sidts) = pair.clone();
    let saved = login::blocking(move || cookies::save_if_current(&sid, &sidts, epoch))
        .await
        .map_err(|err| BrowserError::Message(err.to_string()))?;
    if !saved {
        return Err(BrowserError::Message(
            "The saved session was cleared before sign-in finished.".into(),
        ));
    }
    Ok(pair)
}
